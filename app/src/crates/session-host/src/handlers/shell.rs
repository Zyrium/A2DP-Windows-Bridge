//! Group `shell` in the agent: the tray of a core without its own.
//!
//! The agent's program registers how a tray is made ([`set_tray_backend`]):
//! the real one (a tray icon on a thread of its own) or, for tests, one
//! that only logs. The handler makes the backend when the core sends its
//! first model and drops it with the connection, which removes the tray.
//! What the user does comes back through the [`ActionSink`] as events.

use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::host::shell::{BalloonModel, GROUP, ToAgent, ToCore, TrayModel, UiPage};
use crate::wire::{ErrorCode, WireError, decode};
use crate::{AgentContext, Handler};

/// A tray in the agent. Dropping it removes the icon.
pub trait TrayBackend: Send {
    /// Shows the icon with `model`, or updates it
    fn show(&mut self, model: &TrayModel);
    fn balloon(&mut self, balloon: &BalloonModel);
    fn launch_ui(&mut self, page: UiPage);
}

/// Where a tray sends what the user did: the connected core
#[derive(Clone)]
pub struct ActionSink(Arc<dyn Fn(ToCore) -> bool + Send + Sync>);

impl ActionSink {
    pub fn new(f: impl Fn(ToCore) -> bool + Send + Sync + 'static) -> ActionSink {
        ActionSink(Arc::new(f))
    }

    /// False when the connection is closed
    pub fn send(&self, action: ToCore) -> bool {
        (self.0)(action)
    }
}

impl std::fmt::Debug for ActionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionSink")
    }
}

/// Makes a tray for one connection
pub type TrayFactory = Box<dyn Fn(ActionSink) -> Box<dyn TrayBackend> + Send + Sync>;

static FACTORY: OnceLock<TrayFactory> = OnceLock::new();

/// How this agent makes its tray; once, before [`crate::run_agent`].
/// Without one the agent answers a core's model with a log line only.
pub fn set_tray_backend(factory: TrayFactory) -> bool {
    FACTORY.set(factory).is_ok()
}

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    let sink = ActionSink::new(move |a| ctx.event(GROUP, crate::wire::to_body(&a)));
    let factory = FACTORY.get().map(|f| f as &(dyn Fn(ActionSink) -> Box<dyn TrayBackend> + Send + Sync));
    Box::new(Shell::new(sink, factory))
}

/// Made with the connection: asks the core for its model at once
struct Shell<'f> {
    sink: ActionSink,
    factory: Option<&'f (dyn Fn(ActionSink) -> Box<dyn TrayBackend> + Send + Sync)>,
    tray: Option<Box<dyn TrayBackend>>,
    /// A model came while no tray can be made (logged once)
    no_tray_logged: bool,
}

impl<'f> Shell<'f> {
    fn new(
        sink: ActionSink,
        factory: Option<&'f (dyn Fn(ActionSink) -> Box<dyn TrayBackend> + Send + Sync)>,
    ) -> Shell<'f> {
        sink.send(ToCore::Ready);
        Shell { sink, factory, tray: None, no_tray_logged: false }
    }

    fn tray(&mut self) -> Option<&mut Box<dyn TrayBackend>> {
        if self.tray.is_none() {
            match self.factory {
                Some(f) => {
                    self.tray = Some(f(self.sink.clone()));
                    tracing::info!("shell: tray shown for the core");
                }
                None if !self.no_tray_logged => {
                    self.no_tray_logged = true;
                    tracing::info!("shell: the core sent a tray, this agent has none");
                }
                None => {}
            }
        }
        self.tray.as_mut()
    }

    fn handle(&mut self, event: ToAgent) {
        match event {
            ToAgent::Tray { model } => {
                if let Some(t) = self.tray() {
                    t.show(&model);
                }
            }
            ToAgent::Balloon { balloon } => match self.tray() {
                Some(t) => t.balloon(&balloon),
                None => tracing::debug!("shell: no tray for a balloon"),
            },
            ToAgent::LaunchUi { page } => match self.tray() {
                Some(t) => t.launch_ui(page),
                None => tracing::debug!("shell: no shell to start the UI"),
            },
        }
    }
}

impl Drop for Shell<'_> {
    fn drop(&mut self) {
        if self.tray.take().is_some() {
            tracing::info!("shell: the core went, tray removed");
        }
    }
}

