//! Stage 4 as ordinary processes: the agent the service mode starts and
//! accepts, its PCM channels passed as duplicated handles, the console's
//! session as the service sees it (a test file stands for Windows' session
//! changes), and the move of the user's data when the first agent
//! connects. Each test uses its own instance and temp folders; only
//! processes these tests start are ended. Nothing here registers or starts
//! a Windows service.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use audio_capture::{CaptureOptions, Format, Sink, Source};
use session_host::capture::CaptureHost;
use session_host::render::{RenderHost, RenderTarget};
use session_host::{AgentCheck, Hub, HubConfig, LaunchConfig, RemoteHost};

const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");
const WAIT: Duration = Duration::from_secs(40);

static SEQ: AtomicU32 = AtomicU32::new(0);

fn instance(what: &str) -> String {
    format!("svc{what}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
}

fn own_session() -> u32 {
    win_sys::pipe::process_session_id(std::process::id()).unwrap()
}

/// The program's path as Windows reports a process's image
fn exe_path() -> PathBuf {
    PathBuf::from(EXE.replace('/', "\\"))
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The hub's own log lines in this test process
fn hub_lines() -> &'static Mutex<Vec<String>> {
    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LINES.get_or_init(|| {
        use tracing_subscriber::layer::SubscriberExt;
        struct Collect;
        struct Msg(String);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Collect {
            fn on_event(&self, e: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
                let mut m = Msg(String::new());
                e.record(&mut m);
                hub_lines().lock().unwrap().push(m.0);
            }
        }
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Collect));
        Mutex::new(Vec::new())
    })
}

#[derive(Default)]
struct Counted {
    frames: usize,
}
struct CountSink(Arc<Mutex<Counted>>);
impl Sink for CountSink {
    fn audio(&mut self, _data: &[u8], frames: usize, _format: Format) {
        self.0.lock().unwrap().frames += frames;
    }
    fn silence(&mut self, _frames: usize, _format: Format) {}
}

/// A hub with service mode's agent check, this test as its core: the agent
/// it starts (the real program) is accepted, its test tone and a render
/// channel go through handles duplicated into it, and an agent started by
/// hand is refused
#[test]
fn the_started_agent_is_accepted_and_its_channels_are_handles() {
    let lines = hub_lines();
    let inst = instance("hub");
    let sid = win_sys::pipe::current_user_sid().unwrap();
    let pipe = session_host::pipe_name_for(&sid, &format!("-{inst}"));
    let launch = LaunchConfig {
        exe: EXE.into(),
        args: vec!["--agent".into(), "--child".into()],
        env: vec![("BLUECASTER_INSTANCE".into(), inst.clone())],
    };
    let check = AgentCheck { exe: exe_path(), core_session: own_session() };
    let hub = Hub::start(HubConfig { agent_check: Some(check), ..HubConfig::new(pipe, Some(launch)) }).unwrap();
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the started agent is accepted");
    assert_eq!(info.session_id, own_session());

    let capture = host
        .open_capture(&Source::TestTone, &CaptureOptions { preferred_rate: Some(48_000), fill_silence: false })
        .unwrap();
    let counted = Arc::new(Mutex::new(Counted::default()));
    let running = capture.start(Box::new(CountSink(counted.clone()))).unwrap();
    wait_for("the test tone through duplicated handles", || counted.lock().unwrap().frames >= 48_000 / 2);
    let format = Format { sample_rate: 16_000, channels: 1, sample_format: audio_capture::SampleFormat::I16 };
    let mut render = host.open_render(format, RenderTarget::Discard).unwrap();
    for _ in 0..10 {
        assert!(render.push(&[3; 320]));
    }
    wait_for("the render channel's records", || render.stats().is_ok_and(|s| s.records == 10));
    let handles = lines.lock().unwrap().iter().filter(|l| l.ends_with("passed as handles")).count();
    assert_eq!(handles, 2, "both channels went as handles");
    drop((running, render));

    // An agent started by anyone else is refused (exit code 4), even
    // with the right program and instance
    let mut by_hand = Command::new(EXE)
        .args(["--agent", "--child"])
        .env("BLUECASTER_INSTANCE", &inst)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    let code = loop {
        if let Some(s) = by_hand.try_wait().unwrap() {
            break s.code();
        }
        assert!(Instant::now() < deadline, "the agent started by hand was not refused");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(4));
    assert_eq!(hub.agent().map(|a| a.pid), Some(info.pid), "the started agent stays");
    hub.shutdown();
    wait_for("the child agent's exit", || win_sys::process::ProcessWatch::open(info.pid).is_none_or(|w| w.exited()));
}

