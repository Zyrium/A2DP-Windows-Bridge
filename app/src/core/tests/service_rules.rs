//! The request rules of service mode through the real program:
//! `bluecaster-core.exe --headless --test-shell --service-data <temp>` with
//! a test instance of its own. An ordinary client (this test, not
//! elevated) reads the snapshot, changes an ordinary setting and asks for
//! the keyboard code (developer mode is off), and is refused developer mode
//! and a .wav microphone file. Without
//! service mode the same requests behave as before. Only processes this
//! test starts are ended; nothing outside the temp folders is written.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");
const WAIT: Duration = Duration::from_secs(40);
static SEQ: AtomicU32 = AtomicU32::new(0);

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bluecaster-rules-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(inst: &str, user: &Path, service: Option<&Path>) -> Child {
    let mut c = Command::new(EXE);
    c.args(["--headless", "--test-shell"]);
    if let Some(dir) = service {
        c.arg("--service-data").arg(dir);
    }
    c.env("BLUECASTER_INSTANCE", inst)
        .env("BLUECASTER_CONFIG_DIR", user)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the headless core starts")
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

/// Ends the core this test started, also when an assertion fails (its
/// child agent exits with the connection)
struct Started(Child);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A profile whose microphone output is a file
fn wav_profile() -> Value {
    json!({
        "name": "Test", "device_address": "00:A2:D0:00:00:31", "device_name": "Test Buds",
        "codec": "sbc", "quality": "auto", "abr": false, "codec_fallback": true,
        "sample_rate": 0, "bit_depth": 0, "capture_mode": "loopback", "audio_device_id": "",
        "audio_device_name": "", "auto_switch_device": false, "app_exe": "", "app_name": "",
        "volume_sync": "off", "volume_device_id": "", "volume_device_name": "", "max_media_payload": 0,
        "headset_mic": "on", "mic_device_id": r"C:\Windows\Temp\bluecaster-rules.wav",
    })
}

struct Client {
    rd: tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    wr: tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    next: u64,
}

impl Client {
    async fn connect(pipe: &str, service: bool) -> (Client, Value) {
        let deadline = Instant::now() + WAIT;
        let p = loop {
            let opened = if service {
                // As the UI opens it: without the right to serve the name
                win_sys::pipe_peer::open_client(pipe)
            } else {
                tokio::net::windows::named_pipe::ClientOptions::new().open(pipe)
            };
            match opened {
                Ok(p) => break p,
                Err(e) => {
                    assert!(Instant::now() < deadline, "the core's pipe: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        let (rd, mut wr) = tokio::io::split(p);
        let hello = ipc::ClientMessage::Hello { protocol: ipc::PROTOCOL_VERSION, client: "rules-test".into() };
        ipc::write_frame(&mut wr, &hello).await.unwrap();
        let mut c = Client { rd, wr, next: 1 };
        let welcome = c.frame().await;
        assert_eq!(welcome["type"], "welcome", "{welcome}");
        let snapshot = c.frame().await;
        assert_eq!(snapshot["type"], "snapshot");
        (c, snapshot["state"].clone())
    }

    async fn frame(&mut self) -> Value {
        ipc::read_frame(&mut self.rd).await.unwrap().expect("the core closed the pipe")
    }

    /// The error code, or "ok"
    async fn ask(&mut self, method: &str, params: Option<Value>) -> String {
        let id = self.next;
        self.next += 1;
        let mut m = json!({"type": "request", "id": id, "method": method});
        if let Some(p) = params {
            m["params"] = p;
        }
        ipc::write_frame(&mut self.wr, &m).await.unwrap();
        loop {
            let f = self.frame().await;
            if f["type"] == "response" && f["id"] == id {
                return f["error"]["code"].as_str().unwrap_or("ok").to_string();
            }
        }
    }
}

/// The answers to the same requests, with or without service mode
fn answers(service: bool) -> Vec<String> {
    let inst = format!("rules-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let user = temp(&format!("{inst}-user"));
    let svc = temp(&format!("{inst}-svc"));
    let data = if service { &svc } else { &user };
    // No update check from a test
    std::fs::write(data.join("settings.json"), r#"{"check_updates_on_startup": false}"#).unwrap();
    let core = Started(start(&inst, &user, service.then_some(svc.as_path())));
    let pipe = if service {
        format!(r"\\.\pipe\bluecaster-service-{inst}")
    } else {
        format!(r"\\.\pipe\bluecaster-{}-{inst}", win_sys::pipe::current_user_sid().unwrap())
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let out = rt.block_on(async {
        let (mut c, state) = Client::connect(&pipe, service).await;
        // Service mode names no protected folder
        let shown = if service { String::new() } else { data.display().to_string() };
        assert_eq!(state["config_dir"].as_str(), Some(shown.as_str()));
        assert_eq!(state["service_mode"], service);
        let mut settings = state["settings"].clone();
        settings["theme"] = json!(if settings["theme"] == "dark" { "light" } else { "dark" });
        let mut developer = settings.clone();
        developer["developer_mode"] = json!(true);
        vec![
            c.ask("state.snapshot", None).await,
            c.ask("settings.set", Some(json!({ "settings": settings }))).await,
            c.ask("hid.code", None).await,
            c.ask("settings.set", Some(json!({ "settings": developer }))).await,
            c.ask("profile.save", Some(json!({ "profile": wav_profile(), "original_name": null }))).await,
            c.ask("hid.status", None).await,
        ]
    });
    if service {
        // Its own child agent passes the service-mode check
        let deadline = Instant::now() + WAIT;
        loop {
            let log = log_text(&svc);
            let agent_refused = log.lines().any(|l| l.contains("session agent: process") && l.contains("refused"));
            assert!(!agent_refused, "{log}");
            if log.contains("session agent: connected (process") {
                break;
            }
            assert!(Instant::now() < deadline, "no agent connected:\n{log}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    drop(core);
    // The child agent exits with its core's connection; the folders go
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(&svc);
    out
}

#[test]
fn an_ordinary_client_of_the_service_is_refused_what_needs_an_administrator() {
    assert_eq!(
        answers(true),
        // hid.code is open (hid.enable asks the administrator): here it
        // fails only because developer mode is off
        ["ok", "ok", "developer.off", "ipc.needs_admin", "ipc.not_in_service", "ok"],
        "snapshot, theme, hid.code, developer mode, .wav file, hid.status"
    );
}

#[test]
fn without_service_mode_nothing_changes() {
    // hid.code: developer mode is off; with it on (the second settings.set)
    // the profile with a .wav output is saved as before
    assert_eq!(
        answers(false),
        ["ok", "ok", "developer.off", "ok", "ok", "ok"],
        "snapshot, theme, hid.code, developer mode, .wav file, hid.status"
    );
}
