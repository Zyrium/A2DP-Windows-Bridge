//! `bluecaster-core.exe --headless`: the resident core without a tray of
//! its own, what a Windows service will host later (an ordinary process
//! for now). The service, the IPC server for the UI and the sampler run as
//! in the resident mode; the session's work runs in the session agent
//! (`--agent --child`, started by the core), and so does the shell: the
//! agent shows the tray, its balloons and starts the UI (session_host's
//! `shell` group), and writes the Run values (`autostart`) and runs the
//! administrator steps (`elevation`) in the user's session.
//!
//! The core decides what the tray shows ([`tray_model`]) and what its
//! actions do ([`tray_action`]); the agent sends what the user chose. A new
//! agent asks for the tray at once ([`ToCore::Ready`]), so a tray lost with
//! its agent comes back with the next one. When the core goes, the agent's
//! tray goes with the connection (a child agent exits too).
//!
//! One core per instance: the same mutex as the resident mode, so a
//! headless core and a resident one of the same instance never run
//! together. A test instance (BLUECASTER_INSTANCE) has its own mutex, pipes
//! and agent. `--test-shell` (tests, debug builds only) starts the agent
//! with a tray that only logs, Run values in memory and no administrator
//! steps.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use bluecaster_core::{Service, ServiceOptions};
use bt_host::HciCapture;
use session_host::shell::{BalloonModel, ToCore, TrayItem, TrayModel, UiPage};

use crate::state::{BalloonRequest, CoreState, TrayView};

/// The argument that runs the core headless
pub const HEADLESS_ARG: &str = "--headless";

/// The option that runs the headless core in service mode:
/// `--service-data <dir>` (bluecaster_core::paths::set_service_data)
pub const SERVICE_DATA_ARG: &str = "--service-data";

/// The folder `--service-data` names (None: not given; Err: given
/// without a folder)
pub(crate) fn service_data_arg(args: &[String]) -> Option<Result<std::path::PathBuf, ()>> {
    let i = args.iter().position(|a| a == SERVICE_DATA_ARG)?;
    Some(match args.get(i + 1).filter(|d| !d.is_empty() && !d.starts_with("--")) {
        Some(d) => Ok(std::path::PathBuf::from(d)),
        None => Err(()),
    })
}

/// Service mode, before anything reads a path: the folder of
/// `--service-data` created for the service's account alone
/// (win_sys::acl), then made the data root. Some(exit code): it cannot run.
pub(crate) fn enter_service_mode(args: &[String]) -> Option<i32> {
    let dir = match service_data_arg(args)? {
        Ok(d) => d,
        Err(()) => {
            win_sys::console::attach_parent();
            eprintln!("{SERVICE_DATA_ARG} needs a folder");
            return Some(2);
        }
    };
    if let Err(e) = win_sys::acl::create_service_dir(&dir) {
        win_sys::console::attach_parent();
        eprintln!("cannot prepare the service data folder: {e}");
        return Some(1);
    }
    bluecaster_core::paths::set_service_data(dir);
    // The pairing keys are the service account's from here on
    win_sys::dpapi::use_service_scope();
    None
}

/// How long the move waits for the agent that just connected
const MIGRATION_AGENT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Service mode: the signed-in user's data moved into the service's folder
/// once, through the session agent (bluecaster_core::service_data), when an
/// agent connected (the first one, normally: a service starts before
/// anyone signs in). True when files were taken: the caller reads them
/// again. Not done (no answer): tried again with the next agent.
fn migrate_user_data(root: &std::path::Path) -> bool {
    use session_host::HostError;
    let outcome = bluecaster_core::service_data::migrate(root, || {
        let hub = session_host::hub().ok_or("no session agent")?;
        hub.wait_agent(MIGRATION_AGENT_WAIT).ok_or("no session agent came")?;
        // The error's kind only: a reply that does not decode could quote it
        session_host::migration::collect().map_err(|e| {
            match e {
                HostError::AgentLost => "the session agent was lost",
                HostError::NoAgent => "no session agent",
                HostError::Timeout => "the session agent did not answer",
                HostError::Remote(_) => "the session agent refused",
                HostError::Protocol(_) => "the session agent's answer was malformed",
            }
            .to_string()
        })
    });
    match outcome {
        Ok(o) if o.already_done => {
            tracing::info!("migration: done before");
            false
        }
        Ok(o) => {
            tracing::info!(
                "migration: {} files taken from the user's folder, {} kept, {} not there, {} refused ({} replaced the service's defaults)",
                o.taken,
                o.kept,
                o.missing,
                o.refused,
                o.replaced
            );
            o.taken > 0
        }
        Err(e) => {
            tracing::warn!("migration: not done ({e}); tried again with the next agent");
            false
        }
    }
}

