//! The session agent as the real program (`bluecaster-core.exe --agent`),
//! with this test as its core: the pipe, the hello, the devices group, PCM
//! both ways, an agent killed and replaced, the agent's log in the core's.
//! Each test uses its own instance (BLUECASTER_INSTANCE), so its own pipe.
//! Only processes these tests start are ended.

use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use audio_capture::{CaptureOptions, EndReason, Format, SampleFormat, Sink, Source};
use session_host::capture::CaptureHost;
use session_host::devices::DevicesHost;
use session_host::render::{RenderHost, RenderTarget};
use session_host::{Hub, HubConfig, LaunchConfig, RemoteHost};

/// Console programs started from a test get no terminal window
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");
const WAIT: Duration = Duration::from_secs(10);

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A fresh instance name and its agent pipe
fn instance() -> (String, String) {
    let name = format!("agenttest-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let sid = win_sys::pipe::current_user_sid().unwrap();
    let pipe = session_host::pipe_name_for(&sid, &format!("-{name}"));
    (name, pipe)
}

/// An agent started by hand (it waits for a core, and again after one)
fn agent(instance: &str) -> Child {
    Command::new(EXE)
        .arg("--agent")
        .env("BLUECASTER_INSTANCE", instance)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the agent starts")
}

fn end(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Ends a process the hub started (it is this test's)
fn kill_pid(pid: u32) {
    let status = Command::new("taskkill")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

fn hub(pipe: &str, launch_instance: Option<&str>) -> Arc<Hub> {
    let launch = launch_instance.map(|i| LaunchConfig {
        exe: EXE.into(),
        args: vec!["--agent".into(), "--child".into()],
        env: vec![("BLUECASTER_INSTANCE".into(), i.into())],
    });
    Hub::start(HubConfig::new(pipe.to_string(), launch)).unwrap()
}

/// The agent's log lines as this test's "core" logs them
fn agent_lines() -> &'static Mutex<Vec<String>> {
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
                if e.metadata().target() == "session_agent" {
                    let mut m = Msg(String::new());
                    e.record(&mut m);
                    agent_lines().lock().unwrap().push(format!("{} {}", e.metadata().level(), m.0));
                }
            }
        }
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Collect));
        Mutex::new(Vec::new())
    })
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_agent_started_first_serves_devices_and_is_replaced_when_killed() {
    let (inst, pipe) = instance();
    let first = agent(&inst);
    std::thread::sleep(Duration::from_millis(500));
    let hub = hub(&pipe, None);
    let info = hub.wait_agent(WAIT).expect("the waiting agent connects");
    assert_eq!(info.pid, first.id());
    let host = RemoteHost::new(hub.clone());
    // The devices group, as this process sees Windows itself
    let local = audio_capture::render_devices().map(|l| l.into_iter().map(|d| d.id).collect::<Vec<_>>());
    let remote = host.render_devices().map(|l| l.into_iter().map(|d| d.id).collect::<Vec<_>>());
    assert_eq!(remote.ok(), local.ok());
    assert_eq!(host.default_render_device(), audio_capture::default_render_device());
    assert_eq!(host.mix_format(None), audio_capture::mix_format(None));
    assert_eq!(host.mix_format(Some("no such device")), None);
    // Killed while idle: the hub notices, the next agent serves the next request
    end(first);
    assert!(hub.wait_no_agent(WAIT));
    let second = agent(&inst);
    assert_eq!(host.default_render_device(), audio_capture::default_render_device());
    let info = hub.agent().unwrap();
    assert_eq!((info.pid, info.generation), (second.id(), 2));
    hub.shutdown();
    // A hand-started agent outlives its core and waits for the next one
    std::thread::sleep(Duration::from_millis(500));
    let mut second = second;
    assert!(second.try_wait().unwrap().is_none(), "the agent exited with its core");
    let hub = self::hub(&pipe, None);
    assert_eq!(hub.wait_agent(WAIT).unwrap().pid, second.id());
    hub.tell_agent_to_exit();
    let deadline = Instant::now() + WAIT;
    while second.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the agent did not exit when told");
        std::thread::sleep(Duration::from_millis(20));
    }
    hub.shutdown();
}

