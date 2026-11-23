//! What the signed-in user gets from a core in service mode, through the
//! real program:
//!
//! - The session agent (`bluecaster-core.exe --agent`, this test as its
//!   core) writes a file the core hands over into the user's folder (a
//!   temp folder, BLUECASTER_CONFIG_DIR) under the fixed name, and refuses
//!   one that is too large or not of its kind.
//! - `--headless --test-shell --service-data <temp>`: the snapshot says
//!   service mode and names no protected folder, carries what the UI needs
//!   for its verbose logging, refuses app.quit, and app.close closes the UI
//!   and the tray while the core keeps running; the UI's next connection
//!   brings the tray back.
//!
//! Each test has its own instance. Only processes these tests start are
//! ended; nothing outside the temp folders is written.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use session_host::handover::{CHUNK, HandoverHost, Kind};
use session_host::{Hub, HubConfig, RemoteHost};

const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");
const WAIT: Duration = Duration::from_secs(40);

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bluecaster-user-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Ends a process this test started, also when an assertion fails
struct Started(Child);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn zip(body: usize) -> Vec<u8> {
    let mut z = b"PK\x03\x04".to_vec();
    z.extend((0..body).map(|i| (i % 251) as u8));
    z.extend(b"PK\x05\x06");
    z.extend([0u8; 18]);
    z
}

fn files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

#[test]
fn a_file_handed_over_lands_in_the_users_folder() {
    let inst = format!("handover-{}", std::process::id());
    let user = temp("handover");
    let sid = win_sys::pipe::current_user_sid().unwrap();
    let pipe = session_host::pipe_name_for(&sid, &format!("-{inst}"));
    let hub = Hub::start(HubConfig::new(pipe, None)).unwrap();
    let _agent = Started(
        Command::new(EXE)
            .arg("--agent")
            .env("BLUECASTER_INSTANCE", &inst)
            .env("BLUECASTER_CONFIG_DIR", &user)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the agent starts"),
    );
    hub.wait_agent(WAIT).expect("the agent connects");
    let host = RemoteHost::new(hub.clone());

    // A bundle of three pieces
    let data = zip(2 * CHUNK + 5);
    let path = PathBuf::from(host.hand_over(Kind::Bundle, &data).expect("the agent takes the bundle"));
    assert_eq!(path.parent(), Some(user.join("logs").join("bundles").as_path()));
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stamp = name.strip_prefix("bluecaster-diag-").and_then(|n| n.strip_suffix(".zip")).expect(&name);
    assert!(stamp.len() == 15 && stamp.as_bytes()[8] == b'_', "{name}");
    assert_eq!(std::fs::read(&path).unwrap(), data);
    let report = br#"{"summary":"8 of 8 commands answered"}"#;
    let probe = PathBuf::from(host.hand_over(Kind::ProbeReport, report).unwrap());
    assert_eq!(probe.parent(), Some(user.join("logs").join("probe").as_path()));
    assert_eq!(std::fs::read(&probe).unwrap(), report);

    // Too large: refused before anything is written
    let big = vec![b' '; (Kind::ProbeReport.max_bytes() + 1) as usize];
    assert!(matches!(host.hand_over(Kind::ProbeReport, &big), Err(session_host::HostError::Remote(_))));
    // Not of its kind: a program as a bundle, a zip as a report
    let mut program = b"MZ".to_vec();
    program.extend(vec![0u8; 4096]);
    assert!(host.hand_over(Kind::Bundle, &program).is_err());
    assert!(host.hand_over(Kind::ScriptReport, &data).is_err());
    let mut bundles = files(&user.join("logs").join("bundles"));
    bundles.sort();
    assert_eq!(bundles, [name], "only the bundle that was taken, no part files");
    assert_eq!(files(&user.join("logs").join("probe")).len(), 1);

    hub.tell_agent_to_exit();
    hub.shutdown();
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&user);
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

