//! Group `elevation` in the agent: runs the core's elevated programs in
//! the user's session ([`crate::LocalHost`]). A test agent refuses them
//! ([`refuse_all`]): no test shows a UAC prompt.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::host::elevation::{ElevatedError, ElevationHost, Request};
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler, LocalHost};

static REFUSE: AtomicBool = AtomicBool::new(false);

/// This agent runs nothing as administrator (tests); before
/// [`crate::run_agent`]
pub fn refuse_all() {
    REFUSE.store(true, Ordering::Release);
}

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Elevation { refuse: REFUSE.load(Ordering::Acquire) })
}

struct Elevation {
    refuse: bool,
}

fn failed(e: ElevatedError) -> WireError {
    let what = match &e {
        ElevatedError::Cancelled => "declined",
        ElevatedError::Failed(_) => "failed",
    };
    tracing::info!("elevation: {what}");
    WireError::failed(what, &e)
}

/// A setup the agent runs for a core: a full path to an .exe that exists
fn setup_ok(program: &Path) -> bool {
    program.is_absolute() && program.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) && program.is_file()
}

impl Handler for Elevation {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        let request: Request = decode(body)?;
        if self.refuse {
            tracing::info!("elevation: refused (test agent)");
            return Err(failed(ElevatedError::Failed("this agent runs nothing as administrator".into())));
        }
        match request {
            Request::DriverHelper { args } => {
                // The runner names the result file itself
                if args.iter().any(|a| a == "--out") {
                    return Err(WireError::new(ErrorCode::BadRequest, "--out is the agent's"));
                }
                let run = LocalHost.driver_helper(&args).map_err(failed)?;
                tracing::info!("elevation: driver helper exit {} (result: {})", run.code, run.result.is_some());
                reply(run)
            }
            Request::RunSetup { program, args } => {
                let program = Path::new(&program);
                if !setup_ok(program) {
                    return Err(WireError::new(ErrorCode::BadRequest, "not a program"));
                }
                let code = LocalHost.run_setup(program, &args).map_err(failed)?;
                tracing::info!("elevation: setup exit {code}");
                reply(code)
            }
            Request::CoreRequest { request, ticket } => {
                if !crate::host::elevation::core_request_ok(&request, &ticket) {
                    return Err(WireError::new(ErrorCode::BadRequest, "not a request for the elevated helper"));
                }
                let code = LocalHost.core_request(&request, &ticket).map_err(failed)?;
                tracing::info!("elevation: elevated request exit {code}");
                reply(code)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_agent_refuses_and_bad_requests_are_refused() {
        // Never runs anything: refused first, or not a program
        let mut h = Elevation { refuse: true };
        let e = h.request(serde_json::json!({"op": "driver_helper", "args": ["assign", "X"]})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Failed);
        assert!(matches!(e.detail_as::<ElevatedError>(), Some(ElevatedError::Failed(_))));
        let mut h = Elevation { refuse: false };
        let out = serde_json::json!({"op": "driver_helper", "args": ["list", "--out", "x.json"]});
        assert_eq!(h.request(out).unwrap_err().code, ErrorCode::BadRequest);
        let setup = serde_json::json!({"op": "run_setup", "program": "relative.exe", "args": []});
        assert_eq!(h.request(setup).unwrap_err().code, ErrorCode::BadRequest);
        let missing = serde_json::json!({"op": "run_setup", "program": r"C:\no\such\setup.exe", "args": []});
        assert_eq!(h.request(missing).unwrap_err().code, ErrorCode::BadRequest);
        assert!(!setup_ok(Path::new(r"C:\Windows")));
        let plain =
            serde_json::json!({"op": "core_request", "request": r#"{"method":"state.snapshot"}"#, "ticket": "ab"});
        assert_eq!(h.request(plain).unwrap_err().code, ErrorCode::BadRequest);
        // A test agent refuses the helper too
        let mut t = Elevation { refuse: true };
        let admin = serde_json::json!({"op": "core_request", "request": r#"{"method":"hid.code"}"#, "ticket": "ab"});
        assert_eq!(t.request(admin).unwrap_err().code, ErrorCode::Failed);
    }
}