fn log_text(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "log") {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut text = String::new();
    walk(&root.join("logs"), &mut text);
    text
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bluecaster-svcsess-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

struct Started(Child);
impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn agent_pid(log: &str, nth: usize) -> Option<u32> {
    log.split("session agent: started (process ")
        .nth(nth)
        .and_then(|r| r.split(')').next())
        .and_then(|n| n.parse().ok())
}

/// The UI pipe of service mode answers: the core runs (without an agent)
fn ui_answers(inst: &str) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let pipe = format!(r"\\.\pipe\bluecaster-service-{inst}");
        let Ok(p) = win_sys::pipe_peer::open_client(&pipe) else { return false };
        let (mut rd, mut wr) = tokio::io::split(p);
        let hello = ipc::ClientMessage::Hello { protocol: ipc::PROTOCOL_VERSION, client: "session-test".into() };
        if ipc::write_frame(&mut wr, &hello).await.is_err() {
            return false;
        }
        let welcome: Option<serde_json::Value> = tokio::time::timeout(Duration::from_secs(5), ipc::read_frame(&mut rd))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        welcome.is_some_and(|w| w["type"] == "welcome")
    })
}

/// `--headless --service-data <temp>` with the console's session from a
/// test file: nobody signed in (no agent, the core serves its UI pipe),
/// a sign-in (the agent starts on the service's pipe and is accepted, the
/// user's data moves in and replaces the defaults the core wrote, and is
/// read again), and the user gone again (the agent is ended)
#[test]
#[cfg_attr(not(debug_assertions), ignore = "the test switches exist in debug builds only")]
fn a_service_core_waits_for_a_sign_in_then_takes_the_users_data() {
    let inst = instance("sess");
    let user = temp("user");
    let service = temp("service").join("BlueCaster");
    let sessions = temp("sessions");
    std::fs::create_dir_all(user.join("devices")).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    let file = sessions.join("console.txt");
    std::fs::write(&file, "none").unwrap();
    std::fs::write(user.join("settings.json"), r#"{"check_updates_on_startup": false, "language": "de"}"#).unwrap();
    std::fs::write(user.join("devices").join("devices.txt"), "00:A2:D0:00:00:02|Headphones\n").unwrap();

    let core = Started(
        Command::new(EXE)
            .args(["--headless", "--test-shell", "--service-data"])
            .arg(&service)
            .env("BLUECASTER_INSTANCE", &inst)
            .env("BLUECASTER_CONFIG_DIR", &user)
            .env("BLUECASTER_TEST_SESSIONS", &file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // Nobody signed in: the core runs, serves its UI, starts no agent
    wait_for("the sign-in screen", || log_text(&service).contains("the agent's session is now none"));
    wait_for("the UI pipe", || ui_answers(&inst));
    std::thread::sleep(Duration::from_secs(2));
    let log = log_text(&service);
    assert!(!log.contains("session agent: started"), "{log}");
    assert!(service.join("migration_pending.json").is_file());
    assert!(!service.join("migration.json").exists());
    // The core saved no settings by itself at its start (the automatic
    // update check would, and that save could replace the user's settings
    // a moment after they move in)
    if let Ok(bytes) = std::fs::read(service.join("settings.json")) {
        let s: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(s["last_update_check"], "", "the service checks for no updates by itself");
    }

    // A sign-in at the console
    std::fs::write(&file, own_session().to_string()).unwrap();
    wait_for("the move", || log_text(&service).contains("the moved data read again"));
    let log = log_text(&service);
    assert!(log.contains("session agent: connected (process"), "{log}");
    assert!(log.contains("migration: 2 files taken from the user's folder"), "{log}");
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(service.join("settings.json")).unwrap()).unwrap();
    assert_eq!(s["language"], "de", "the user's settings replaced the core's defaults");
    assert!(service.join("migration.json").is_file() && !service.join("migration_pending.json").exists());
    let pid = agent_pid(&log, 1).expect("the agent's process");

    // The user is gone: the agent is ended, the core keeps running
    std::fs::write(&file, "none").unwrap();
    wait_for("the agent's end", || win_sys::process::ProcessWatch::open(pid).is_none_or(|w| w.exited()));
    assert!(log_text(&service).contains("ending the agent of the previous session"));
    assert!(ui_answers(&inst), "the core runs on without an agent");
    // No address or key in the log
    assert!(!log_text(&service).contains("00:A2:D0:00:00:02"));

    drop(core);
    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(service.parent().unwrap());
    let _ = std::fs::remove_dir_all(&sessions);
}

/// `--service` outside the service control manager: refused at once, no
/// folder or log written (nothing is registered or started)
#[test]
fn the_service_entry_refuses_an_ordinary_start() {
    let status = Command::new(EXE)
        .arg("--service")
        .env("BLUECASTER_INSTANCE", instance("entry"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1063));
}
