//! The session agent's tray for a headless core (session_host's `shell`
//! group): the icon, its menu and balloons, on a thread of its own with a
//! tao event loop, made the first time a core sends a tray and kept for
//! the process's life (Windows' Quick Settings watch needs that thread
//! too). Each connection's tray ([`AgentTray`]) shows the core's model;
//! dropped with its connection, the icon goes. What the user does goes to
//! the connected core.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use session_host::handlers::shell::{ActionSink, TrayBackend, TrayFactory};
use session_host::shell::{BalloonModel, ToCore, TrayItem, TrayModel, UiPage};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::platform::windows::EventLoopBuilderExtWindows;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use win_sys::notify::{Balloon, BalloonIcon, ClickAction, TrayNotifier};

enum ShellEvent {
    /// The tray of connection `n`
    Show(u64, TrayModel),
    /// Connection `n` ended
    Remove(u64),
    Balloon(BalloonModel),
    Menu(String),
    Click,
    QuickSettingsOpened,
}

/// The connection whose core gets the user's actions
static CURRENT: Mutex<Option<(u64, ActionSink)>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

fn current() -> std::sync::MutexGuard<'static, Option<(u64, ActionSink)>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

fn send_action(action: ToCore) {
    let sink = current().as_ref().map(|(_, s)| s.clone());
    if let Some(s) = sink {
        s.send(action);
    }
}

/// The tray thread's loop; None if it could not be made
fn shell_loop() -> Option<EventLoopProxy<ShellEvent>> {
    static PROXY: OnceLock<Option<Mutex<EventLoopProxy<ShellEvent>>>> = OnceLock::new();
    PROXY
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new().name("agent-tray".into()).spawn(move || {
                let mut event_loop = EventLoopBuilder::<ShellEvent>::with_user_event().with_any_thread(true).build();
                let _ = tx.send(event_loop.create_proxy());
                run(&mut event_loop);
            });
            if let Err(e) = spawned {
                tracing::error!("shell: no tray thread: {e}");
                return None;
            }
            rx.recv().ok().map(Mutex::new)
        })
        .as_ref()
        .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

fn build_menu(model: &TrayModel) -> Menu {
    let menu = Menu::new();
    for item in &model.items {
        let _ = match item {
            TrayItem::Item { id, label, enabled } => {
                menu.append(&MenuItem::with_id(id.as_str(), label, *enabled, None))
            }
            TrayItem::Check { id, label, enabled, checked } => {
                menu.append(&CheckMenuItem::with_id(id.as_str(), label, *enabled, *checked, None))
            }
            TrayItem::Separator => menu.append(&PredefinedMenuItem::separator()),
        };
    }
    menu
}

/// A balloon's click opens web pages only
fn click_action(url: Option<String>) -> Option<ClickAction> {
    url.filter(|u| u.starts_with("https://") || u.starts_with("http://")).map(ClickAction::OpenUrl)
}