/// Service mode: the move runs when an agent connects, one at a time; what
/// it took is read again at once
fn migrate_on_agent(state: &Weak<CoreState>, root: &std::path::Path) {
    static RUNNING: Mutex<()> = Mutex::new(());
    let _one = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    // No other change of the settings between the files written and read
    // again: one saved in between would put the old settings back
    let Some(s) = state.upgrade() else { return };
    let _tx = s.service.settings_tx();
    if migrate_user_data(root) {
        s.reload_from_disk();
    }
}

/// Service mode: what a service's stop control does (headless::run
/// returns after the core shut the stack down), and whether one came
/// before the core was ready to take it
static STOP: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);
static STOP_EARLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes a running headless core exit as the tray's Exit does (the
/// service's stop and shutdown controls)
pub(crate) fn request_stop() {
    STOP_EARLY.store(true, std::sync::atomic::Ordering::Release);
    if let Some(tx) = STOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(());
    }
}

/// Called once the headless core runs (the service then reports running)
type Running = Box<dyn FnOnce() + Send>;

/// The tray before the core's first view (the resident mode's)
fn first_view() -> TrayView {
    TrayView {
        streaming: false,
        tooltip: "BlueCaster".into(),
        profiles: Vec::new(),
        labels: Vec::new(),
        active_profile: None,
        language: "en".into(),
    }
}

fn tr(lang: &str, key: &str, default: &str) -> String {
    bluecaster_core::messages::translate(lang, key, default)
}

/// The tray's menu, as the resident mode builds it (resident.rs
/// `build_menu`): open, the profiles by their device, disconnect, exit
pub(crate) fn tray_model(view: &TrayView) -> TrayModel {
    let lang = view.language.as_str();
    let item = |id: &str, label: String, enabled: bool| TrayItem::Item { id: id.into(), label, enabled };
    let mut items = vec![item("open", tr(lang, "tray.open", "Open BlueCaster"), true), TrayItem::Separator];
    for (i, (name, label)) in view.profiles.iter().zip(&view.labels).take(crate::resident::TRAY_PROFILES).enumerate() {
        let checked = view.streaming && view.active_profile.as_deref() == Some(name.as_str());
        items.push(TrayItem::Check { id: format!("profile:{i}"), label: label.clone(), enabled: true, checked });
    }
    if !view.profiles.is_empty() {
        items.push(TrayItem::Separator);
    }
    items.push(item("disconnect", tr(lang, "tray.disconnect", "Disconnect"), view.streaming));
    items.push(TrayItem::Separator);
    items.push(item("exit", tr(lang, "tray.exit", "Exit"), true));
    TrayModel { tooltip: view.tooltip.clone(), items }
}

fn balloon_model(b: BalloonRequest) -> BalloonModel {
    BalloonModel { title: b.title, text: b.text, url: b.url }
}

/// What the tray's actions do in the core
pub(crate) trait TrayCore {
    fn start_profile(&self, name: &str);
    fn stop(&self);
    fn quit(&self);
    fn launch_ui(&self, page: UiPage);
    /// A click on the icon opens the flyout, not the main window
    fn click_opens_flyout(&self) -> bool;
    /// The flyout opens above Quick Settings
    fn flyout_with_quick_settings(&self) -> bool;
    /// A new agent: the tray as it is now, the Run values looked at
    fn agent_ready(&self);
}

/// An action of the agent's tray, as the resident mode's tray does it
/// (`view`: the tray's last view, whose profiles the menu lists)
pub(crate) fn tray_action(core: &dyn TrayCore, view: &TrayView, action: ToCore) {
    match action {
        ToCore::Ready => core.agent_ready(),
        ToCore::Click => core.launch_ui(if core.click_opens_flyout() { UiPage::Flyout } else { UiPage::Main }),
        ToCore::QuickSettingsOpened => {
            if core.flyout_with_quick_settings() {
                core.launch_ui(UiPage::QuickSettings);
            }
        }
        ToCore::Menu { id } => match id.as_str() {
            "open" => core.launch_ui(UiPage::Main),
            "disconnect" => core.stop(),
            "exit" => core.quit(),
            other => match other.strip_prefix("profile:").and_then(|i| i.parse::<usize>().ok()) {
                Some(i) => match view.profiles.get(i) {
                    Some(name) => core.start_profile(name),
                    None => tracing::debug!("tray: profile {i} of an older menu"),
                },
                None => tracing::debug!("tray: unknown menu item"),
            },
        },
    }
}

