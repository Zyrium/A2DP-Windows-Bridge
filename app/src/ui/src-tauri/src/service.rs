//! The service add-on from the app's side: which core the window connects
//! to ([`route`]), the state the Developer screen shows, and running the
//! per-user install's `bluecaster-core.exe --service-setup <action>` as
//! administrator (Windows' own prompt) to add, update or remove the
//! service.
//!
//! The service runs its own copy of the core from Program Files. This
//! window stays in the user's folder, so the two can differ after the
//! app's own updater ran: a different version is never connected to, and
//! no core of the user's own starts beside it, until the service is
//! updated (one administrator prompt).

use std::sync::atomic::{AtomicBool, Ordering};

use ipc::service_state::ServiceState;
use serde::Serialize;

/// This window's version; the service must be exactly this one
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the window's connection goes, from the service's state
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Route {
    /// No service: the user's own core, as without the add-on
    NotAdded,
    /// The service of this version runs: its pipe
    Running { version: String },
    /// The service of this version is added but does not run
    Stopped { version: String },
    /// An older version (or one that cannot be read, or a registration
    /// that runs another program): nothing connects until it is updated
    NeedsUpdate { version: String },
    /// A newer version than this window: this app needs the update. The
    /// service is not set up again from here, which would take it back to
    /// this older version for every user of the PC.
    AppOlder { version: String },
}

/// The pure decision over the service's state and this window's version
/// (the service's own rule, ipc::service_state::version_reject). An update
/// repairs a wrong registration and a version that cannot be read too.
pub fn route(state: &ServiceState, app_version: &str) -> Route {
    use ipc::service_state::{SERVICE_NEEDS_UPDATE, version_reject};
    match state {
        ServiceState::NotInstalled => Route::NotAdded,
        ServiceState::Installed { version, running, trusted_path } => {
            let version = version.clone();
            if !*trusted_path || version.is_empty() {
                return Route::NeedsUpdate { version };
            }
            match version_reject(&version, app_version) {
                Some(SERVICE_NEEDS_UPDATE) => return Route::NeedsUpdate { version },
                Some(_) => return Route::AppOlder { version },
                None => {}
            }
            if *running { Route::Running { version } } else { Route::Stopped { version } }
        }
    }
}

/// The service's route on this PC now (read-only queries). A test
/// instance (BLUECASTER_INSTANCE) never uses the service: it sees none.
pub fn current() -> Route {
    if !ipc::instance_suffix().is_empty() {
        return Route::NotAdded;
    }
    route(&ipc::service_state::service_state(), APP_VERSION)
}

/// What the set-up tool is asked to do
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Remove { data: bool },
}

impl Action {
    /// The frontend's words: "install", "update", "remove"; the data goes
    /// only with a removal
    pub fn parse(action: &str, remove_data: bool) -> Result<Action, String> {
        match (action, remove_data) {
            ("install", false) => Ok(Action::Install),
            ("update", false) => Ok(Action::Update),
            ("remove", data) => Ok(Action::Remove { data }),
            ("install" | "update", true) => Err("only a removal deletes the data".into()),
            _ => Err(format!("unknown service action {action:?}")),
        }
    }

    /// Whether the action fits the service's state: adding only when there
    /// is none, updating only an older or stopped one (never a newer one
    /// back to this version), removing any
    pub fn allowed(self, route: &Route) -> Result<(), String> {
        let ok = match self {
            Action::Install => *route == Route::NotAdded,
            Action::Update => {
                matches!(route, Route::NeedsUpdate { .. } | Route::Stopped { .. } | Route::Running { .. })
            }
            Action::Remove { .. } => *route != Route::NotAdded,
        };
        if ok { Ok(()) } else { Err(format!("not with the service's state {route:?}")) }
    }

    /// The set-up tool's arguments
    pub fn args(self) -> Vec<&'static str> {
        match self {
            Action::Install => vec!["--service-setup", "install"],
            Action::Update => vec!["--service-setup", "update"],
            Action::Remove { data: false } => vec!["--service-setup", "remove"],
            Action::Remove { data: true } => vec!["--service-setup", "remove", "--remove-data"],
        }
    }
}

/// How a set-up ended
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Done,
    /// The user answered No on Windows' prompt
    Declined,
    /// The tool's exit code (5: it did not get administrator rights, 2:
    /// bad arguments; others: the set-up tool's own)
    Failed {
        code: u32,
    },
    /// The tool could not be started or waited for
    NotStarted {
        detail: String,
    },
}

/// The set-up tool's exit code -> the outcome
pub fn outcome_of(code: u32) -> Outcome {
    if code == 0 { Outcome::Done } else { Outcome::Failed { code } }
}

