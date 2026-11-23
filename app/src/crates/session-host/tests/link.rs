//! The agent link over a real named pipe, with the agent's side running on
//! a thread of the test (the real agent process: app/src/core/tests/
//! session_agent.rs). Each test has its own pipe name.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio_capture::{CaptureOptions, EndReason, Format, SampleFormat, Sink, Source};
use session_host::render::{RenderHost, RenderTarget};
use session_host::{AgentOptions, HostError, Hub, HubConfig, RemoteHost};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn pipe() -> String {
    let sid = win_sys::pipe::current_user_sid().unwrap();
    let suffix = format!("-linktest-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    session_host::pipe_name_for(&sid, &suffix)
}

fn hub(pipe: &str) -> Arc<Hub> {
    let mut config = HubConfig::new(pipe.to_string(), None);
    config.request_timeout = Duration::from_secs(5);
    Hub::start(config).unwrap()
}

/// An agent on a thread; `child`: it exits when the core goes away
fn agent(pipe: &str, child: bool) -> JoinHandle<i32> {
    let options = AgentOptions { pipe_name: pipe.to_string(), child, service: false };
    std::thread::spawn(move || session_host::run_agent(&options))
}

const WAIT: Duration = Duration::from_secs(5);

fn default_output(host: &RemoteHost) -> Option<String> {
    session_host::devices::DevicesHost::default_render_device(host)
}

#[test]
fn an_agent_started_first_connects_when_the_core_comes() {
    let name = pipe();
    let agent = agent(&name, false);
    std::thread::sleep(Duration::from_millis(400));
    let hub = hub(&name);
    let info = hub.wait_agent(WAIT).expect("the waiting agent connected");
    assert_eq!(info.generation, 1);
    assert_eq!(info.session_id, win_sys::pipe::process_session_id(std::process::id()).unwrap());
    let host = RemoteHost::new(hub.clone());
    assert_eq!(default_output(&host), audio_capture::default_render_device());
    // Told to exit, it does
    assert!(hub.tell_agent_to_exit());
    assert_eq!(agent.join().unwrap(), 0);
    assert!(hub.wait_no_agent(WAIT));
    hub.shutdown();
}

#[test]
fn a_lost_agent_is_replaced_and_the_next_request_works() {
    let name = pipe();
    let hub = hub(&name);
    let host = RemoteHost::new(hub.clone());
    let a = agent(&name, false);
    assert_eq!(hub.wait_agent(WAIT).unwrap().generation, 1);
    assert_eq!(default_output(&host), audio_capture::default_render_device());
    // The agent goes while idle
    hub.tell_agent_to_exit();
    assert_eq!(a.join().unwrap(), 0);
    assert!(hub.wait_no_agent(WAIT));
    // No agent: a request waits for one, and fails without
    let started = Instant::now();
    let err = hub.request_on("devices", serde_json::json!({"op": "default_render_device"}), Duration::from_millis(300));
    assert_eq!(err.unwrap_err(), HostError::NoAgent);
    assert!(started.elapsed() >= Duration::from_millis(300));
    // A request made before the next agent comes is served by it
    let late = {
        let hub = hub.clone();
        std::thread::spawn(move || hub.request("devices", serde_json::json!({"op": "default_render_device"})))
    };
    std::thread::sleep(Duration::from_millis(200));
    let b = agent(&name, true);
    assert!(late.join().unwrap().is_ok());
    assert_eq!(hub.agent().unwrap().generation, 2);
    assert_eq!(default_output(&host), audio_capture::default_render_device());
    // A child agent exits with its core
    hub.shutdown();
    assert_eq!(b.join().unwrap(), 0);
}

#[test]
fn a_second_agent_waits_until_the_first_goes() {
    let name = pipe();
    let hub = hub(&name);
    let first = agent(&name, false);
    let first_info = hub.wait_agent(WAIT).unwrap();
    // A child agent turned away as busy exits; a hand-started one waits
    let busy_child = agent(&name, true);
    assert_eq!(busy_child.join().unwrap(), 0);
    let second = agent(&name, false);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(hub.agent().unwrap(), first_info);
    hub.tell_agent_to_exit();
    assert_eq!(first.join().unwrap(), 0);
    let deadline = Instant::now() + WAIT;
    while hub.agent().is_none_or(|a| a.generation == first_info.generation) {
        assert!(Instant::now() < deadline, "the second agent did not take over");
        std::thread::sleep(Duration::from_millis(20));
    }
    hub.tell_agent_to_exit();
    assert_eq!(second.join().unwrap(), 0);
    hub.shutdown();
}

/// A hand-made agent: connects, says `hello`, returns the core's answer
/// and the pipe
async fn raw_agent(
    pipe: &str,
    hello: session_host::wire::Hello,
) -> (session_host::wire::Frame, tokio::net::windows::named_pipe::NamedPipeClient) {
    use session_host::wire::Frame;
    let mut client = loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe) {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    };
    ipc::write_frame(&mut client, &Frame::Hello(hello)).await.unwrap();
    let answer = ipc::read_frame::<_, Frame>(&mut client).await.unwrap().unwrap();
    (answer, client)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn my_hello() -> session_host::wire::Hello {
    session_host::wire::Hello {
        protocol: session_host::PROTOCOL_VERSION,
        pid: std::process::id(),
        session_id: win_sys::pipe::process_session_id(std::process::id()).unwrap(),
    }
}

#[test]
fn events_and_requests_from_the_agent_reach_the_core() {
    use session_host::wire::{Frame, WireError};
    let name = pipe();
    let hub = hub(&name);
    let (tx, events) = std::sync::mpsc::channel();
    hub.on_event("news", move |body| {
        let _ = tx.send(body);
    });
    hub.serve("ask", |body| match body.as_u64() {
        Some(n) => Ok(serde_json::json!(n * 2)),
        None => Err(WireError::new(session_host::ErrorCode::BadRequest, "a number")),
    });
    rt().block_on(async {
        let (answer, mut c) = raw_agent(&name, my_hello()).await;
        assert_eq!(answer, Frame::Welcome { protocol: session_host::PROTOCOL_VERSION });
        for seq in 1..=3 {
            let e = Frame::Event { seq, group: "news".into(), body: serde_json::json!(seq) };
            ipc::write_frame(&mut c, &e).await.unwrap();
        }
        // The agent's requests, numbered by the agent, answered by id
        for (id, body) in [(1, serde_json::json!(21)), (2, serde_json::json!("x"))] {
            ipc::write_frame(&mut c, &Frame::Request { id, group: "ask".into(), body }).await.unwrap();
        }
        ipc::write_frame(&mut c, &Frame::Request { id: 3, group: "nobody".into(), body: serde_json::Value::Null })
            .await
            .unwrap();
        let mut replies = std::collections::HashMap::new();
        while replies.len() < 3 {
            match ipc::read_frame::<_, Frame>(&mut c).await.unwrap().unwrap() {
                Frame::Reply { id, result } => {
                    replies.insert(id, result);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(replies[&1], Ok(serde_json::json!(42)));
        assert_eq!(replies[&2].as_ref().unwrap_err().code, session_host::ErrorCode::BadRequest);
        assert_eq!(replies[&3].as_ref().unwrap_err().code, session_host::ErrorCode::UnknownGroup);
    });
    let got: Vec<_> = (0..3).map(|_| events.recv_timeout(WAIT).unwrap()).collect();
    assert_eq!(got, [serde_json::json!(1), serde_json::json!(2), serde_json::json!(3)]);
    hub.shutdown();
}

#[test]
fn a_client_that_is_not_the_agent_it_names_is_refused() {
    use session_host::wire::{Frame, RejectReason};
    let name = pipe();
    let hub = hub(&name);
    rt().block_on(async {
        let mut lying = my_hello();
        lying.pid += 4;
        let (answer, _) = raw_agent(&name, lying).await;
        assert_eq!(answer, Frame::Reject { reason: RejectReason::Unauthorized });
        let mut old = my_hello();
        old.protocol += 1;
        let (answer, _) = raw_agent(&name, old).await;
        assert_eq!(answer, Frame::Reject { reason: RejectReason::Protocol });
    });
    assert!(hub.agent().is_none());
    hub.shutdown();
}

#[test]
fn only_one_core_serves_a_pipe() {
    let name = pipe();
    let hub = hub(&name);
    assert!(Hub::start(HubConfig::new(name.clone(), None)).is_err());
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
fn two_captures_run_at_once_and_end_with_their_agent() {
    use session_host::capture::CaptureHost;
    let name = pipe();
    let hub = hub(&name);
    let host = RemoteHost::new(hub.clone());
    let a = agent(&name, false);
    hub.wait_agent(WAIT).unwrap();
    let options = |rate| CaptureOptions { preferred_rate: Some(rate), fill_silence: false };
    let music = host.open_capture(&Source::TestTone, &options(48_000)).unwrap();
    let voice = host.open_capture(&Source::TestTone, &options(16_000)).unwrap();
    assert_eq!(music.format(), Format { sample_rate: 48_000, channels: 2, sample_format: SampleFormat::F32 });
    assert_eq!(voice.format().sample_rate, 16_000);
    let (m, v) = (Arc::new(Mutex::new(Counted::default())), Arc::new(Mutex::new(Counted::default())));
    let music = music.start(Box::new(CountSink(m.clone()))).unwrap();
    let voice = voice.start(Box::new(CountSink(v.clone()))).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(!music.ended() && !voice.ended());
    let (mf, vf) = (m.lock().unwrap().frames, v.lock().unwrap().frames);
    // About 0.6 s of each, in 10 ms blocks of its own rate
    assert!(mf >= 480 * 40 && mf.is_multiple_of(480), "{mf}");
    assert!(vf >= 160 * 40 && vf.is_multiple_of(160), "{vf}");
    // The agent goes: both end as lost, and a new capture needs a new agent
    hub.tell_agent_to_exit();
    assert_eq!(a.join().unwrap(), 0);
    let deadline = Instant::now() + WAIT;
    while !(music.ended() && voice.ended()) {
        assert!(Instant::now() < deadline, "the captures did not end");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(music.end_reason(), Some(EndReason::AgentLost));
    assert_eq!(voice.end_reason(), Some(EndReason::AgentLost));
    drop((music, voice));
    let b = agent(&name, false);
    let again = host.open_capture(&Source::TestTone, &options(48_000)).unwrap();
    let again = again.start(Box::new(CountSink(Arc::new(Mutex::new(Counted::default()))))).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(again.frames() > 0 && !again.ended());
    drop(again);
    hub.tell_agent_to_exit();
    assert_eq!(b.join().unwrap(), 0);
    hub.shutdown();
}

#[test]
fn pcm_rendered_through_the_agent_arrives_intact() {
    let name = pipe();
    let hub = hub(&name);
    let host = RemoteHost::new(hub.clone());
    let a = agent(&name, false);
    let format = Format { sample_rate: 16_000, channels: 1, sample_format: SampleFormat::I16 };
    let mut ch = host.open_render(format, RenderTarget::Discard).unwrap();
    let mut blocks = Vec::new();
    for i in 0..100u32 {
        let block: Vec<u8> = (0..320u32).map(|b| (b * 7 + i) as u8).collect();
        assert!(ch.push(&block));
        blocks.extend_from_slice(&block);
    }
    assert!(ch.push_silence(160));
    let deadline = Instant::now() + WAIT;
    let stats = loop {
        let s = ch.stats().unwrap();
        if s.records == 101 {
            break s;
        }
        assert!(Instant::now() < deadline, "{s:?}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((stats.frames, stats.silence_frames), (100 * 160, 160));
    // The same bytes in the same order: the local renderer's checksum
    let mut local = session_host::LocalHost.open_render(format, RenderTarget::Discard).unwrap();
    assert!(local.push(&blocks));
    let deadline = Instant::now() + WAIT;
    while local.stats().unwrap().records == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(local.stats().unwrap().checksum, stats.checksum);
    // The agent goes: the channel is lost, pushing fails without blocking
    hub.tell_agent_to_exit();
    assert_eq!(a.join().unwrap(), 0);
    let deadline = Instant::now() + WAIT;
    while !ch.lost() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!ch.push(&[0; 320]));
    assert_eq!(ch.stats().unwrap_err(), HostError::AgentLost);
    hub.shutdown();
}
