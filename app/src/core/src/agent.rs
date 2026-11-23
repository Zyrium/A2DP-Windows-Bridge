//! The session agent: `bluecaster-core.exe --agent [--child]`. Does the
//! core's session-bound work (session_host's groups: capture, devices,
//! render) for a core that runs with `--session-agent`, over the core's
//! agent pipe. Started by hand it waits for a core, serves it, and waits
//! again when the core goes; `--child` (the core started it) exits instead.
//! Its log lines go to the core's log while connected, else to stderr. No
//! settings folder, no window.

/// The first argument that starts the agent
pub const AGENT_ARG: &str = "--agent";
/// Started by the core: exit when the core goes away
pub const CHILD_ARG: &str = "--child";
/// Tests: a tray that only logs, Run values in memory, nothing run as
/// administrator. Debug builds only ([`TEST_SWITCHES`]).
pub const TEST_SHELL_ARG: &str = "--test-shell";

/// The test switches (`--test-shell`, session_host's fake volume) exist in
/// debug builds only, which `cargo test` runs: an installed (release)
/// agent or core never fakes its work, whoever starts it with them.
pub const TEST_SWITCHES: bool = cfg!(debug_assertions);

/// The agent of a core in service mode: the service's agent pipe, served
/// only by the core's own program
pub const SERVICE_PIPE_ARG: &str = "--service-pipe";

/// Tests (debug builds only): a file that stands for the console's session
/// in service mode, holding the session's number or `none` (nobody signed
/// in), read once a second (session_host::SessionSource::TestFile)
pub const TEST_SESSIONS_VAR: &str = "BLUECASTER_TEST_SESSIONS";