/// The core's side of the agent's tray: the last view, sent again to each
/// new agent
struct Tray {
    view: Mutex<TrayView>,
    /// Service mode: the user chose Exit ([`close_for_user`]): no icon
    /// until BlueCaster is started again or a new agent comes
    closed: std::sync::atomic::AtomicBool,
}

/// The tray to send: the menu, or none at all (no items: the agent shows
/// no icon) after the user's Exit in service mode
fn shown_model(view: &TrayView, closed: bool) -> TrayModel {
    if closed {
        return TrayModel { tooltip: view.tooltip.clone(), items: Vec::new() };
    }
    tray_model(view)
}

impl Tray {
    fn new() -> Tray {
        Tray { view: Mutex::new(first_view()), closed: std::sync::atomic::AtomicBool::new(false) }
    }

    fn view(&self) -> TrayView {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    fn show(&self, view: TrayView) {
        let model = shown_model(&view, self.closed());
        *self.view.lock().unwrap_or_else(|e| e.into_inner()) = view;
        // No agent now: the next one asks for it
        session_host::shell::show_tray(&model);
    }

    fn resend(&self) {
        session_host::shell::show_tray(&shown_model(&self.view(), self.closed()));
    }

    /// The icon goes (true) or comes back (false); true if that changed
    fn set_closed(&self, closed: bool) -> bool {
        let changed = self.closed.swap(closed, std::sync::atomic::Ordering::AcqRel) != closed;
        if changed {
            self.resend();
        }
        changed
    }
}

/// The headless core's tray, for [`close_for_user`] and [`user_returned`]
static TRAY: OnceLock<Arc<Tray>> = OnceLock::new();

/// Service mode's Exit (the tray's, and app.close): Windows runs the
/// service, so the core stays; what the user sees of BlueCaster goes.
/// Streaming stops, the UI closes (event ui_close) and the tray icon goes
/// until BlueCaster is started again ([`user_returned`]). Keyboards and
/// mice keep working: their input needs the core, not the UI.
pub(crate) fn close_for_user(state: &CoreState) {
    tracing::info!("exit: streaming stops and the UI and the tray close; the service keeps running");
    state.stop();
    state.publish(ipc::Event::UiClose);
    if let Some(t) = TRAY.get() {
        t.set_closed(true);
    }
}

/// BlueCaster's window connected (the user started it again): the tray
/// icon comes back after an Exit
pub(crate) fn user_returned() {
    if TRAY.get().is_some_and(|t| t.set_closed(false)) {
        tracing::info!("tray: shown again");
    }
}

struct Core {
    state: Weak<CoreState>,
    tray: Arc<Tray>,
    replaces_v1: bool,
}

impl Core {
    fn with(&self, f: impl FnOnce(&Arc<CoreState>)) {
        if let Some(s) = self.state.upgrade() {
            f(&s);
        }
    }
}

impl TrayCore for Core {
    fn start_profile(&self, name: &str) {
        self.with(|s| {
            if let Err(e) = s.start_profile(name) {
                tracing::warn!("tray: the profile did not start: {}", e.code);
            }
        });
    }

    fn stop(&self) {
        self.with(|s| s.stop());
    }

    fn quit(&self) {
        // Service mode: Windows starts and stops the service
        if bluecaster_core::paths::service_mode() {
            self.with(|s| close_for_user(s));
        } else {
            self.with(|s| s.quit());
        }
    }

    fn launch_ui(&self, page: UiPage) {
        if !session_host::shell::launch_ui(page) {
            tracing::info!("tray: no agent to start the UI");
        }
    }

    fn click_opens_flyout(&self) -> bool {
        self.state.upgrade().is_some_and(|s| s.tray_click_opens_flyout())
    }

    fn flyout_with_quick_settings(&self) -> bool {
        self.state.upgrade().is_some_and(|s| s.flyout_with_quick_settings())
    }

