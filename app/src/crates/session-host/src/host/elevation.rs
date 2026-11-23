//! Group `elevation`: programs run as administrator (the UAC prompt). A
//! service has no desktop to show the prompt on, so the agent runs them in
//! the user's session and hands back the result; the core still decides
//! what to run and with which arguments.
//!
//! - [`ElevationHost::driver_helper`]: `bluecaster-driver.exe`, next to
//!   the program that runs it (the agent's own folder, never a path the
//!   core names). The runner adds `--out <file>` itself (a file of its
//!   own in its temp folder), reads the helper's JSON from it and deletes it.
//! - [`ElevationHost::run_setup`]: a setup the core prepared (VB-CABLE's).
//! - [`ElevationHost::core_request`]: service mode's elevated helper,
//!   `bluecaster-core.exe --request <call> --ticket <ticket>` (the agent's
//!   own program), which connects to the core as an elevated client and
//!   sends that one request.
//!
//! STAGE 3: these requests cross a privilege boundary once the core is a
//! service: what the core hands over is listed on [`Request`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "elevation";

/// How long the core waits for an elevated run: the user may take a while
/// on the prompt, and a driver change takes up to a minute or two
pub const ELEVATED_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The driver helper with `args`: its command (assign, restore,
    /// windows-keys, remove-cable), an adapter's instance ID or a radio's
    /// address, `--previous @<file>`, `--keys <file>` (BlueCaster's link
    /// keys in plain text, in the core's settings folder), a setup's path
    /// (remove-cable), `--log <file>` (the core's log folder), `--verbose`.
    /// Reply: [`HelperRun`]
    DriverHelper { args: Vec<String> },
    /// `program` (a full path to an .exe; VB-CABLE's setup, unpacked by
    /// the core into its temp folder) with `args` ("-i -h"). Reply: `u32`,
    /// its exit code
    RunSetup { program: String, args: Vec<String> },
    /// Service mode: the elevated helper for one request that needs an
    /// administrator (`request`: the call as JSON, ipc::Method; `ticket`:
    /// hex, the core's). Reply: `u32`, the helper's exit code
    CoreRequest { request: String, ticket: String },
}

/// The argument of the elevated helper: `--request <call JSON>`
pub const REQUEST_ARG: &str = "--request";
/// Its ticket: `--ticket <hex>`
pub const TICKET_ARG: &str = "--ticket";

/// A request the agent may hand to the elevated helper: a call that needs
/// an administrator (ipc::access), with a ticket of hex digits
pub fn core_request_ok(request: &str, ticket: &str) -> bool {
    let ticket_ok = !ticket.is_empty() && ticket.len() <= 64 && ticket.bytes().all(|b| b.is_ascii_hexdigit());
    let call_ok = serde_json::from_str::<ipc::Method>(request)
        .is_ok_and(|m| ipc::access::access(&m) == ipc::access::Access::Admin);
    ticket_ok && call_ok
}

/// The driver helper's exit code and what it wrote to its `--out` file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HelperRun {
    pub code: u32,
    pub result: Option<Value>,
}

/// Why an elevated program did not run
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ElevatedError {
    /// The user answered "No" on the UAC prompt
    #[error("the administrator prompt was declined")]
    Cancelled,
    /// It could not be started or waited for, or the agent is not there
    #[error("{0}")]
    Failed(String),
}

impl From<win_sys::elevation::ElevationError> for ElevatedError {
    fn from(e: win_sys::elevation::ElevationError) -> ElevatedError {
        match e {
            win_sys::elevation::ElevationError::Cancelled => ElevatedError::Cancelled,
            e => ElevatedError::Failed(e.to_string()),
        }
    }
}

/// The facade's elevated runs. They block until the program exits (the
/// user answers the prompt first): only for a request's worker thread.
pub trait ElevationHost {
    fn driver_helper(&self, args: &[String]) -> Result<HelperRun, ElevatedError>;
    fn run_setup(&self, program: &Path, args: &[String]) -> Result<u32, ElevatedError>;
    fn core_request(&self, request: &str, ticket: &str) -> Result<u32, ElevatedError>;
}

/// bluecaster-driver.exe next to this program
pub fn helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("bluecaster-driver.exe")))
        .unwrap_or_else(|| "bluecaster-driver.exe".into())
}

