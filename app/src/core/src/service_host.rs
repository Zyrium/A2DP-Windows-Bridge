//! `bluecaster-core.exe --service`: the core as a Windows service.
//! Installing, updating and removing it is the set-up tool's
//! (`--service-setup`, crate::service_setup), which copies the service's
//! files into their own folder under Program Files first; stage 4's
//! `--install-service` / `--uninstall-service` run its `install` and
//! `remove`.
//!
//! The service's body is the headless core in service mode
//! (`--headless --service-data %ProgramData%\BlueCaster`, the folder from
//! the known-folder API): [`service_body`] runs it until the service
//! control manager says stop or shutdown, reporting start-pending and
//! stop-pending with checkpoints meanwhile, and passes the session changes
//! to the agent's hub (session_host::sessions). The same body runs as an
//! ordinary process for the tests (`--headless --service-data <dir>`).
//!
//! # The account: LocalSystem, one process
//!
//! The service runs as LocalSystem. A lower account (LocalService, or a
//! virtual account) cannot do two things the service must:
//!
//! - start the agent in the user's session with the user's token:
//!   WTSQueryUserToken needs SeTcbPrivilege, which only LocalSystem holds;
//! - start the input helper as SYSTEM in the user's session (input on the
//!   sign-in screen, UAC prompts): only a SYSTEM token can be moved into
//!   another session (SetTokenInformation(TokenSessionId), SeTcbPrivilege).
//!
//! The adapter's WinUSB interface would also need opening up for a lower
//! account: driver.rs writes BlueCaster's DeviceInterfaceGUIDs, and the
//! interface then gets WinUSB's default security (SYSTEM and the
//! Administrators, often everyone for read and write, depending on the
//! INF). A security descriptor written for the device when the driver
//! helper assigns WinUSB (the device's `Security` property,
//! SPDRP_SECURITY_SDS, e.g. `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;<service SID>)`)
//! could admit exactly the service's SID; that is the driver helper's job
//! (bluecaster-driver.exe, elevated), not done yet.
//!
//! The right end state is a split, not done here because it is a project
//! of its own: (1) a stack host running as a virtual account
//! (`NT SERVICE\BlueCaster`, SERVICE_SID_TYPE_RESTRICTED, no privileges
//! but SeChangeNotifyPrivilege) that owns the adapter (device ACL above),
//! the data folder (ACL for its SID instead of SYSTEM's) and the pipes;
//! (2) a tiny LocalSystem launcher service with SeTcbPrivilege,
//! SeAssignPrimaryTokenPrivilege and SeIncreaseQuotaPrivilege whose only
//! job is session tracking and win_sys::session_launch for the agent and
//! the input helper, talking to the stack host over a pipe that admits
//! only the host's service SID, and handing the host the processes'
//! handles (DuplicateHandle into the host) so that the host's pid and
//! handle checks keep working. Everything the core does with Windows'
//! own keys (the driver helper) already runs elevated in the user's
//! session, not in the service.
//!
//! Until then the single service keeps only the privileges it needs
//! ([`REQUIRED_PRIVILEGES`]) and an unrestricted service SID
//! ([`win_sys::service::SID_TYPE_UNRESTRICTED`]): a restricted (write-
//! restricted) token would also restrict the input helper, which gets a
//! copy of the service's token, on the input desktop and in the data
//! folder; the split above is where the restricted SID belongs.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use win_sys::service::{Control, InstallConfig, ServiceContext};

/// The service control manager starts the core with this
pub const SERVICE_ARG: &str = "--service";
/// Stage 4's name for `--service-setup install` (needs an administrator)
pub const INSTALL_ARG: &str = "--install-service";
/// Stage 4's name for `--service-setup remove` (needs an administrator)
pub const UNINSTALL_ARG: &str = "--uninstall-service";

pub const EXIT_FAILED: i32 = 1;
/// `--service` outside the service control manager
pub const EXIT_NOT_A_SERVICE: i32 = 1063;