fn run(event_loop: &mut tao::event_loop::EventLoop<ShellEvent>) {
    let proxy = event_loop.create_proxy();
    let mut tray: Option<TrayIcon> = None;
    // Balloons from the tray icon; on this thread (not Send)
    let mut notifier: Option<TrayNotifier> = None;
    let mut shown: Option<u64> = None;
    event_loop.run_return(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let p = proxy.clone();
                MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                    let _ = p.send_event(ShellEvent::Menu(e.id.0));
                }));
                let p = proxy.clone();
                TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left, button_state: MouseButtonState::Up, ..
                    } = e
                    {
                        let _ = p.send_event(ShellEvent::Click);
                    }
                }));
                let p = proxy.clone();
                win_sys::quick_settings::watch_opened(move || {
                    let _ = p.send_event(ShellEvent::QuickSettingsOpened);
                });
            }
            // No items: the core wants no icon (the user's Exit)
            Event::UserEvent(ShellEvent::Show(n, model)) if model.items.is_empty() => {
                if tray.take().is_some() {
                    tracing::info!("shell: tray icon removed");
                }
                notifier = None;
                shown = Some(n);
            }
            Event::UserEvent(ShellEvent::Show(n, model)) => {
                if let Some(t) = &tray {
                    t.set_menu(Some(Box::new(build_menu(&model))));
                    let _ = t.set_tooltip(Some(&model.tooltip));
                } else {
                    // At the tray's own size: its image from the .ico
                    let size = win_sys::app_icon::tray_icon_size();
                    let mut b = TrayIconBuilder::new()
                        .with_menu(Box::new(build_menu(&model)))
                        .with_menu_on_left_click(false)
                        .with_tooltip(&model.tooltip);
                    if let Ok(i) = tray_icon::Icon::from_resource(101, Some((size, size))) {
                        b = b.with_icon(i);
                    }
                    match b.build() {
                        Ok(t) => {
                            notifier = Some(TrayNotifier::new(t.window_handle() as isize));
                            tray = Some(t);
                            tracing::info!("shell: tray icon shown ({size} px)");
                        }
                        Err(e) => tracing::error!("shell: tray icon: {e}"),
                    }
                }
                shown = Some(n);
            }
            Event::UserEvent(ShellEvent::Remove(n)) => {
                if shown == Some(n) {
                    notifier = None;
                    tray = None;
                    shown = None;
                }
            }
            Event::UserEvent(ShellEvent::Balloon(b)) => match &notifier {
                Some(n) => {
                    let balloon = Balloon {
                        title: b.title,
                        text: b.text,
                        icon: BalloonIcon::Info,
                        on_click: click_action(b.url),
                    };
                    if let Err(e) = n.show(balloon) {
                        tracing::warn!("shell: balloon: {e}");
                    }
                }
                None => tracing::info!("shell: no tray icon for a balloon"),
            },
            Event::UserEvent(ShellEvent::Menu(id)) => send_action(ToCore::Menu { id }),
            Event::UserEvent(ShellEvent::Click) => {
                // The UI the core has started takes the foreground (#140)
                win_sys::notify::allow_foreground_for_click();
                send_action(ToCore::Click);
            }
            Event::UserEvent(ShellEvent::QuickSettingsOpened) if tray.is_some() => {
                send_action(ToCore::QuickSettingsOpened);
            }
            _ => {}
        }
    });
}

/// One connection's tray
struct AgentTray {
    n: u64,
    proxy: Option<EventLoopProxy<ShellEvent>>,
}

impl AgentTray {
    fn new(sink: ActionSink) -> AgentTray {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        *current() = Some((n, sink));
        AgentTray { n, proxy: shell_loop() }
    }

    fn send(&self, e: ShellEvent) {
        if let Some(p) = &self.proxy {
            let _ = p.send_event(e);
        }
    }
}

impl TrayBackend for AgentTray {
    fn show(&mut self, model: &TrayModel) {
        self.send(ShellEvent::Show(self.n, model.clone()));
    }

    fn balloon(&mut self, balloon: &BalloonModel) {
        self.send(ShellEvent::Balloon(balloon.clone()));
    }

    fn launch_ui(&mut self, page: UiPage) {
        session_host::shell::start_ui(page);
    }
}

impl Drop for AgentTray {
    fn drop(&mut self) {
        {
            let mut c = current();
            if c.as_ref().is_some_and(|(n, _)| *n == self.n) {
                *c = None;
            }
        }
        self.send(ShellEvent::Remove(self.n));
    }
}

/// The agent's tray maker ([`session_host::handlers::shell::set_tray_backend`])
pub fn factory() -> TrayFactory {
    Box::new(|sink| Box::new(AgentTray::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balloons_open_web_pages_only() {
        assert!(click_action(Some("https://github.com/x".into())).is_some());
        assert!(click_action(Some("file:///C:/x.exe".into())).is_none());
        assert!(click_action(Some(r"C:\x.exe".into())).is_none());
        assert!(click_action(None).is_none());
    }
}