impl ElevationHost for LocalHost {
    fn driver_helper(&self, args: &[String]) -> Result<HelperRun, ElevatedError> {
        let out = std::env::temp_dir().join(format!("bluecaster-driver-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut all: Vec<String> = args.to_vec();
        all.extend(["--out".to_string(), out.to_string_lossy().into_owned()]);
        let code = win_sys::elevation::run_elevated(&helper_path(), &all)?;
        let result: Option<Value> = std::fs::read_to_string(&out)
            .ok()
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok());
        let _ = std::fs::remove_file(&out);
        Ok(HelperRun { code, result })
    }

    fn run_setup(&self, program: &Path, args: &[String]) -> Result<u32, ElevatedError> {
        Ok(win_sys::elevation::run_elevated(program, args)?)
    }

    fn core_request(&self, request: &str, ticket: &str) -> Result<u32, ElevatedError> {
        if !core_request_ok(request, ticket) {
            return Err(ElevatedError::Failed("not a request for the elevated helper".into()));
        }
        // This program: the agent is bluecaster-core.exe itself
        let exe = std::env::current_exe().map_err(|e| ElevatedError::Failed(e.to_string()))?;
        let args = [REQUEST_ARG.to_string(), request.to_string(), TICKET_ARG.to_string(), ticket.to_string()];
        Ok(win_sys::elevation::run_elevated(&exe, &args)?)
    }
}

fn elevated_error(e: crate::HostError) -> ElevatedError {
    match &e {
        crate::HostError::Remote(w) => {
            w.detail_as::<ElevatedError>().unwrap_or_else(|| ElevatedError::Failed(e.to_string()))
        }
        _ => ElevatedError::Failed(e.to_string()),
    }
}

impl RemoteHost {
    /// Waits for an agent as any request does, then for the run itself up
    /// to [`ELEVATED_TIMEOUT`]
    fn elevated<T: serde::de::DeserializeOwned>(&self, request: &Request) -> Result<T, ElevatedError> {
        let hub = self.hub();
        if hub.wait_agent(hub.request_timeout()).is_none() {
            return Err(elevated_error(crate::HostError::NoAgent));
        }
        hub.request_on(GROUP, crate::wire::to_body(request), ELEVATED_TIMEOUT)
            .and_then(|(v, _)| crate::wire::decode_reply(v))
            .map_err(elevated_error)
    }
}

impl ElevationHost for RemoteHost {
    fn driver_helper(&self, args: &[String]) -> Result<HelperRun, ElevatedError> {
        self.elevated(&Request::DriverHelper { args: args.to_vec() })
    }

    fn run_setup(&self, program: &Path, args: &[String]) -> Result<u32, ElevatedError> {
        self.elevated(&Request::RunSetup { program: program.to_string_lossy().into_owned(), args: args.to_vec() })
    }

    fn core_request(&self, request: &str, ticket: &str) -> Result<u32, ElevatedError> {
        self.elevated(&Request::CoreRequest { request: request.into(), ticket: ticket.into() })
    }
}

/// [`ElevationHost::core_request`] of this process's host
pub fn core_request(request: &str, ticket: &str) -> Result<u32, ElevatedError> {
    crate::host().core_request(request, ticket)
}

/// [`ElevationHost::driver_helper`] of this process's host
pub fn driver_helper(args: &[String]) -> Result<HelperRun, ElevatedError> {
    crate::host().driver_helper(args)
}

/// [`ElevationHost::run_setup`] of this process's host
pub fn run_setup(program: &Path, args: &[String]) -> Result<u32, ElevatedError> {
    crate::host().run_setup(program, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_round_trip() {
        for e in [ElevatedError::Cancelled, ElevatedError::Failed("x".into())] {
            let wire = crate::WireError::failed("run", &e);
            assert_eq!(elevated_error(crate::HostError::Remote(wire)), e);
        }
        assert!(matches!(elevated_error(crate::HostError::AgentLost), ElevatedError::Failed(_)));
        assert!(helper_path().ends_with("bluecaster-driver.exe"));
    }

    #[test]
    fn only_calls_that_need_an_administrator_go_to_the_helper() {
        assert!(core_request_ok(r#"{"method":"diag.capture_start"}"#, "ab12"));
        assert!(!core_request_ok(r#"{"method":"state.snapshot"}"#, "ab12"));
        assert!(!core_request_ok(r#"{"method":"diag.capture_start"}"#, ""));
        assert!(!core_request_ok(r#"{"method":"diag.capture_start"}"#, "a b"));
        assert!(!core_request_ok("not json", "ab12"));
    }
}