/// The privileges the service keeps; Windows removes every other one from
/// its token: acting as part of the system (WTSQueryUserToken, moving a
/// token to another session), starting a process with another token and
/// raising its quotas (CreateProcessAsUserW), and the traversal right
/// every process needs
pub const REQUIRED_PRIVILEGES: &[&str] =
    &["SeTcbPrivilege", "SeAssignPrimaryTokenPrivilege", "SeIncreaseQuotaPrivilege", "SeChangeNotifyPrivilege"];

/// In services.msc. Not in app/lang: services.msc shows one description
/// for every user, so it stays in English.
pub const DESCRIPTION: &str =
    "Connects Bluetooth headphones, keyboards and mice through BlueCaster, even before you sign in.";

/// The service as the set-up tool registers it for the program `exe` (the
/// service folder's core)
pub fn install_config(exe: &Path) -> InstallConfig {
    InstallConfig {
        name: ipc::access::SERVICE_NAME.into(),
        display_name: "BlueCaster".into(),
        description: DESCRIPTION.into(),
        command_line: format!("\"{}\" {SERVICE_ARG}", exe.display()),
        // Keyboards and mice are wanted on the sign-in screen: not delayed
        delayed_start: false,
        sid_type: win_sys::service::SID_TYPE_UNRESTRICTED,
        privileges: REQUIRED_PRIVILEGES.iter().map(|p| p.to_string()).collect(),
        // The only keyboard may depend on it: back within seconds
        restart_delays: vec![Duration::from_secs(1), Duration::from_secs(5), Duration::from_secs(30)],
        failure_reset: Duration::from_secs(24 * 60 * 60),
        restart_on_error_exit: true,
        preshutdown_timeout: Duration::from_secs(15),
    }
}

/// The arguments the body runs the core with
pub fn core_args(data_dir: &Path) -> Vec<String> {
    vec![crate::headless::HEADLESS_ARG.into(), crate::headless::SERVICE_DATA_ARG.into(), data_dir.display().to_string()]
}

/// `--service`, `--service-setup ...`, `--install-service`,
/// `--uninstall-service`: the exit code; None for any other first argument
pub fn dispatch(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some(SERVICE_ARG) => Some(run_service()),
        Some(crate::service_setup::SETUP_ARG) => Some(crate::service_setup::run(&args[1..])),
        Some(INSTALL_ARG | UNINSTALL_ARG) => {
            let action = if args[0] == INSTALL_ARG { "install" } else { "remove" };
            let rest = std::iter::once(action.to_string()).chain(args[1..].iter().cloned());
            Some(crate::service_setup::run(&rest.collect::<Vec<_>>()))
        }
        _ => None,
    }
}

fn run_service() -> i32 {
    match win_sys::service::run_dispatcher(ipc::access::SERVICE_NAME, service_body) {
        Ok(()) => 0,
        Err(e) if e.raw_os_error() == Some(win_sys::service::NOT_A_SERVICE) => {
            win_sys::console::attach_parent();
            eprintln!("{SERVICE_ARG} is used by Windows to start the BlueCaster service and can't be run directly.");
            EXIT_NOT_A_SERVICE
        }
        Err(e) => {
            win_sys::console::attach_parent();
            eprintln!("{SERVICE_ARG}: {e}");
            EXIT_FAILED
        }
    }
}

/// The core is up (headless::run_with's callback)
static UP: AtomicBool = AtomicBool::new(false);

/// The service's main function: the headless core in service mode, with
/// the controls on a thread beside it
fn service_body(ctx: &ServiceContext) -> u32 {
    let dir = match win_sys::service::program_data_dir() {
        Ok(d) => d.join("BlueCaster"),
        // ERROR_PATH_NOT_FOUND
        Err(_) => return 3,
    };
    let args = core_args(&dir);
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| controls(ctx, &done));
        let code = run_core(&args);
        done.store(true, Ordering::Release);
        ctx.stopping();
        code.max(0) as u32
    })
}

