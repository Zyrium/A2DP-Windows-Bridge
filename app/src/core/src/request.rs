//! `bluecaster-core.exe --request <call> [--ticket <hex>]`: the elevated
//! helper of service mode. The core starts it through the session agent,
//! as administrator (a UAC prompt), when a normal client's request needs
//! an administrator (app.run_as_admin). It connects to the service pipe,
//! checks that the core serves it, sends that one request and exits:
//! 0 answered, 1 the request failed, 2 bad arguments, 3 no core or not
//! ours. The answer is printed (JSON) for a developer who runs it by hand
//! in an elevated terminal; the core hands it to the client that asked.
//!
//! The core recognises the helper by its elevated token and its program
//! (crate::rules); the ticket ties it to the call it was started for.

use std::time::Duration;

use ipc::{ClientMessage, Method, PROTOCOL_VERSION, read_frame, write_frame};
use serde_json::Value;

/// The argument that runs the helper
pub const REQUEST_ARG: &str = session_host::elevation::REQUEST_ARG;
const TICKET_ARG: &str = session_host::elevation::TICKET_ARG;

/// How long the helper waits for the core's answer
const ANSWER_WAIT: Duration = Duration::from_secs(300);

/// The call and the ticket of the arguments after `--request`
fn parse(args: &[String]) -> Result<(Method, Option<String>), String> {
    let mut call = None;
    let mut ticket = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            REQUEST_ARG => call = it.next().cloned(),
            TICKET_ARG => ticket = it.next().cloned(),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let call = call.ok_or("--request needs a request")?;
    let call: Method = serde_json::from_str(&call).map_err(|e| format!("not a request: {e}"))?;
    Ok((call, ticket))
}

pub fn run(args: &[String]) -> i32 {
    win_sys::console::attach_parent();
    let (call, ticket) = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}\nusage: --request <request JSON> [--ticket <hex>]");
            return 2;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return 3;
        }
    };
    match rt.block_on(send(call, ticket.as_deref())) {
        Ok(Ok(v)) => {
            println!("{v}");
            0
        }
        Ok(Err(e)) => {
            eprintln!("refused or failed: {e}");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            3
        }
    }
}

/// Ok(Ok(result)), Ok(Err(the error's JSON)), Err(no trusted core)
async fn send(call: Method, ticket: Option<&str>) -> Result<Result<Value, Value>, String> {
    let name = ipc::service_pipe_name();
    let mut pipe =
        win_sys::pipe_peer::open_client(&name).map_err(|e| format!("no core serves the service pipe: {e}"))?;
    // A fixed name can be taken by anyone first: for the real service the
    // service folder's core (wherever this helper runs from: an elevated
    // copy in the user's folder trusts the same server), in session 0, the
    // process the service control manager runs as the service; for a test
    // instance the core beside this program
    {
        use std::os::windows::io::AsHandle;
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        ipc::service_state::check_service_pipe(pipe.as_handle(), &me)?;
    }
    let version = env!("CARGO_PKG_VERSION");
    let client = match ticket {
        Some(t) => ipc::access::helper_client_name(version, t),
        None => format!("bluecaster-request/{version}"),
    };
    let io = |e: ipc::FrameError| e.to_string();
    write_frame(&mut pipe, &ClientMessage::Hello { protocol: PROTOCOL_VERSION, client }).await.map_err(io)?;
    write_frame(&mut pipe, &ClientMessage::Request { id: 1, call }).await.map_err(io)?;
    let answer = async {
        loop {
            let msg: Value = match read_frame(&mut pipe).await.map_err(io)? {
                Some(m) => m,
                None => return Err("the core closed the connection".to_string()),
            };
            match msg.get("type").and_then(Value::as_str) {
                Some("reject") => return Err(format!("the core refused this client: {}", msg["reason"])),
                Some("response") if msg.get("id").and_then(Value::as_u64) == Some(1) => {
                    return Ok(match msg.get("error").filter(|e| !e.is_null()) {
                        Some(e) => Err(e.clone()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    });
                }
                _ => {}
            }
        }
    };
    tokio::time::timeout(ANSWER_WAIT, answer).await.map_err(|_| "no answer from the core".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn arguments() {
        let (call, ticket) = parse(&args(&["--request", r#"{"method":"hid.code"}"#, "--ticket", "ab"])).unwrap();
        assert_eq!((call, ticket.as_deref()), (Method::HidCode, Some("ab")));
        assert!(parse(&args(&["--request", r#"{"method":"hid.code"}"#])).unwrap().1.is_none());
        assert!(parse(&args(&["--request"])).is_err());
        assert!(parse(&args(&["--request", "nonsense"])).is_err());
        assert!(parse(&args(&["--request", r#"{"method":"hid.code"}"#, "--other"])).is_err());
    }
}