impl Handler for Shell<'static> {
    fn request(&mut self, _body: Value) -> Result<Value, WireError> {
        Err(WireError::new(ErrorCode::BadRequest, "the shell takes events only"))
    }

    fn event(&mut self, body: Value) {
        match decode::<ToAgent>(body) {
            Ok(e) => self.handle(e),
            Err(e) => tracing::debug!("shell: an event of the core that does not decode ({:?})", e.code),
        }
    }
}

/// A tray that shows nothing: logs what it got (counts only) and starts
/// no UI. For tests of the agent (`--agent --test-shell`).
#[derive(Debug)]
pub struct LogOnlyTray {
    models: u64,
}

impl LogOnlyTray {
    pub fn factory() -> TrayFactory {
        Box::new(|_sink| Box::new(LogOnlyTray { models: 0 }))
    }
}

impl TrayBackend for LogOnlyTray {
    fn show(&mut self, model: &TrayModel) {
        self.models += 1;
        tracing::info!("shell (test): tray model {} with {} menu item(s)", self.models, model.item_ids().count());
    }

    fn balloon(&mut self, _balloon: &BalloonModel) {
        tracing::info!("shell (test): balloon");
    }

    fn launch_ui(&mut self, page: UiPage) {
        tracing::info!("shell (test): the UI not started ({page:?})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::shell::TrayItem;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Seen {
        calls: Vec<String>,
        sent: Vec<ToCore>,
    }

    struct FakeTray(Arc<Mutex<Seen>>, ActionSink);

    impl TrayBackend for FakeTray {
        fn show(&mut self, model: &TrayModel) {
            self.0.lock().unwrap().calls.push(format!("show {}", model.tooltip));
        }
        fn balloon(&mut self, b: &BalloonModel) {
            self.0.lock().unwrap().calls.push(format!("balloon {}", b.text));
        }
        fn launch_ui(&mut self, page: UiPage) {
            self.0.lock().unwrap().calls.push(format!("ui {page:?}"));
            // What the user does next goes to the core
            self.1.send(ToCore::Menu { id: "exit".into() });
        }
    }

    impl Drop for FakeTray {
        fn drop(&mut self) {
            self.0.lock().unwrap().calls.push("removed".into());
        }
    }

    fn event(e: ToAgent) -> Value {
        serde_json::to_value(e).unwrap()
    }

    #[test]
    fn the_tray_comes_with_the_model_and_goes_with_the_connection() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let s = seen.clone();
        let sink = ActionSink::new(move |a| {
            s.lock().unwrap().sent.push(a);
            true
        });
        let s = seen.clone();
        let factory = move |sink: ActionSink| -> Box<dyn TrayBackend> { Box::new(FakeTray(s.clone(), sink)) };
        let mut shell = Shell::new(sink, Some(&factory));
        // A new connection asks for the model; nothing shown before it
        assert_eq!(seen.lock().unwrap().sent, [ToCore::Ready]);
        assert!(seen.lock().unwrap().calls.is_empty());
        let model = |tooltip: &str| TrayModel {
            tooltip: tooltip.into(),
            items: vec![TrayItem::Item { id: "open".into(), label: "Open".into(), enabled: true }],
        };
        shell.handle(ToAgent::Tray { model: model("a") });
        shell.handle(ToAgent::Tray { model: model("b") });
        shell
            .handle(ToAgent::Balloon { balloon: BalloonModel { title: String::new(), text: "low".into(), url: None } });
        shell.handle(ToAgent::LaunchUi { page: UiPage::Flyout });
        drop(shell);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.calls, ["show a", "show b", "balloon low", "ui Flyout", "removed"]);
        assert_eq!(seen.sent, [ToCore::Ready, ToCore::Menu { id: "exit".into() }]);
    }

    #[test]
    fn without_a_backend_nothing_is_shown() {
        let sink = ActionSink::new(|_| true);
        let mut shell = Shell::new(sink, None);
        shell.handle(ToAgent::Tray { model: TrayModel::default() });
        shell.handle(ToAgent::LaunchUi { page: UiPage::Main });
        assert!(shell.tray.is_none());
    }

    #[test]
    fn the_handler_takes_events_only() {
        let mut h = new(AgentContext::detached());
        assert_eq!(h.request(serde_json::json!({"op": "tray"})).unwrap_err().code, ErrorCode::BadRequest);
        h.event(event(ToAgent::LaunchUi { page: UiPage::Main }));
        h.event(serde_json::json!({"op": "nonsense"}));
    }
}