#[derive(Default)]
struct Counted {
    frames: u64,
}

struct CountSink(Arc<Mutex<Counted>>);

impl Sink for CountSink {
    fn audio(&mut self, _data: &[u8], frames: usize, _format: Format) {
        self.0.lock().unwrap().frames += frames as u64;
    }
    fn silence(&mut self, _frames: usize, _format: Format) {}
}

#[test]
fn the_core_starts_its_agent_and_a_new_one_after_a_kill() {
    let lines = agent_lines();
    let (inst, pipe) = instance();
    let hub = hub(&pipe, Some(&inst));
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the hub starts an agent");
    // Two captures at once: the stream's and a microphone uplink's
    let opts = |rate| CaptureOptions { preferred_rate: Some(rate), fill_silence: false };
    let music = host.open_capture(&Source::TestTone, &opts(48_000)).unwrap();
    let voice = host.open_capture(&Source::TestTone, &opts(16_000)).unwrap();
    let (m, v) = (Arc::new(Mutex::new(Counted::default())), Arc::new(Mutex::new(Counted::default())));
    let music = music.start(Box::new(CountSink(m.clone()))).unwrap();
    let voice = voice.start(Box::new(CountSink(v.clone()))).unwrap();
    // And PCM the other way
    let format = Format { sample_rate: 16_000, channels: 1, sample_format: SampleFormat::I16 };
    let mut render = host.open_render(format, RenderTarget::Discard).unwrap();
    for _ in 0..20 {
        assert!(render.push(&[7; 320]));
    }
    wait_for("the render channel's records", || render.stats().is_ok_and(|s| s.records == 20));
    std::thread::sleep(Duration::from_millis(500));
    let (mf, vf) = (m.lock().unwrap().frames, v.lock().unwrap().frames);
    assert!(mf >= 480 * 30 && mf.is_multiple_of(480), "{mf}");
    assert!(vf >= 160 * 30 && vf.is_multiple_of(160), "{vf}");
    // The agent's log reached this "core", marked as the agent's
    wait_for("the agent's log", || {
        lines.lock().unwrap().iter().any(|l| l == "INFO [agent] capture agent: channel opened (2 open)")
    });
    // Killed mid-capture: both captures end as lost, the render channel too
    kill_pid(info.pid);
    wait_for("the captures' end", || music.ended() && voice.ended());
    assert_eq!(music.end_reason(), Some(EndReason::AgentLost));
    assert_eq!(voice.end_reason(), Some(EndReason::AgentLost));
    wait_for("the render channel lost", || render.lost());
    assert!(!render.push(&[0; 320]));
    drop((music, voice, render));
    // The hub starts the next agent; a capture opened again runs
    let again = host.open_capture(&Source::TestTone, &opts(48_000)).unwrap();
    let next = hub.agent().unwrap();
    assert!(next.generation == info.generation + 1 && next.pid != info.pid, "{next:?}");
    let again = again.start(Box::new(CountSink(Arc::new(Mutex::new(Counted::default()))))).unwrap();
    wait_for("audio from the new agent", || again.frames() > 0);
    drop(again);
    // Its child agent exits with the core
    hub.shutdown();
    let deadline = Instant::now() + WAIT;
    while win_sys::process::ProcessWatch::open(next.pid).is_some_and(|w| !w.exited()) {
        assert!(Instant::now() < deadline, "the child agent outlived its core");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The samples of a mono 16-bit WAV file this test's agent wrote
fn wav_samples(path: &std::path::Path) -> Vec<i16> {
    let data = std::fs::read(path).unwrap();
    assert!(data.len() >= 44 && &data[..4] == b"RIFF", "not a WAV file");
    data[44..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

#[test]
fn the_headset_microphone_is_written_by_the_agent_and_reopened_after_a_kill() {
    let (inst, pipe) = instance();
    let hub = hub(&pipe, Some(&inst));
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the hub starts an agent");
    let dir = std::env::temp_dir().join(format!("bc-voice-agent-{inst}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mic.wav");
    let target = RenderTarget::Wav { path: path.clone() };
    // Every sample arrives, in order, and the file is complete at the end
    let mut out = host.open_voice(&target, 16_000).unwrap();
    assert!(out.in_agent() && out.stats().is_none());
    let mut p = out.producer().unwrap();
    let ramp: Vec<i16> = (1..=8000).collect();
    for block in ramp.chunks(120) {
        assert_eq!(p.push(block), block.len());
        std::thread::sleep(Duration::from_millis(7));
    }
    assert_eq!(out.ended(), None);
    drop(out);
    assert_eq!(wav_samples(&path), ramp);
    // A missing endpoint: the renderer's own error, as in this process
    let id = "{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}";
    let missing = RenderTarget::Endpoint { device_id: id.into(), prefill_ms: 40 };
    let err = host.open_voice(&missing, 16_000).unwrap_err();
    assert_eq!(err, format!("audio output device not found: {id}"));
    // Killed while the microphone runs: the next agent writes it again
    let mut out = host.open_voice(&target, 16_000).unwrap();
    let mut p = out.producer().unwrap();
    let mut next = 1i16;
    let mut push_for = |ms: u64| {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            let block: Vec<i16> = (0..120).map(|i| next.wrapping_add(i)).collect();
            next = next.wrapping_add(120);
            // Never waits, agent or none
            let started = Instant::now();
            p.push(&block);
            assert!(started.elapsed() < Duration::from_millis(5));
            std::thread::sleep(Duration::from_millis(7));
        }
    };
    push_for(500);
    kill_pid(info.pid);
    push_for(300);
    wait_for("the next agent", || hub.agent().is_some_and(|a| a.generation == info.generation + 1));
    push_for(2000);
    assert_eq!(out.ended(), None);
    drop(out);
    // The new agent's file: at least a second of the ramp, in order
    let tail = wav_samples(&path);
    assert!(tail.len() >= 16_000, "{} samples", tail.len());
    assert!(tail.windows(2).all(|w| w[1] == w[0].wrapping_add(1)), "samples out of order");
    assert!(tail[0] > 120 * 60, "the new file starts with the old agent's samples");
    let _ = std::fs::remove_dir_all(&dir);
    hub.shutdown();
}

/// LE Audio's media control and the media keys through the agent, as a
/// dry run: the agent runs the simulated player and only logs the keys,
/// so nothing on this PC is read, controlled or pressed
#[test]
fn the_media_session_and_media_keys_go_through_the_agent() {
    use session_host::media::{Key, MediaHost};
    const OP_PLAY: u8 = 0x01;
    const RESULT_SUCCESS: u8 = 0x01;
    const RESULT_MEDIA_PLAYER_INACTIVE: u8 = 0x03;
    const PLAYING: u8 = 0x01;
    const PAUSED: u8 = 0x02;
    const PLAYER: &str = "BlueCaster test player";
    let lines = agent_lines();
    let logged = |line: &str| lines.lock().unwrap().iter().filter(|l| *l == line).count();
    let (inst, pipe) = instance();
    let hub = hub(&pipe, Some(&inst));
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the hub starts an agent");
    let mut media = host.media_backend(true).expect("a backend with the agent");
    assert!(media.dry_run());
    let changes = Arc::new(AtomicU32::new(0));
    let c = changes.clone();
    media.start(Box::new(move || {
        c.fetch_add(1, Ordering::Relaxed);
    }));
    wait_for("the agent's player", || media.info().player == PLAYER);
    assert_eq!(media.info().state, PAUSED);
    // A command returns at once; its result and the new state come later
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    media.command(OP_PLAY, 0, Box::new(move |r| tx.send(r).unwrap()));
    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), RESULT_SUCCESS);
    wait_for("playing", || media.info().state == PLAYING);
    wait_for("the agent's dry run", || logged("INFO [agent] MCP: dry run: PLAY (nothing sent to Windows)") == 1);
    // A media key: an event to the agent, only logged in a dry run
    let started = Instant::now();
    host.send_media_key(Key::PlayPause, true);
    assert!(started.elapsed() < Duration::from_millis(50));
    wait_for("the key in the agent", || logged("INFO [agent] media key PlayPause: dry run, not sent") == 1);
    // Agent killed: no player until the next agent
    kill_pid(info.pid);
    wait_for("no player", || !media.info().session);
    // The next agent watches again: the simulated player from its start
    wait_for("the next agent's player", || media.info().player == PLAYER);
    assert_eq!(hub.agent().unwrap().generation, info.generation + 1);
    assert_eq!(media.info().state, PAUSED);
    let (tx, rx) = std::sync::mpsc::channel();
    media.command(OP_PLAY, 0, Box::new(move |r| tx.send(r).unwrap()));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), RESULT_SUCCESS);
    wait_for("the second dry run", || logged("INFO [agent] MCP: dry run: PLAY (nothing sent to Windows)") == 2);
    assert!(changes.load(Ordering::Relaxed) >= 4);
    // Stopped: returns at once, the agent's watch ends, no player
    let started = Instant::now();
    media.stop();
    assert!(started.elapsed() < Duration::from_millis(50));
    assert!(!media.info().session);
    wait_for("the watch's end", || logged("INFO [agent] media: the watch ended") == 1);
    let (tx, rx) = std::sync::mpsc::channel();
    media.command(OP_PLAY, 0, Box::new(move |r| tx.send(r).unwrap()));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), RESULT_MEDIA_PLAYER_INACTIVE);
    hub.shutdown();
}