/// What the agent of a service gets beside the clean environment of the
/// user's token: the test instance and folders (debug builds only; the
/// installed service has neither)
fn agent_env() -> Vec<(String, String)> {
    if !TEST_SWITCHES {
        return Vec::new();
    }
    ["BLUECASTER_INSTANCE", "BLUECASTER_CONFIG_DIR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
}

/// Where the hub takes the agent's session from: its own session as an
/// ordinary process, the console's as a service in session 0, a test's
/// file with [`TEST_SESSIONS_VAR`]
pub(crate) fn session_source(own_session: u32, test_file: Option<std::path::PathBuf>) -> session_host::SessionSource {
    match test_file {
        Some(f) if TEST_SWITCHES => session_host::SessionSource::TestFile(f),
        _ if own_session == 0 => session_host::SessionSource::Console,
        _ => session_host::SessionSource::Own(own_session),
    }
}

/// The real service: a core in session 0 that is not a test instance
pub(crate) fn real_service(core_session: u32) -> bool {
    core_session == 0 && !ipc::service_state::test_instance()
}

/// The program the hub starts as the agent and requires of it
/// (session_host's `authorize`): this program, which for the real service
/// must be the service folder's core. A service registered with another
/// program (the stage 4 install in the user's folder) gets no agent.
pub(crate) fn agent_program(
    exe: &std::path::Path,
    service_core: Option<&std::path::Path>,
    real_service: bool,
) -> Result<(), String> {
    let required = win_sys::service_files::required_core(exe, service_core, real_service)?;
    if win_sys::service_files::same_path(required, exe) {
        Ok(())
    } else {
        Err("session agent: the service does not run from the service folder".into())
    }
}

pub fn run(args: &[String]) -> i32 {
    let log = crate::logging::init(&crate::logging::LogOptions { console: true, agent: true, ..Default::default() });
    let (mut child, mut test_shell, mut service) = (false, false, false);
    for a in args {
        match a.as_str() {
            CHILD_ARG => child = true,
            SERVICE_PIPE_ARG => service = true,
            TEST_SHELL_ARG if TEST_SWITCHES => test_shell = true,
            _ => {
                tracing::error!("session agent: usage: --agent [--child] [--service-pipe]");
                return 2;
            }
        }
    }
    // The shell of a headless core: the tray (session_host's shell group)
    if test_shell {
        session_host::handlers::shell::set_tray_backend(session_host::handlers::shell::LogOnlyTray::factory());
        session_host::handlers::autostart::use_memory_store();
        session_host::handlers::elevation::refuse_all();
    } else {
        session_host::handlers::shell::set_tray_backend(crate::agent_shell::factory());
    }
    // The user's folder, read (never changed) for a core's one-time move
    // into its service folder
    if let Some(dir) = bluecaster_core::paths::user_config_location() {
        session_host::handlers::migration::set_user_folder(dir);
    }
    let pipe_name = if service { Ok(session_host::service_pipe_name()) } else { session_host::pipe_name() };
    let code = match pipe_name {
        Ok(pipe_name) => {
            let _span = tracing::info_span!("agent").entered();
            tracing::info!(
                "session agent: starting (pid {}, started by the core: {child}, service: {service})",
                std::process::id()
            );
            session_host::run_agent(&session_host::AgentOptions { pipe_name, child, service })
        }
        Err(e) => {
            tracing::error!("session agent: no pipe name: {e}");
            3
        }
    };
    drop(log);
    code
}

/// Makes this process's session-bound work run in the session agent,
/// started from this program when none is connected
pub fn use_agent() -> Result<(), String> {
    use_agent_with(&[])
}

/// [`use_agent`], the agent started with `extra` arguments as well
pub fn use_agent_with(extra: &[&str]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the program for the session agent: {e}"))?;
    let service = bluecaster_core::paths::service_mode();
    let pipe_name = if service {
        session_host::service_pipe_name()
    } else {
        session_host::pipe_name().map_err(|e| format!("session agent: no pipe name: {e}"))?
    };
    let mut args: Vec<String> = vec![AGENT_ARG.into(), CHILD_ARG.into()];
    if service {
        args.push(SERVICE_PIPE_ARG.into());
    }
    args.extend(extra.iter().map(|a| a.to_string()));
    let core_session = win_sys::pipe::process_session_id(std::process::id()).unwrap_or(0);
    // The real service (session 0, not a test instance) runs only from the
    // service folder, and its agent is that folder's core
    if service {
        agent_program(&exe, ipc::service_state::service_core().as_deref(), real_service(core_session))?;
    }
    // Service mode: only the process the hub started, from our own
    // program, in the session it was started in
    let agent_check = service.then(|| session_host::AgentCheck { exe: exe.clone(), core_session });
    let env = if service { agent_env() } else { Vec::new() };
    let launch = session_host::LaunchConfig { exe, args, env };
    let mut config = session_host::HubConfig { agent_check, ..session_host::HubConfig::new(pipe_name, Some(launch)) };
    if service {
        let test_file = std::env::var_os(TEST_SESSIONS_VAR).map(std::path::PathBuf::from);
        config.sessions = session_source(core_session, test_file);
        config.service_pipe = true;
    }
    session_host::use_session_agent(config)?;
    tracing::info!("audio capture: through a capture agent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_host::SessionSource;

    #[test]
    fn the_agents_session_comes_from_the_mode() {
        assert_eq!(session_source(0, None), SessionSource::Console);
        assert_eq!(session_source(2, None), SessionSource::Own(2));
        let f = std::path::PathBuf::from("sessions.txt");
        let with_file = session_source(2, Some(f.clone()));
        if TEST_SWITCHES {
            assert_eq!(with_file, SessionSource::TestFile(f));
        } else {
            assert_eq!(with_file, SessionSource::Own(2));
        }
    }

    #[test]
    fn the_real_services_agent_is_the_service_folders_core() {
        use std::path::Path;
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        let user = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        assert_eq!(agent_program(svc, Some(svc), true), Ok(()));
        assert!(agent_program(user, Some(svc), true).is_err(), "the stage 4 install in the user's folder");
        assert!(agent_program(svc, None, true).is_err());
        // A test instance or a core run by hand: its own program
        assert_eq!(agent_program(user, Some(svc), false), Ok(()));
        // Not session 0: never the real service
        assert!(!real_service(2));
    }

    /// Only the test instance and folders go to a service's agent, and
    /// only in debug builds
    #[test]
    fn the_agent_of_a_service_gets_few_variables() {
        let names: Vec<String> = agent_env().into_iter().map(|(k, _)| k).collect();
        assert!(names.iter().all(|k| k == "BLUECASTER_INSTANCE" || k == "BLUECASTER_CONFIG_DIR"), "{names:?}");
        if !TEST_SWITCHES {
            assert!(names.is_empty());
        }
    }
}