    fn agent_ready(&self) {
        tracing::info!("tray: a session agent shows it now");
        // A new agent (a new session): an Exit of the last one is over
        if !self.tray.set_closed(false) {
            self.tray.resend();
        }
        // The agent's user's Run values: missing, v1's, or a copy gone
        self.with(|s| s.refresh_autostart(self.replaces_v1));
    }
}

pub fn run(args: &[String]) -> i32 {
    run_with(args, None)
}

/// [`run`]; `running` is called once the core is up (the service reports
/// running then), and [`request_stop`] ends it
pub(crate) fn run_with(args: &[String], running: Option<Running>) -> i32 {
    let test_shell = args.iter().any(|a| a == crate::agent::TEST_SHELL_ARG);
    if test_shell && !crate::agent::TEST_SWITCHES {
        tracing::error!("headless: {} is a test switch of debug builds", crate::agent::TEST_SHELL_ARG);
        return 2;
    }
    if std::env::var_os(crate::agent::TEST_SESSIONS_VAR).is_some() && !crate::agent::TEST_SWITCHES {
        tracing::error!("headless: {} is a test switch of debug builds", crate::agent::TEST_SESSIONS_VAR);
        return 2;
    }
    let _instance = match win_sys::single_instance::SingleInstance::acquire(&ipc::core_mutex_name()) {
        Ok(Some(g)) => g,
        Ok(None) => {
            tracing::info!("the core runs already");
            return 0;
        }
        Err(e) => {
            tracing::error!("single instance check failed: {e}");
            return 1;
        }
    };
    // As the resident mode: a 2.0.x core and v1 stop first
    let old_core_guard = crate::resident::replace_old_core();
    let holds_old_core_mutex = old_core_guard.is_some();
    let replaces_v1 = ipc::instance_suffix().is_empty();
    let v1_guard = if replaces_v1 { crate::resident::replace_v1() } else { None };
    let watches_v1 = replaces_v1 && v1_guard.is_none();
    if holds_old_core_mutex && !watches_v1 {
        bluecaster_core::paths::old_apps_stopped();
    }
    // The session's work, the shell included, in the agent
    let extra: &[&str] = if test_shell { &[crate::agent::TEST_SHELL_ARG] } else { &[] };
    if let Err(e) = crate::agent::use_agent_with(extra) {
        tracing::error!("headless: {e}");
        return 1;
    }
    tracing::info!("headless: the tray is the session agent's");

    let (tx, rx) = mpsc::channel();
    let config_dir = bluecaster_core::paths::config_dir();
    if let Some(note) = bluecaster_core::paths::take_folder_note() {
        tracing::info!("{note}");
    }
    // Service mode: the user's data comes with the first agent (nobody may
    // be signed in yet); what the service writes until then is replaceable
    let service_mode = bluecaster_core::paths::service_mode();
    if service_mode && let Err(e) = bluecaster_core::service_data::note_pending(&config_dir) {
        tracing::warn!("migration: the folder's files were not recorded ({})", e.kind());
    }
    let service = Arc::new(Service::new(
        ServiceOptions { config_dir, hci_tcp: None, capture: HciCapture::new(), afh: None, usb: Default::default() },
        tx,
    ));
    // Keyboards and mice: through the input helper (as SYSTEM on every
    // desktop once this runs as a service); the guard stays for the
    // resident mode and the command line
    if let Ok(exe) = std::env::current_exe() {
        service.enable_hid_guard(exe.clone());
        service.enable_input_helper(exe, None);
    }
    let tray = Arc::new(Tray::new());
    let _ = TRAY.set(tray.clone());
    let (quit_tx, quit_rx) = mpsc::channel::<()>();
    *STOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(quit_tx.clone());
    if STOP_EARLY.load(std::sync::atomic::Ordering::Acquire) {
        let _ = quit_tx.send(());
    }
    let quit_tx = Mutex::new(quit_tx);
    let state = {
        let t = tray.clone();
        CoreState::new(
            service.clone(),
            Box::new(move |v| t.show(v)),
            Box::new(move || {
                let _ = quit_tx.lock().unwrap_or_else(|e| e.into_inner()).send(());
            }),
            Box::new(move |b| {
                // No agent now: the balloon is not shown (as without a tray)
                session_host::shell::show_balloon(&balloon_model(b));
            }),
        )
    };
    // The agent's actions, off the hub's thread (starting a profile, the
    // Run values: they may wait)
    let (actions_tx, actions_rx) = mpsc::channel::<ToCore>();
    let actions_tx = Mutex::new(actions_tx);
    session_host::shell::on_agent_event(move |a| {
        let _ = actions_tx.lock().unwrap_or_else(|e| e.into_inner()).send(a);
    });
    {
        let core = Core { state: Arc::downgrade(&state), tray: tray.clone(), replaces_v1 };
        let tray = tray.clone();
        std::thread::Builder::new()
            .name("tray-actions".into())
            .spawn(move || {
                while let Ok(a) = actions_rx.recv() {
                    tray_action(&core, &tray.view(), a);
                }
            })
            .expect("spawning the tray action thread");
    }
    {
        let state = state.clone();
        std::thread::Builder::new()
            .name("audio-endpoints".into())
            .spawn(move || crate::resident::watch_audio_endpoints(&state))
            .expect("spawning the audio endpoint thread");
    }
    {
        let state = state.clone();
        std::thread::Builder::new()
            .name("service-events".into())
            .spawn(move || {
                while let Ok(e) = rx.recv() {
                    state.on_service_event(e);
                }
            })
            .expect("spawning the service event thread");
    }
    static IPC_FAILED: OnceLock<()> = OnceLock::new();
    {
        let state = state.clone();
        std::thread::Builder::new()
            .name("ipc".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("IPC runtime: {e}");
                        return;
                    }
                };
                rt.block_on(async move {
                    tokio::spawn(crate::resident::sampler(state.clone()));
                    if watches_v1 {
                        tokio::spawn(crate::resident::watch_v1(state.clone()));
                    }
                    if !holds_old_core_mutex {
                        tokio::spawn(crate::resident::hold_old_core_mutex());
                    }
                    let quit = state.clone();
                    // Service mode: one pipe for every user, and the
                    // request rules (crate::rules)
                    let served = if bluecaster_core::paths::service_mode() {
                        crate::server::serve_service(state, ipc::service_pipe_name()).await
                    } else {
                        let (name, old_name) = match win_sys::pipe::current_user_sid() {
                            Ok(sid) => (ipc::pipe_name(&sid), ipc::old_pipe_name(&sid)),
                            Err(e) => {
                                tracing::error!("cannot read the user SID: {e}");
                                return;
                            }
                        };
                        crate::server::serve(state, name, old_name).await
                    };
                    if let Err(e) = served {
                        tracing::error!("IPC server: {e}");
                        let _ = IPC_FAILED.set(());
                        quit.quit();
                    }
                });
            })
            .expect("spawning the IPC thread");
    }
    // Not in service mode: the window looks for updates of the app, and the
    // set-up tool updates the service. The check also saves the settings
    // (its date) from the copy in memory, which at this point may still be
    // the service's own defaults: saved a moment after the user's settings
    // moved in, they would replace them.
    if !service_mode {
        state.check_update_if_due();
    }
    if let Some(hub) = session_host::hub() {
        // Service mode: the user's data moves in with the first agent
        if service_mode {
            let weak = Arc::downgrade(&state);
            let root = state.config_dir();
            let (w, r) = (weak.clone(), root.clone());
            hub.on_agent_connected(move |_| migrate_on_agent(&w, &r));
            // An agent that connected while the core was starting
            if hub.agent().is_some() {
                let spawned =
                    std::thread::Builder::new().name("migration".into()).spawn(move || migrate_on_agent(&weak, &root));
                if let Err(e) = spawned {
                    tracing::warn!("migration: cannot start it: {e}");
                }
            }
        }
        // Another user at the console (fast user switching), or nobody: a
        // stream fed from the old user's session stops. Its audio is that
        // user's (their apps, their output device) and its capture goes
        // with their agent; the next user starts their own.
        let weak = Arc::downgrade(&state);
        hub.on_session_change(move |_, _| {
            if let Some(s) = weak.upgrade()
                && s.service.is_running()
            {
                tracing::info!("headless: the console's user changed, the stream stops");
                s.stop();
            }
        });
    }
    if let Some(running) = running {
        running();
    }

    // Until app.quit, the tray's Exit or a failed IPC server (service mode:
    // a failed IPC server only; Windows stops the service), or the
    // service's stop (request_stop)
    let _ = quit_rx.recv();
    tracing::info!("exiting");
    // Streaming stops and the adapter is released before exit
    service.shutdown();
    // The agent's connection closes: its tray goes, the child agent exits
    if let Some(hub) = session_host::hub() {
        hub.tell_agent_to_exit();
        hub.shutdown();
    }
    drop((v1_guard, old_core_guard));
    if IPC_FAILED.get().is_some() { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn view(streaming: bool) -> TrayView {
        TrayView {
            streaming,
            tooltip: "BlueCaster\nSony WH".into(),
            profiles: vec!["p1".into(), "p2".into()],
            labels: vec!["Sony WH".into(), "Buds".into()],
            active_profile: Some("p2".into()),
            language: "en".into(),
        }
    }

    #[test]
    fn the_model_is_the_resident_menu() {
        let m = tray_model(&view(true));
        assert_eq!(m.tooltip, "BlueCaster\nSony WH");
        assert_eq!(m.item_ids().collect::<Vec<_>>(), ["open", "profile:0", "profile:1", "disconnect", "exit"]);
        let checked: Vec<bool> = m
            .items
            .iter()
            .filter_map(|i| if let TrayItem::Check { checked, .. } = i { Some(*checked) } else { None })
            .collect();
        assert_eq!(checked, [false, true]);
        assert!(m.items.contains(&TrayItem::Item {
            id: "disconnect".into(),
            label: "Disconnect".into(),
            enabled: true
        }));
        // Not streaming: nothing checked, nothing to disconnect
        let idle = tray_model(&view(false));
        assert!(idle.items.iter().all(|i| !matches!(i, TrayItem::Check { checked: true, .. })));
        assert!(idle.items.contains(&TrayItem::Item {
            id: "disconnect".into(),
            label: "Disconnect".into(),
            enabled: false
        }));
        // No profiles: one separator fewer
        let first = tray_model(&first_view());
        assert_eq!(first.item_ids().collect::<Vec<_>>(), ["open", "disconnect", "exit"]);
        assert_eq!(first.items.len(), 5);
        // At most TRAY_PROFILES profiles
        let many = TrayView {
            profiles: (0..15).map(|i| format!("p{i}")).collect(),
            labels: (0..15).map(|i| format!("d{i}")).collect(),
            ..view(false)
        };
        assert_eq!(tray_model(&many).item_ids().filter(|i| i.starts_with("profile:")).count(), 10);
    }

    /// After the user's Exit in service mode the agent gets a tray without
    /// items: no icon
    #[test]
    fn a_closed_tray_has_no_items() {
        assert!(shown_model(&view(true), true).items.is_empty());
        assert_eq!(shown_model(&view(true), false), tray_model(&view(true)));
        let t = Tray::new();
        assert!(!t.closed());
        assert!(t.set_closed(true) && t.closed());
        assert!(!t.set_closed(true), "already closed");
        assert!(t.set_closed(false) && !t.closed());
    }

    #[derive(Default)]
    struct Fake {
        flyout: bool,
        quick_settings: bool,
        did: RefCell<Vec<String>>,
    }

    impl TrayCore for Fake {
        fn start_profile(&self, name: &str) {
            self.did.borrow_mut().push(format!("start {name}"));
        }
        fn stop(&self) {
            self.did.borrow_mut().push("stop".into());
        }
        fn quit(&self) {
            self.did.borrow_mut().push("quit".into());
        }
        fn launch_ui(&self, page: UiPage) {
            self.did.borrow_mut().push(format!("ui {page:?}"));
        }
        fn click_opens_flyout(&self) -> bool {
            self.flyout
        }
        fn flyout_with_quick_settings(&self) -> bool {
            self.quick_settings
        }
        fn agent_ready(&self) {
            self.did.borrow_mut().push("ready".into());
        }
    }

    #[test]
    fn actions_do_what_the_resident_tray_does() {
        let v = view(true);
        let menu = |id: &str| ToCore::Menu { id: id.into() };
        let fake = Fake::default();
        for a in [
            ToCore::Ready,
            menu("open"),
            menu("profile:1"),
            menu("profile:7"),
            menu("profile:x"),
            menu("nonsense"),
            menu("disconnect"),
            ToCore::Click,
            ToCore::QuickSettingsOpened,
            menu("exit"),
        ] {
            tray_action(&fake, &v, a);
        }
        assert_eq!(*fake.did.borrow(), ["ready", "ui Main", "start p2", "stop", "ui Main", "quit"]);
        // Settings: the click opens the flyout, or the flyout opens with
        // Quick Settings (then the click opens the window)
        let flyout = Fake { flyout: true, ..Fake::default() };
        tray_action(&flyout, &v, ToCore::Click);
        let qs = Fake { quick_settings: true, ..Fake::default() };
        tray_action(&qs, &v, ToCore::QuickSettingsOpened);
        assert_eq!(*flyout.did.borrow(), ["ui Flyout"]);
        assert_eq!(*qs.did.borrow(), ["ui QuickSettings"]);
    }
}