/// A set-up is running (one at a time)
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears [`RUNNING`] however the set-up ends
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Runs the set-up tool as administrator and waits for it. Blocking: call
/// it from a thread of its own.
///
/// Written, never executed: this starts the UAC prompt and, once approved,
/// changes the PC for every user (the service and its folder).
pub fn run_setup(action: Action) -> Outcome {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Outcome::NotStarted { detail: "a set-up is running already".into() };
    }
    let _guard = RunningGuard;
    if !ipc::instance_suffix().is_empty() {
        return Outcome::NotStarted { detail: "not from a test instance".into() };
    }
    // The per-user install's core: it copies itself into the service folder
    let core = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(ipc::service_state::CORE_EXE),
        Err(e) => return Outcome::NotStarted { detail: e.to_string() },
    };
    if !core.is_file() {
        return Outcome::NotStarted { detail: "bluecaster-core.exe not found".into() };
    }
    tracing::info!("service set-up: {} started", action.args()[1]);
    let outcome = match win_sys::elevation::run_elevated(&core, &action.args()) {
        Ok(code) => outcome_of(code),
        Err(win_sys::elevation::ElevationError::Cancelled) => Outcome::Declined,
        Err(e) => Outcome::NotStarted { detail: e.to_string() },
    };
    match &outcome {
        Outcome::Failed { code } => tracing::warn!("service set-up: exit code {code}"),
        other => tracing::info!("service set-up: {other:?}"),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(version: &str, running: bool, trusted_path: bool) -> ServiceState {
        ServiceState::Installed { version: version.into(), running, trusted_path }
    }

    #[test]
    fn without_the_service_the_users_own_core_runs() {
        assert_eq!(route(&ServiceState::NotInstalled, "3.0.3"), Route::NotAdded);
    }

    #[test]
    fn the_service_of_this_version_is_used_when_it_runs() {
        assert_eq!(route(&installed("3.0.3", true, true), "3.0.3"), Route::Running { version: "3.0.3".into() });
        assert_eq!(route(&installed("3.0.3", false, true), "3.0.3"), Route::Stopped { version: "3.0.3".into() });
    }

    #[test]
    fn any_other_version_needs_an_update() {
        let needs = |v: &str| Route::NeedsUpdate { version: v.into() };
        // The app's updater ran: the service is older
        assert_eq!(route(&installed("3.0.2", true, true), "3.0.3"), needs("3.0.2"));
        // Stopped, or an earlier pre-release
        assert_eq!(route(&installed("3.0.2", false, true), "3.0.3"), needs("3.0.2"));
        assert_eq!(route(&installed("3.0.0-beta.3", true, true), "3.0.0-beta.4"), needs("3.0.0-beta.3"));
        // The alphas come after their beta
        assert_eq!(route(&installed("3.0.0-beta.4", true, true), "3.0.0-beta.4.alpha.1"), needs("3.0.0-beta.4"));
        // Not readable
        assert_eq!(route(&installed("", true, true), "3.0.3"), needs(""));
    }

    #[test]
    fn a_newer_service_asks_for_the_apps_update() {
        let older = |v: &str| Route::AppOlder { version: v.into() };
        assert_eq!(route(&installed("3.0.4", true, true), "3.0.3"), older("3.0.4"));
        assert_eq!(route(&installed("3.0.0", false, true), "3.0.0-beta.4"), older("3.0.0"));
        // Not a version: never set up again from here
        assert_eq!(route(&installed("x", true, true), "3.0.3"), older("x"));
    }

    #[test]
    fn a_registration_of_another_program_needs_an_update() {
        assert_eq!(route(&installed("3.0.3", true, false), "3.0.3"), Route::NeedsUpdate { version: "3.0.3".into() });
    }

    #[test]
    fn actions_and_their_arguments() {
        assert_eq!(Action::parse("install", false).unwrap().args(), ["--service-setup", "install"]);
        assert_eq!(Action::parse("update", false).unwrap().args(), ["--service-setup", "update"]);
        assert_eq!(Action::parse("remove", false).unwrap().args(), ["--service-setup", "remove"]);
        assert_eq!(Action::parse("remove", true).unwrap().args(), ["--service-setup", "remove", "--remove-data"]);
        assert!(Action::parse("install", true).is_err());
        assert!(Action::parse("update", true).is_err());
        assert!(Action::parse("start", false).is_err());
        assert!(Action::parse("--install-service", false).is_err());
    }

    #[test]
    fn actions_fit_the_state() {
        let v = || "3.0.3".to_string();
        let none = Route::NotAdded;
        let older = Route::NeedsUpdate { version: v() };
        let newer = Route::AppOlder { version: v() };
        let stopped = Route::Stopped { version: v() };
        assert!(Action::Install.allowed(&none).is_ok());
        assert!(Action::Install.allowed(&older).is_err());
        assert!(Action::Update.allowed(&older).is_ok());
        assert!(Action::Update.allowed(&stopped).is_ok());
        // A newer service is never taken back to this version
        assert!(Action::Update.allowed(&newer).is_err());
        assert!(Action::Update.allowed(&none).is_err());
        assert!(Action::Remove { data: false }.allowed(&newer).is_ok());
        assert!(Action::Remove { data: true }.allowed(&none).is_err());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(outcome_of(0), Outcome::Done);
        assert_eq!(outcome_of(5), Outcome::Failed { code: 5 });
    }

    #[test]
    fn the_frontend_reads_these_shapes() {
        let json = |r: &Route| serde_json::to_value(r).unwrap();
        assert_eq!(json(&Route::NotAdded), serde_json::json!({ "state": "not_added" }));
        assert_eq!(
            json(&Route::NeedsUpdate { version: "3.0.2".into() }),
            serde_json::json!({ "state": "needs_update", "version": "3.0.2" })
        );
        assert_eq!(
            serde_json::to_value(Outcome::Failed { code: 5 }).unwrap(),
            serde_json::json!({ "outcome": "failed", "code": 5 })
        );
    }
}