#[test]
fn the_default_input_app_list_and_endpoint_watch_go_through_the_agent() {
    use audio_capture::EndpointRole;
    use session_host::default_device::DefaultDeviceHost;
    use session_host::endpoints::EndpointsHost;
    let lines = agent_lines();
    let logged = |line: &str| lines.lock().unwrap().iter().filter(|l| *l == line).count();
    let (inst, pipe) = instance();
    let hub = hub(&pipe, Some(&inst));
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the hub starts an agent");
    // Read-only: the default input and a recording side as this process sees them
    for role in EndpointRole::ALL {
        assert_eq!(host.default_capture_device(role), audio_capture::default_capture_device(role));
    }
    if let Some(d) = audio_capture::render_devices().ok().and_then(|l| l.into_iter().next()) {
        assert_eq!(host.paired_capture_device(&d.id), audio_capture::paired_capture_device(&d.id));
    }
    // The app list (sessions come and go meanwhile: only its shape)
    let apps = host.audio_apps();
    assert!(apps.iter().all(|a| a.process_id != std::process::id() && !a.exe.is_empty()));
    // The watch: through this agent, and again through the next one
    let changes = Arc::new(AtomicU32::new(0));
    let c = changes.clone();
    let watch = host
        .watch_endpoints(Box::new(move || {
            c.fetch_add(1, Ordering::Relaxed);
        }))
        .unwrap();
    wait_for("the agent's watch", || logged("INFO [agent] audio endpoints: watching") == 1);
    kill_pid(info.pid);
    wait_for("the next agent's watch", || logged("INFO [agent] audio endpoints: watching") == 2);
    assert_eq!(hub.agent().unwrap().generation, info.generation + 1);
    // Taken as a change: one may have come while no agent watched
    wait_for("a change after the next agent", || changes.load(Ordering::Relaxed) >= 1);
    drop(watch);
    wait_for("the watch's end", || logged("INFO [agent] audio endpoints: no longer watched") == 1);
    hub.shutdown();
}

