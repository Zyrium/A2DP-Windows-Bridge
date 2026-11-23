//! The input helper (crate input-helper) run as the real executable in its
//! record mode (a test switch of debug builds): it writes what it would
//! pass to Windows to a pipe this test reads, and never calls SendInput.
//! The helper runs as this user, a plain child, as it does outside a
//! service. Nothing here injects input.

#![cfg(debug_assertions)]

use std::io::BufRead;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use input_helper::client::{HelperProcess, Launcher};
use input_helper::protocol::{self, ToCore, ToHelper};
use input_helper::{HelperConfig, HelperSink, HidCounters, InputSink, SessionLauncher};
use win_sys::input_inject::{Inject, MouseButton};

/// Console programs started from a test get no terminal window
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");

/// The record pipe: its write end inheritable (each helper started from
/// here inherits it), its lines collected on a thread
struct Record {
    number: usize,
    _write: OwnedHandle,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Record {
    fn new() -> Record {
        let (read, write) = win_sys::input_desktop::inheritable_pipe().unwrap();
        let number = win_sys::input_desktop::handle_number(write.as_handle());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for l in std::io::BufReader::new(read).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(l);
            }
        });
        Record { number, _write: write, lines }
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// Waits until `f` holds for the lines (10 s at most)
    fn wait(&self, what: &str, f: impl Fn(&[String]) -> bool) -> Vec<String> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let l = self.lines();
            if f(&l) {
                return l;
            }
            assert!(Instant::now() < until, "{what}: never; record {l:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < until, "{what}: never");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn key(scan: u16, up: bool) -> Inject {
    Inject::Key { scan, extended: false, up }
}

fn sink(record: &Record) -> Arc<HelperSink> {
    let sink = HelperSink::new(
        HelperConfig { exe: PathBuf::from(EXE), record: Some(record.number) },
        Arc::new(SessionLauncher),
    );
    sink.start();
    wait_for("the helper connects", || sink.connected());
    sink
}

#[test]
fn a_typed_sequence_arrives_in_order() {
    let record = Record::new();
    let sink = sink(&record);
    let c = Arc::new(HidCounters::default());
    // Keys and buttons with moves between them (adjacent moves would merge)
    let mut sent = Vec::new();
    for n in 0..60u16 {
        sent.push(key(0x10 + n % 30, false));
        sent.push(Inject::Move { dx: n as i32, dy: -1 });
        sent.push(key(0x10 + n % 30, true));
    }
    sent.push(Inject::Button { button: MouseButton::Right, up: false });
    sent.push(Inject::Wheel(-240));
    sent.push(Inject::Button { button: MouseButton::Right, up: true });
    for chunk in sent.chunks(7) {
        sink.pass(None, chunk, &c);
    }
    let lines = record.wait("all inputs recorded", |l| l.len() >= sent.len());
    let expect: Vec<String> = sent.iter().map(input_helper::engine::record_line).collect();
    assert_eq!(lines, expect);
    wait_for("the acks counted", || c.injected.load(Ordering::Relaxed) == sent.len() as u64);
    assert_eq!(c.dropped.load(Ordering::Relaxed) + c.blocked.load(Ordering::Relaxed), 0);
    // The lock keys reported (whatever they are on this machine)
    wait_for("lock keys reported", || sink.lock_keys().is_some());
    assert!(sink.lock_keys().unwrap() <= 7);
    // A held key at the stop: released, then the helper exits
    sink.pass(None, &[key(0x2A, false)], &c);
    record.wait("the key recorded", |l| l.len() == sent.len() + 1);
    sink.stop();
    let lines = record.wait("the stop", |l| l.last().is_some_and(|x| x.starts_with("exit")));
    assert_eq!(lines[sent.len() + 1..], ["key 0x2a up", "exit 0 released 0"]);
    assert_eq!(sink.started(), 1);
}

/// Killed hard, the helper cannot release anything itself: its
/// replacement releases what it held
#[test]
fn a_killed_helpers_replacement_releases_its_keys() {
    let record = Record::new();
    let sink = sink(&record);
    let tested = Arc::new(Mutex::new(Vec::new()));
    let t = tested.clone();
    sink.on_tested(Box::new(move |r| t.lock().unwrap().push(r)));
    let c = Arc::new(HidCounters::default());
    sink.pass(None, &[key(0x2A, false), key(0x1E, false), Inject::Button { button: MouseButton::Left, up: false }], &c);
    record.wait("the downs", |l| l.len() == 3);
    let first = sink.helper_pid().unwrap();
    let killed = Command::new("taskkill")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/F", "/PID", &first.to_string()])
        .stdout(Stdio::null())
        .status();
    assert!(killed.unwrap().success());
    wait_for("a new helper", || sink.connected() && sink.helper_pid().is_some_and(|p| p != first));
    // The new helper tests itself (its zero move) after the releases
    let lines = record.wait("the releases and the self-test", |l| l.len() >= 7);
    // Keys, then the modifier, then the button
    assert_eq!(lines[3..7], ["key 0x1e up", "key 0x2a up", "button Left up", "move 0 0"]);
    wait_for("the self-test result", || !tested.lock().unwrap().is_empty());
    assert_eq!(*tested.lock().unwrap(), [input_helper::client::SelfTest::Passed { windows_saw: None }]);
    // Input while no helper ran was dropped and counted, not queued for
    // the next one; input now goes to the new helper
    sink.pass(None, &[key(0x30, false), key(0x30, true)], &c);
    record.wait("input to the new helper", |l| l.len() == 9);
    sink.stop();
    assert_eq!(sink.started(), 2);
}

