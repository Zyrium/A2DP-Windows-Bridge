//! The helper process: `bluecaster-core.exe --input-helper <pipe>`.
//!
//! Started by the core (in a service: as SYSTEM in the console session,
//! win_sys::session_launch). It opens the core's pipe, checks that the
//! process serving it is its own parent (held open, so its id cannot be
//! reused), and from then on passes what the core sends to Windows on the
//! input desktop ([`crate::engine`]). It takes nothing from its command
//! line but the pipe's name, which must have the exact form the core
//! makes; no file paths, no flags (the record switch exists in debug
//! builds only).
//!
//! It releases everything it holds and exits when the core closes the
//! pipe or dies, when its parent exits, when the core says Stop, and on
//! any frame that breaks the protocol. It logs nothing (no log file, no
//! console); the core logs the states it reports.
//!
//! Exit codes: 0 done, 2 bad arguments, 3 refused (not its parent's pipe,
//! or no parent), 4 protocol violation, 5 no interactive window station.

use std::os::windows::io::AsHandle;
use std::sync::mpsc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::desktop::{DesktopApi, Fixed, Win32};
use crate::engine::{HelperEngine, Injector, RecordInjector, SendInputInjector};
use crate::protocol::{self, ToCore, ToHelper};

/// The helper's mode on the command line
pub const HELPER_ARG: &str = "--input-helper";
/// Test switch, debug builds only: `--record <handle>`, an inherited pipe
/// handle the helper writes what it would inject to, instead of calling
/// `SendInput`. A release helper given it exits with code 2.
pub const RECORD_ARG: &str = "--record";
/// Every pipe the core serves for a helper: this, then 32 hex digits of
/// the system's random generator
pub const PIPE_PREFIX: &str = r"\\.\pipe\bluecaster-input-";

/// How often the helper follows the desktop and reads the lock keys
const TICK: Duration = Duration::from_millis(150);
/// How long it tries to open the core's pipe
const CONNECT_WAIT: Duration = Duration::from_secs(3);

pub const EXIT_OK: i32 = 0;
pub const EXIT_ARGS: i32 = 2;
pub const EXIT_REFUSED: i32 = 3;
pub const EXIT_PROTOCOL: i32 = 4;
pub const EXIT_STATION: i32 = 5;

/// The record switch is compiled in
pub const RECORD_AVAILABLE: bool = cfg!(debug_assertions);

/// A pipe name of the form the core makes
pub fn valid_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_PREFIX)
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    pipe: String,
    record: Option<usize>,
}

/// The arguments after [`HELPER_ARG`]
fn parse(args: &[String]) -> Option<Args> {
    let mut it = args.iter();
    let pipe = it.next().filter(|p| valid_pipe_name(p))?.clone();
    let record = match it.next().map(String::as_str) {
        None => None,
        Some(RECORD_ARG) if RECORD_AVAILABLE => Some(it.next()?.parse().ok().filter(|&n: &usize| n != 0)?),
        Some(_) => return None,
    };
    it.next().is_none().then_some(Args { pipe, record })
}

enum Cmd {
    Msg(ToHelper),
    /// The core or the parent went away (or broke the protocol): release,
    /// then exit with this code
    Gone(i32),
}

/// The helper process; `args` are those after [`HELPER_ARG`]. Returns its
/// exit code (the injecting thread may also end the process itself, once
/// it has released everything).
pub fn run(args: &[String]) -> i32 {
    let Some(a) = parse(args) else { return EXIT_ARGS };
    let record = match a.record {
        Some(n) => match win_sys::input_desktop::inherited_pipe(n) {
            Some(f) => Some(f),
            None => return EXIT_ARGS,
        },
        None => None,
    };
    if record.is_none() && win_sys::input_desktop::ensure_interactive_station().is_err() {
        return EXIT_STATION;
    }
    let Ok(parent) = win_sys::input_desktop::ParentProcess::open() else { return EXIT_REFUSED };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return EXIT_STATION };
    rt.block_on(serve(&a.pipe, parent, record))
}