/// The groups of a headless core's shell through a test agent (`--agent
/// --test-shell`: Run values in memory, nothing run as administrator, a
/// tray that only logs): the Run values round trip, an elevated run is
/// refused, the shell asks for its tray and logs the one it gets
#[test]
#[cfg_attr(not(debug_assertions), ignore = "--test-shell exists in debug builds only")]
fn a_test_agent_keeps_run_values_in_memory_and_runs_nothing_elevated() {
    use session_host::autostart::AutostartHost;
    use session_host::elevation::{ElevatedError, ElevationHost};
    use session_host::shell::{ShellHost, ToCore, TrayItem, TrayModel};
    let lines = agent_lines();
    let (inst, pipe) = instance();
    let launch = LaunchConfig {
        exe: EXE.into(),
        args: vec!["--agent".into(), "--child".into(), "--test-shell".into()],
        env: vec![("BLUECASTER_INSTANCE".into(), inst.clone())],
    };
    let hub = Hub::start(HubConfig::new(pipe, Some(launch))).unwrap();
    let ready = Arc::new(AtomicU32::new(0));
    let r = ready.clone();
    hub.on_event(session_host::shell::GROUP, move |body| {
        if serde_json::from_value::<ToCore>(body).ok() == Some(ToCore::Ready) {
            r.fetch_add(1, Ordering::Relaxed);
        }
    });
    let host = RemoteHost::new(hub.clone());
    hub.wait_agent(WAIT).expect("the hub starts an agent");
    // Never the user's registry: the test agent's store starts empty
    assert_eq!(host.autostart_get("BlueCaster"), Ok(None));
    let core = r#""C:\BlueCaster\bluecaster-core.exe" --background"#;
    host.autostart_set("BlueCaster", core).unwrap();
    assert_eq!(host.autostart_get("BlueCaster").unwrap().as_deref(), Some(core));
    assert_eq!(host.autostart_remove("BlueCaster"), Ok(true));
    assert!(host.autostart_get("OneDrive").is_err(), "only BlueCaster's values");
    assert!(host.autostart_set("BlueCaster", r#""C:\x\other.exe""#).is_err(), "only the core's command line");
    let refused = host.driver_helper(&["list".to_string()]);
    assert!(matches!(refused, Err(ElevatedError::Failed(_))), "{refused:?}");
    // The shell: a new connection asks for its tray
    wait_for("the shell's ready", || ready.load(Ordering::Relaxed) == 1);
    let model = TrayModel {
        tooltip: "BlueCaster".into(),
        items: vec![TrayItem::Item { id: "exit".into(), label: "Exit".into(), enabled: true }],
    };
    assert!(host.show_tray(&model));
    wait_for("the test tray's model", || {
        lines.lock().unwrap().iter().any(|l| l == "INFO [agent] shell (test): tray model 1 with 1 menu item(s)")
    });
    hub.shutdown();
}

/// The text of a settings folder's log files
fn log_text(config: &std::path::Path) -> String {
    fn walk(dir: &std::path::Path, out: &mut String) {
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
    walk(&config.join("logs"), &mut text);
    text
}

/// `bluecaster-core.exe --headless` as a UI sees it: a test instance of its
/// own (BLUECASTER_INSTANCE, a temp settings folder), its child agent with
/// the test shell; the UI's pipe gives the snapshot, the agent shows the
/// tray, and `--quit` ends both
#[test]
#[cfg_attr(not(debug_assertions), ignore = "--test-shell exists in debug builds only")]
fn a_headless_core_serves_the_ui_and_its_agent_shows_the_tray() {
    let inst = format!("headless-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let config = std::env::temp_dir().join(format!("bluecaster-{inst}"));
    let _ = std::fs::remove_dir_all(&config);
    std::fs::create_dir_all(&config).unwrap();
    // No update check from a test
    std::fs::write(config.join("settings.json"), r#"{"check_updates_on_startup": false}"#).unwrap();
    let with_instance = |c: &mut Command| {
        c.env("BLUECASTER_INSTANCE", &inst)
            .env("BLUECASTER_CONFIG_DIR", &config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    };
    let mut cmd = Command::new(EXE);
    cmd.args(["--headless", "--test-shell"]);
    with_instance(&mut cmd);
    let mut core = cmd.spawn().expect("the headless core starts");
    let sid = win_sys::pipe::current_user_sid().unwrap();
    let pipe = format!(r"\\.\pipe\bluecaster-{sid}-{inst}");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (welcome, snapshot) = rt.block_on(async {
        use tokio::net::windows::named_pipe::ClientOptions;
        let deadline = Instant::now() + WAIT;
        let mut client = loop {
            match ClientOptions::new().open(&pipe) {
                Ok(c) => break c,
                Err(e) => {
                    assert!(Instant::now() < deadline, "the core's pipe: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        let hello = ipc::ClientMessage::Hello { protocol: ipc::PROTOCOL_VERSION, client: "headless-test".into() };
        ipc::write_frame(&mut client, &hello).await.unwrap();
        let welcome: serde_json::Value = ipc::read_frame(&mut client).await.unwrap().unwrap();
        let snapshot: serde_json::Value = ipc::read_frame(&mut client).await.unwrap().unwrap();
        (welcome, snapshot)
    });
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["state"]["config_dir"].as_str(), Some(config.display().to_string().as_str()));
    // A second core of the instance exits at once
    let mut again = Command::new(EXE);
    again.args(["--headless", "--test-shell"]);
    with_instance(&mut again);
    assert_eq!(again.status().unwrap().code(), Some(0));
    // The agent asked for the tray and got the core's
    wait_for("the agent's tray", || {
        let log = log_text(&config);
        log.contains("tray: a session agent shows it now") && log.contains("[agent] shell (test): tray model")
    });
    let log = log_text(&config);
    let agent_pid: u32 = log
        .split("session agent: started (process ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.parse().ok())
        .expect("the agent's process in the log");
    // --quit of this instance (as the installer does): both exit
    let mut quit = Command::new(EXE);
    quit.arg("--quit");
    with_instance(&mut quit);
    assert_eq!(quit.status().unwrap().code(), Some(0));
    let deadline = Instant::now() + WAIT;
    let code = loop {
        if let Some(s) = core.try_wait().unwrap() {
            break s.code();
        }
        assert!(Instant::now() < deadline, "the headless core did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(code, Some(0));
    while win_sys::process::ProcessWatch::open(agent_pid).is_some_and(|w| !w.exited()) {
        assert!(Instant::now() < deadline, "the child agent outlived its core");
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = log_text(&config);
    assert!(log.contains("[agent] shell: tray shown for the core"), "{log}");
    let _ = std::fs::remove_dir_all(&config);
}

/// The volume and activity groups share one mirror per process: their
/// tests run one at a time
static VOLUME_TESTS: Mutex<()> = Mutex::new(());

#[test]
fn volume_and_the_process_watch_go_through_the_agent_read_only() {
    use session_host::activity::ActivityHost;
    use session_host::volume::VolumeHost;
    let _one = VOLUME_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (inst, pipe) = instance();
    let hub = hub(&pipe, Some(&inst));
    let host = RemoteHost::new(hub.clone());
    let info = hub.wait_agent(WAIT).expect("the hub starts an agent");
    // Read only: this PC's volume and mute stay as they are
    let default = audio_capture::default_render_device();
    assert_eq!(host.volume_default_output(), default);
    assert!(host.host_keeps_mute());
    assert!(host.open_output_volume("no such device").is_err());
    let volume = default.as_deref().map(|id| {
        let v = host.open_output_volume(id).unwrap();
        let direct = audio_capture::EndpointVolume::open(Some(id)).unwrap();
        assert_eq!(v.device_id(), id);
        assert!(v.keeps_mute());
        assert_eq!(v.muted(), direct.muted().ok());
        let (a, b) = (v.volume().unwrap(), direct.volume().unwrap());
        assert!((a - b).abs() < 0.01, "{a} {b}");
        v
    });
    let _ = host.mixer_apps();
    // This test's own process, found by its name, runs
    let me = std::env::current_exe().unwrap();
    let exe = me.file_name().unwrap().to_str().unwrap();
    assert_eq!(host.find_process(exe), Some(std::process::id()));
    assert_eq!(host.find_process("no-such-app-bluecaster.exe"), None);
    assert!(!host.process_exited(std::process::id()));
    // A process watched until it exits: the agent's event
    let mut child = Command::new("cmd")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/c", "ping", "-n", "30", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(!host.process_exited(child.id()));
    let _ = child.kill();
    let _ = child.wait();
    wait_for("the exit reported", || host.process_exited(child.id()));
    // The recording watch answers as Windows does
    if let Some(id) = &default {
        let watch = host.watch_recording(id, Duration::from_millis(50));
        match audio_capture::mic_capture_active(id) {
            Ok(Some(_)) => assert!(matches!(watch, Ok(Some(_))), "{watch:?}"),
            Ok(None) => assert!(matches!(watch, Ok(None)), "{watch:?}"),
            Err(_) => {}
        }
    }
    // Killed: a new agent gets the watches again
    let mut other = Command::new("cmd")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/c", "ping", "-n", "30", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(!host.process_exited(other.id()));
    kill_pid(info.pid);
    wait_for("the next agent", || hub.agent().is_some_and(|a| a.generation > info.generation));
    if let (Some(v), Some(id)) = (&volume, &default) {
        wait_for("the volume watched again", || v.volume().is_some());
        wait_for("the default watched again", || host.volume_default_output().as_deref() == Some(id.as_str()));
    }
    let _ = other.kill();
    let _ = other.wait();
    wait_for("the exit reported by the new agent", || host.process_exited(other.id()));
    drop(volume);
    hub.shutdown();
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the fake volume exists in debug builds only")]
fn the_agent_holds_the_output_mute_and_gives_it_up_with_its_core() {
    use session_host::volume::VolumeHost;
    let _one = VOLUME_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    // A fake output in the agent: this PC's mute is never touched
    let fake_agent = |instance: &str| {
        Command::new(EXE)
            .arg("--agent")
            .env("BLUECASTER_INSTANCE", instance)
            .env("BLUECASTER_TEST_FAKE_VOLUME", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the agent starts")
    };
    const FAKE: &str = "fake-output";
    let (inst, pipe) = instance();
    let first = fake_agent(&inst);
    let hub = hub(&pipe, None);
    hub.wait_agent(WAIT).expect("the agent connects");
    let host = RemoteHost::new(hub.clone());
    assert_eq!(host.volume_default_output().as_deref(), Some(FAKE));
    let v = host.open_output_volume(FAKE).unwrap();
    assert_eq!((v.volume(), v.muted()), (Some(0.5), Some(false)));
    assert!(v.set_volume(0.25));
    assert_eq!(v.volume(), Some(0.25));
    // Held muted, let go of, held again
    assert!(v.set_muted(true));
    wait_for("muted", || v.muted() == Some(true));
    assert!(v.set_muted(false));
    wait_for("unmuted", || v.muted() == Some(false));
    assert!(v.set_muted(true));
    wait_for("muted again", || v.muted() == Some(true));
    // The agent dies while it holds the mute: the next one mutes again
    end(first);
    assert!(hub.wait_no_agent(WAIT));
    let second = fake_agent(&inst);
    hub.wait_agent(WAIT).expect("the next agent connects");
    wait_for("the mute held by the new agent", || v.muted() == Some(true));
    assert_eq!(v.volume(), Some(0.5), "a new agent's fake output");
    // The core goes away holding it: the agent unmutes
    drop(v);
    hub.shutdown();
    let hub = self::hub(&pipe, None);
    hub.wait_agent(WAIT).expect("the agent serves the next core");
    let host = RemoteHost::new(hub.clone());
    let v = host.open_output_volume(FAKE).unwrap();
    assert_eq!(v.muted(), Some(false), "unmuted when its core went away");
    drop(v);
    hub.tell_agent_to_exit();
    let mut second = second;
    let deadline = Instant::now() + WAIT;
    while second.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the agent did not exit when told");
        std::thread::sleep(Duration::from_millis(20));
    }
    hub.shutdown();
}