/// What counts as delivered through the helper: a batch of that device
/// the helper confirmed; the self-test injects one zero move and passes
#[test]
fn delivery_is_per_device_and_the_self_test_goes_through_the_helper() {
    let record = Record::new();
    let sink = sink(&record);
    let c = Arc::new(HidCounters::default());
    let (a, b) = ([0, 1, 2, 3, 4, 5], [0, 1, 2, 3, 4, 6]);
    sink.pass(None, &[key(0x2A, false), key(0x2A, true)], &c);
    sink.pass(Some(a), &[key(0x1E, false), key(0x1E, true)], &c);
    record.wait("the inputs", |l| l.len() == 4);
    wait_for("A delivered", || sink.delivered(&a).is_some());
    // Record mode never asks Windows
    assert_eq!(sink.delivered(&a).unwrap().windows_saw, None);
    assert_eq!(sink.delivered(&b), None);
    let result = sink.self_test(Duration::from_secs(5));
    assert_eq!(result, input_helper::client::SelfTest::Passed { windows_saw: None });
    assert_eq!(record.lines()[4..], ["move 0 0"]);
    // The test input is nobody's delivery
    assert_eq!(sink.delivered(&b), None);
    sink.stop();
}

/// This test binary run again as a fake core (only with BC_FAKE_CORE set):
/// starts a recording helper, holds two keys, then waits to be killed
#[test]
fn fake_core() {
    let Ok(number) = std::env::var("BC_FAKE_CORE") else { return };
    let sink = HelperSink::new(
        HelperConfig { exe: PathBuf::from(EXE), record: Some(number.parse().unwrap()) },
        Arc::new(SessionLauncher),
    );
    sink.start();
    wait_for("the helper connects", || sink.connected());
    let c = Arc::new(HidCounters::default());
    sink.pass(None, &[key(0x1D, false), key(0x20, false)], &c);
    println!("helper pid {}", sink.helper_pid().unwrap());
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn the_helper_releases_and_exits_when_its_core_dies() {
    let record = Record::new();
    let mut core = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fake_core", "--nocapture", "--test-threads=1"])
        .env("BC_FAKE_CORE", record.number.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let out = core.stdout.take().unwrap();
    std::thread::spawn(move || {
        for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            // libtest's "test fake_core ... " may come first on the line
            if let Some((_, p)) = l.split_once("helper pid ") {
                let _ = tx.send(p.trim().parse::<u32>().unwrap());
            }
        }
    });
    let Ok(helper) = rx.recv_timeout(Duration::from_secs(30)) else {
        let _ = core.kill();
        let out = core.wait_with_output().unwrap();
        panic!("the fake core did not start its helper: {}", String::from_utf8_lossy(&out.stderr));
    };
    record.wait("the downs", |l| l.len() == 2);
    core.kill().unwrap();
    let _ = core.wait();
    let lines = record.wait("the helper's exit", |l| l.last().is_some_and(|x| x.starts_with("exit")));
    assert_eq!(lines[2..], ["key 0x20 up", "key 0x1d up", "exit 0 released 2"]);
    wait_for("the helper process ends", || !process_runs(helper));
}

fn process_runs(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

/// The exit code of a child, polled without blocking the runtime (10 s)
async fn exit_code(child: &mut std::process::Child) -> Option<i32> {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Some(s) = child.try_wait().unwrap() {
            return s.code();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = child.kill();
    None
}

fn pipe_name() -> String {
    format!("{}{}", input_helper::helper::PIPE_PREFIX, win_sys::pipe_peer::random_hex(16).unwrap())
}

/// The helper started by a process other than the one serving the pipe
/// (here through cmd.exe): it refuses to take input from it
#[test]
fn a_process_other_than_its_parent_cannot_feed_the_helper() {
    let name = pipe_name();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let server = win_sys::pipe::create_server(&name, true).unwrap();
        let mut cmd = Command::new("cmd")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/c", EXE, input_helper::HELPER_ARG, &name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        // It connects, sees who serves the pipe, and leaves
        tokio::time::timeout(Duration::from_secs(10), server.connect()).await.unwrap().unwrap();
        assert_eq!(exit_code(&mut cmd).await, Some(input_helper::helper::EXIT_REFUSED));
    });
}

/// The core's side refuses a helper that is not the process it started:
/// a launcher whose process is cmd.exe, which starts the real helper. Both
/// sides refuse (the helper: not its parent; the core: another process).
#[test]
fn the_core_takes_only_the_process_it_started() {
    struct ThroughCmd;
    struct Child(win_sys::session_launch::Child);
    impl HelperProcess for Child {
        fn pid(&self) -> u32 {
            self.0.pid()
        }
        fn exited(&self) -> bool {
            HelperProcess::exited(&self.0)
        }
        fn kill(&self) {
            HelperProcess::kill(&self.0)
        }
    }
    impl Launcher for ThroughCmd {
        fn session(&self) -> Option<u32> {
            Some(1)
        }
        fn launch(&self, exe: &Path, args: Vec<String>, _: u32) -> std::io::Result<Box<dyn HelperProcess>> {
            let mut all = vec!["/c".to_string(), exe.display().to_string()];
            all.extend(args);
            let l = win_sys::session_launch::Launch {
                exe: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                args: all,
                session: 1,
                identity: win_sys::session_launch::Identity::User,
            };
            Ok(Box::new(Child(win_sys::session_launch::launch(&l)?)))
        }
    }
    let record = Record::new();
    let sink =
        HelperSink::new(HelperConfig { exe: PathBuf::from(EXE), record: Some(record.number) }, Arc::new(ThroughCmd));
    sink.start();
    let c = Arc::new(HidCounters::default());
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        assert!(!sink.connected());
        sink.pass(None, &[key(0x1E, false), key(0x1E, true)], &c);
        std::thread::sleep(Duration::from_millis(50));
    }
    sink.stop();
    assert!(sink.started() >= 1);
    assert!(c.dropped.load(Ordering::Relaxed) > 0);
    assert!(record.lines().iter().all(|l| l.starts_with("exit")), "{:?}", record.lines());
}