async fn connect(name: &str) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let until = tokio::time::Instant::now() + CONNECT_WAIT;
    loop {
        match win_sys::pipe_peer::open_client(name) {
            Ok(c) => return Ok(c),
            // ERROR_PIPE_BUSY, or not there yet
            Err(e)
                if tokio::time::Instant::now() < until
                    && (e.raw_os_error() == Some(231) || e.kind() == std::io::ErrorKind::NotFound) =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => return Err(e),
        }
    }
}

/// Whether the parent that serves the pipe runs the right program. A
/// parent in session 0 (the service): it and this helper both run the
/// service folder's core, so a helper started from the user's folder is
/// never fed by the service, and a service registered with another
/// program feeds no helper. Any other parent (a core run by hand, the
/// tests' fake core): only the parent check (its process id) applies.
fn parent_program_ok(
    server_image: Option<&std::path::Path>,
    server_session: Option<u32>,
    own: Option<&std::path::Path>,
    service_core: Option<&std::path::Path>,
) -> bool {
    use win_sys::service_files::same_path;
    if server_session != Some(0) {
        return true;
    }
    match (server_image, own, service_core) {
        (Some(image), Some(own), Some(core)) => same_path(image, core) && same_path(own, core),
        _ => false,
    }
}

async fn serve(name: &str, parent: win_sys::input_desktop::ParentProcess, record: Option<std::fs::File>) -> i32 {
    let Ok(pipe) = connect(name).await else { return EXIT_REFUSED };
    // Only the process that started this one may feed it; the service
    // only when both run the service folder's core
    let own = std::env::current_exe().ok();
    let service_core = win_sys::service_files::service_core().ok();
    match win_sys::pipe_peer::server_process(pipe.as_handle()) {
        Ok(server)
            if server.pid == parent.pid()
                && parent_program_ok(
                    server.image.as_deref(),
                    server.session_id,
                    own.as_deref(),
                    service_core.as_deref(),
                ) => {}
        _ => return EXIT_REFUSED,
    }
    let (mut rd, mut wr) = tokio::io::split(pipe);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<ToCore>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let _ = out_tx.send(ToCore::Hello { version: protocol::VERSION, pid: std::process::id() });
    let injecting = {
        let out_tx = out_tx.clone();
        std::thread::Builder::new().name("inject".into()).spawn(move || {
            let code = match record {
                Some(file) => inject_loop(HelperEngine::new(RecordInjector::new(file), Fixed), &cmd_rx, &out_tx),
                None => inject_loop(HelperEngine::new(SendInputInjector, Win32), &cmd_rx, &out_tx),
            };
            // Everything is released: nothing else needs to run
            std::process::exit(code);
        })
    };
    if injecting.is_err() {
        return EXIT_STATION;
    }
    drop(out_tx);
    {
        let cmd_tx = cmd_tx.clone();
        let _ = std::thread::Builder::new().name("parent".into()).spawn(move || {
            parent.exited(u32::MAX);
            let _ = cmd_tx.send(Cmd::Gone(EXIT_OK));
        });
    }
    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if wr.write_all(&protocol::encode_to_core(&m)).await.is_err() {
                break;
            }
        }
    });
    loop {
        let code = match protocol::read_frame(&mut rd).await {
            Ok(Some(body)) => match protocol::decode_to_helper(&body) {
                Ok(m) => {
                    let stop = m == ToHelper::Stop;
                    let _ = cmd_tx.send(Cmd::Msg(m));
                    if stop {
                        break;
                    }
                    continue;
                }
                Err(_) => EXIT_PROTOCOL,
            },
            Ok(None) => EXIT_OK,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => EXIT_PROTOCOL,
            Err(_) => EXIT_OK,
        };
        let _ = cmd_tx.send(Cmd::Gone(code));
        break;
    }
    // The injecting thread exits the process once it has released
    // everything; this only waits for that
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// The injecting thread: attached to the input desktop, it runs every
/// batch and tick, and releases everything before it returns
fn inject_loop<I: Injector, D: DesktopApi>(
    mut e: HelperEngine<I, D>,
    rx: &mpsc::Receiver<Cmd>,
    tx: &tokio::sync::mpsc::UnboundedSender<ToCore>,
) -> i32 {
    let send = |out: Vec<ToCore>| {
        for m in out {
            let _ = tx.send(m);
        }
    };
    send(e.tick());
    let code = loop {
        match rx.recv_timeout(TICK) {
            Ok(Cmd::Msg(ToHelper::Inject(batch))) => send(e.batch(&batch)),
            Ok(Cmd::Msg(ToHelper::ReleaseAll)) => send(e.release_request()),
            Ok(Cmd::Msg(ToHelper::Stop)) => break EXIT_OK,
            Ok(Cmd::Gone(code)) => break code,
            Err(mpsc::RecvTimeoutError::Timeout) => send(e.tick()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break EXIT_OK,
        }
    };
    let released = e.release_all();
    e.injector().note(&format!("exit {code} released {released}"));
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_its_own_program_may_be_the_parent() {
        use std::path::Path;
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        let user = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        // The service and its helper, both from the service folder
        assert!(parent_program_ok(Some(svc), Some(0), Some(svc), Some(svc)));
        // A service registered with the user's folder's core
        assert!(!parent_program_ok(Some(user), Some(0), Some(user), Some(svc)));
        // A helper from the user's folder, the service from its folder
        assert!(!parent_program_ok(Some(svc), Some(0), Some(user), Some(svc)));
        assert!(!parent_program_ok(Some(svc), Some(0), Some(svc), None));
        assert!(!parent_program_ok(None, Some(0), Some(svc), Some(svc)));
        // A core run by hand, or a test's fake core: the process id alone
        assert!(parent_program_ok(Some(user), Some(1), Some(user), Some(svc)));
        assert!(parent_program_ok(None, Some(1), Some(user), None));
    }

    #[test]
    fn only_the_cores_pipe_names_are_taken() {
        let good = format!("{PIPE_PREFIX}{}", "0123456789abcdef".repeat(2));
        assert!(valid_pipe_name(&good));
        assert!(!valid_pipe_name(&good.to_uppercase()));
        assert!(!valid_pipe_name(&format!("{good}0")));
        assert!(!valid_pipe_name(r"\\.\pipe\bluecaster-input-..\..\x"));
        assert!(!valid_pipe_name(r"C:\Windows\System32\config\SAM"));
        assert!(!valid_pipe_name(&format!(r"\\server\pipe\bluecaster-input-{}", "a".repeat(32))));
        assert_eq!(parse(&a(&[&good])), Some(Args { pipe: good.clone(), record: None }));
        assert_eq!(parse(&a(&[])), None);
        assert_eq!(parse(&a(&["x"])), None);
        assert_eq!(parse(&a(&[&good, "--verbose"])), None);
        assert_eq!(parse(&a(&[&good, RECORD_ARG])), None);
        assert_eq!(parse(&a(&[&good, RECORD_ARG, "0"])), None);
        assert_eq!(parse(&a(&[&good, RECORD_ARG, "file.txt"])), None);
        let rec = parse(&a(&[&good, RECORD_ARG, "348"]));
        if RECORD_AVAILABLE {
            assert_eq!(rec, Some(Args { pipe: good.clone(), record: Some(348) }));
            assert_eq!(parse(&a(&[&good, RECORD_ARG, "348", "x"])), None);
        } else {
            assert_eq!(rec, None);
        }
        assert_eq!(run(&a(&["nonsense"])), EXIT_ARGS);
        // A handle number that is not an inherited pipe
        if RECORD_AVAILABLE {
            assert_eq!(run(&a(&[&good, RECORD_ARG, "2147483632"])), EXIT_ARGS);
        }
    }
}