/// The steps of main.rs for `--headless`: the service folder, the log,
/// then the headless core until it is told to stop
fn run_core(args: &[String]) -> i32 {
    bluecaster_core::paths::set_old_app_check(|| false);
    if let Some(code) = crate::headless::enter_service_mode(args) {
        return code;
    }
    let config = bluecaster_core::paths::config_dir();
    let dev = bluecaster_core::settings::Settings::load(&config).log();
    let log = crate::logging::init(&crate::logging::LogOptions {
        dir: Some(bluecaster_core::paths::log_dir(&config)),
        console: false,
        crash_dir: Some(bluecaster_core::paths::crash_dir(&config)),
        dev,
        agent: false,
    });
    tracing::info!("service: starting");
    let code = crate::headless::run_with(args, Some(Box::new(|| UP.store(true, Ordering::Release))));
    tracing::info!("service: stopped ({code})");
    drop(log);
    code
}

/// What one control does; true for a stop
pub(crate) fn on_control(c: Control) -> bool {
    match c {
        Control::Stop | Control::Shutdown => {
            tracing::info!("service: {}", if c == Control::Stop { "stop" } else { "shutdown" });
            crate::headless::request_stop();
            true
        }
        Control::SessionChange { event, session } => {
            if let Some(e) = session_host::SessionEvent::from_wts(event, session)
                && let Some(hub) = session_host::hub()
            {
                hub.session_event(e);
            }
            false
        }
        Control::Power { event } => {
            // Sleep and resume come through the core's own registration
            // (win_sys::power), which works in a service as well
            tracing::debug!("service: power event {event}");
            false
        }
    }
}

/// Reports running once the core is up, passes on the controls, and keeps
/// the checkpoints moving while the core starts or stops
fn controls(ctx: &ServiceContext, done: &AtomicBool) {
    let mut reported_up = false;
    let mut last_progress = std::time::Instant::now();
    while !done.load(Ordering::Acquire) {
        if !reported_up && UP.load(Ordering::Acquire) {
            ctx.running();
            reported_up = true;
        }
        match ctx.next_control_timeout(Duration::from_millis(100)) {
            Ok(c) => {
                if on_control(c) {
                    ctx.stopping();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_progress.elapsed() >= Duration::from_secs(1) {
            ctx.progress();
            last_progress = std::time::Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_is_configured_as_intended() {
        let exe = Path::new(r"C:\Program Files\BlueCaster\bluecaster-core.exe");
        let c = install_config(exe);
        assert_eq!(c.name, "BlueCaster");
        assert_eq!(c.command_line, r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --service"#);
        assert!(!c.delayed_start, "keyboards on the sign-in screen want it early");
        assert_eq!(c.sid_type, win_sys::service::SID_TYPE_UNRESTRICTED);
        assert_eq!(c.privileges.len(), 4);
        assert!(c.privileges.iter().any(|p| p == "SeTcbPrivilege"));
        assert!(!c.privileges.iter().any(|p| p == "SeDebugPrivilege" || p == "SeImpersonatePrivilege"));
        assert!(c.restart_delays.first().is_some_and(|d| *d <= Duration::from_secs(5)));
        assert!(c.restart_on_error_exit);
        // One sentence, short enough for services.msc
        assert!(c.description.ends_with('.') && c.description.len() <= 120);
        assert_eq!(c.description.matches(". ").count(), 0);
    }

    #[test]
    fn the_body_runs_the_headless_core_in_service_mode() {
        let a = core_args(Path::new(r"C:\ProgramData\BlueCaster"));
        assert_eq!(a, ["--headless", "--service-data", r"C:\ProgramData\BlueCaster"]);
        assert_eq!(crate::headless::service_data_arg(&a), Some(Ok(r"C:\ProgramData\BlueCaster".into())));
    }

    #[test]
    fn other_arguments_are_not_the_services() {
        assert_eq!(dispatch(&["--headless".into()]), None);
        assert_eq!(dispatch(&[]), None);
    }

    #[test]
    fn session_changes_without_a_hub_are_ignored() {
        // No hub in the unit tests: nothing happens, nothing is a stop
        assert!(!on_control(Control::SessionChange { event: 5, session: 1 }));
        assert!(!on_control(Control::Power { event: 4 }));
    }
}