fn wait_log(dir: &Path, what: impl Fn(&str) -> bool, core: &mut Child) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let log = log_text(dir);
        if what(&log) {
            return log;
        }
        assert!(core.try_wait().unwrap().is_none(), "the core exited:\n{log}");
        assert!(Instant::now() < deadline, "timed out:\n{log}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The test shell's tray lines: how many menu items each model had
fn tray_items(log: &str) -> Vec<usize> {
    log.lines()
        .filter_map(|l| l.split(" with ").nth(1).filter(|_| l.contains("shell (test): tray model")))
        .filter_map(|rest| rest.split(' ').next()?.parse().ok())
        .collect()
}

struct Client {
    rd: tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    wr: tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    next: u64,
}

impl Client {
    async fn connect(pipe: &str, name: &str) -> (Client, Value) {
        let deadline = Instant::now() + WAIT;
        let p = loop {
            match win_sys::pipe_peer::open_client(pipe) {
                Ok(p) => break p,
                Err(e) => {
                    assert!(Instant::now() < deadline, "the core's pipe: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        let (rd, mut wr) = tokio::io::split(p);
        let hello = ipc::ClientMessage::Hello { protocol: ipc::PROTOCOL_VERSION, client: name.into() };
        ipc::write_frame(&mut wr, &hello).await.unwrap();
        let mut c = Client { rd, wr, next: 1 };
        assert_eq!(c.frame().await["type"], "welcome");
        let snapshot = c.frame().await;
        assert_eq!(snapshot["type"], "snapshot");
        (c, snapshot["state"].clone())
    }

    async fn frame(&mut self) -> Value {
        ipc::read_frame(&mut self.rd).await.unwrap().expect("the core closed the pipe")
    }

    /// The response's error code ("ok": none); `events` gets the event
    /// kinds that came meanwhile
    async fn ask(&mut self, method: &str, params: Option<Value>, events: &mut Vec<String>) -> String {
        let id = self.next;
        self.next += 1;
        let mut m = json!({"type": "request", "id": id, "method": method});
        if let Some(p) = params {
            m["params"] = p;
        }
        ipc::write_frame(&mut self.wr, &m).await.unwrap();
        loop {
            let f = self.frame().await;
            if f["type"] == "event" {
                events.push(f["event"]["kind"].as_str().unwrap_or("").to_string());
            }
            if f["type"] == "response" && f["id"] == id {
                return f["error"]["code"].as_str().unwrap_or("ok").to_string();
            }
        }
    }

    /// Reads until an event of `kind`
    async fn event(&mut self, kind: &str) -> Value {
        let read = async {
            loop {
                let f = self.frame().await;
                if f["type"] == "event" && f["event"]["kind"] == kind {
                    return f["event"].clone();
                }
            }
        };
        tokio::time::timeout(WAIT, read).await.expect(kind)
    }
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "--test-shell exists in debug builds only")]
fn the_service_core_closes_for_the_user_and_keeps_running() {
    let inst = format!("svcuser-{}", std::process::id());
    let user = temp("user");
    let svc = temp("svc");
    // No update check from a test
    std::fs::write(svc.join("settings.json"), r#"{"check_updates_on_startup": false}"#).unwrap();
    let mut core = Started(
        Command::new(EXE)
            .args(["--headless", "--test-shell", "--service-data"])
            .arg(&svc)
            .env("BLUECASTER_INSTANCE", &inst)
            .env("BLUECASTER_CONFIG_DIR", &user)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the headless core starts"),
    );
    let pipe = format!(r"\\.\pipe\bluecaster-service-{inst}");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (mut c, state) = Client::connect(&pipe, "service-user-test").await;
        // Service mode, and no protected folder named
        assert_eq!(state["service_mode"], true);
        assert_eq!(state["config_dir"], "");
        assert_eq!(state["log_dir"], "");
        // The UI's verbose logging: both switches are in the snapshot
        assert!(state.get("verbose").is_some_and(Value::is_null), "{}", state["verbose"]);
        assert!(state["settings"]["developer_mode"].is_boolean());
        assert!(state["settings"]["dev_log"].get("always_verbose").is_some());
        let mut events = Vec::new();
        assert_eq!(c.ask("diag.set_verbose", Some(json!({"level": "debug"})), &mut events).await, "ok");
        let v = if events.iter().any(|e| e == "verbose") { None } else { Some(c.event("verbose").await) };
        if let Some(v) = v {
            assert_eq!(v["value"]["level"], "debug");
        }
        // The tray is the agent's; wait until it shows the menu
        wait_log(&svc, |l| tray_items(l).last().is_some_and(|&n| n > 0), &mut core.0);
        // Windows stops a service: app.quit is refused, app.close is not
        assert_eq!(c.ask("app.quit", None, &mut events).await, ipc::access::NOT_IN_SERVICE);
        assert_eq!(c.ask("app.quit_for_update", None, &mut events).await, ipc::access::NOT_IN_SERVICE);
        events.clear();
        assert_eq!(c.ask("app.close", None, &mut events).await, "ok");
        if !events.iter().any(|e| e == "ui_close") {
            c.event("ui_close").await;
        }
        // The tray icon goes (a model without items); the core stays
        let log = wait_log(&svc, |l| tray_items(l).last() == Some(&0), &mut core.0);
        assert!(log.contains("exit: streaming stops"), "{log}");
        std::thread::sleep(Duration::from_millis(500));
        assert!(core.0.try_wait().unwrap().is_none(), "the core exited");
        // A client that is not the UI does not bring it back...
        let (mut other, _) = Client::connect(&pipe, "service-user-test").await;
        assert_eq!(other.ask("state.snapshot", None, &mut events).await, "ok");
        assert_eq!(tray_items(&log_text(&svc)).last(), Some(&0));
        // ...BlueCaster's window does
        let (mut ui, _) = Client::connect(&pipe, &format!("bluecaster-ui/{}", env!("CARGO_PKG_VERSION"))).await;
        assert_eq!(ui.ask("state.snapshot", None, &mut events).await, "ok");
        let log = wait_log(&svc, |l| tray_items(l).last().is_some_and(|&n| n > 0), &mut core.0);
        assert!(log.contains("tray: shown again"), "{log}");
        // The service's log names no folder of the user (its own folder
        // still appears in a few older lines: %ProgramData% names no one)
        assert!(!log.contains(&*user.to_string_lossy()), "{log}");
    });
    drop(core);
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(&svc);
}