/// This test as the core, by hand: a frame that breaks the protocol ends
/// the helper, which releases what it holds first
#[test]
fn a_broken_frame_ends_the_helper_after_it_releases() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let record = Record::new();
    let name = pipe_name();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut server = win_sys::pipe::create_server(&name, true).unwrap();
        let mut helper = Command::new(EXE)
            .args([input_helper::HELPER_ARG, &name, input_helper::RECORD_ARG, &record.number.to_string()])
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), server.connect()).await.unwrap().unwrap();
        let hello = protocol::read_frame(&mut server).await.unwrap().unwrap();
        assert!(matches!(protocol::decode_to_core(&hello), Ok(ToCore::Hello { version: protocol::VERSION, .. })));
        let down = protocol::encode_to_helper(&ToHelper::Inject(vec![key(0x1E, false)]));
        server.write_all(&down).await.unwrap();
        // Read until the ack of that key
        loop {
            let body = protocol::read_frame(&mut server).await.unwrap().unwrap();
            if matches!(protocol::decode_to_core(&body), Ok(ToCore::Ack { processed: 1, .. })) {
                break;
            }
        }
        // A record with an unknown type
        let mut bad = down.clone();
        bad[4] = 0x77;
        server.write_all(&bad).await.unwrap();
        assert_eq!(exit_code(&mut helper).await, Some(input_helper::helper::EXIT_PROTOCOL));
        let mut rest = Vec::new();
        let _ = server.read_to_end(&mut rest).await;
    });
    let lines = record.wait("the exit", |l| l.last().is_some_and(|x| x.starts_with("exit")));
    assert_eq!(lines, ["key 0x1e down", "key 0x1e up", "exit 4 released 1"]);
}

/// A length above the protocol's bound: refused before anything is read
#[test]
fn an_oversized_frame_ends_the_helper() {
    use tokio::io::AsyncWriteExt;
    let record = Record::new();
    let name = pipe_name();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut server = win_sys::pipe::create_server(&name, true).unwrap();
        let mut helper = Command::new(EXE)
            .args([input_helper::HELPER_ARG, &name, input_helper::RECORD_ARG, &record.number.to_string()])
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), server.connect()).await.unwrap().unwrap();
        let _hello = protocol::read_frame(&mut server).await.unwrap().unwrap();
        server.write_all(&((protocol::MAX_BODY + 1) as u16).to_le_bytes()).await.unwrap();
        server.write_all(&vec![1u8; 4096]).await.unwrap_or(());
        assert_eq!(exit_code(&mut helper).await, Some(input_helper::helper::EXIT_PROTOCOL));
    });
    record.wait("the exit", |l| l == ["exit 4 released 0"]);
}
