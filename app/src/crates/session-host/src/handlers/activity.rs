//! Group `activity` in the agent: looks for processes and watches them
//! until they exit, and watches whether an application records from a
//! capture side, on one thread ([`watch`]) that sends the core an event
//! when that changes. Dropping the handler ends the watches.
//!
//! The Windows calls are behind [`Backend`]; the tests use a fake one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::host::activity::{Event, GROUP, Request};
use crate::wire::{ErrorCode, WireError, decode, reply, to_body};
use crate::{AgentContext, Handler};

/// The watcher's step
const STEP: Duration = Duration::from_millis(10);
/// Watched processes are looked at this often
const PROCESS_EVERY: Duration = Duration::from_millis(100);

/// Whether an application records (bound to the thread that opened it)
pub(crate) trait Recording {
    fn active(&self) -> bool;
}

pub(crate) trait Backend: Send + Sync + 'static {
    fn find_process(&self, exe: &str) -> Option<u32>;
    fn process_exited(&self, pid: u32) -> bool;
    fn mic_capture_active(&self, device: &str) -> Result<Option<bool>, audio_capture::CaptureError>;
    fn mic_recorders(&self, device: &str) -> Vec<String>;
    /// Ok(None): no capture side. The message goes to the core's log: no
    /// device IDs.
    fn open_recording(&self, device: &str) -> Result<Option<Box<dyn Recording>>, String>;
}

struct Windows;

struct WinRecording {
    // Dropped before the apartment
    sessions: win_sys::audio_policy::CaptureSessions,
    _com: win_sys::audio_policy::ComApartment,
}

impl Recording for WinRecording {
    fn active(&self) -> bool {
        self.sessions.active()
    }
}

impl Backend for Windows {
    fn find_process(&self, exe: &str) -> Option<u32> {
        audio_capture::find_process(exe)
    }

    fn process_exited(&self, pid: u32) -> bool {
        audio_capture::process_exited(pid)
    }

    fn mic_capture_active(&self, device: &str) -> Result<Option<bool>, audio_capture::CaptureError> {
        audio_capture::mic_capture_active(device)
    }

    fn mic_recorders(&self, device: &str) -> Vec<String> {
        audio_capture::mic_recorders(device)
    }

    fn open_recording(&self, device: &str) -> Result<Option<Box<dyn Recording>>, String> {
        use win_sys::audio_policy::{CaptureSessions, ComApartment};
        let com = ComApartment::init_mta().map_err(|e| format!("no COM (HRESULT {:08x})", e.code().0))?;
        match CaptureSessions::open(device) {
            Ok(Some(sessions)) => Ok(Some(Box::new(WinRecording { sessions, _com: com }))),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("the capture sessions cannot be read (HRESULT {:08x})", e.code().0)),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Sink = Arc<dyn Fn(&Event) + Send + Sync>;

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    let sink: Sink = Arc::new(move |e: &Event| {
        ctx.event(GROUP, to_body(e));
    });
    sink(&Event::Ready);
    Box::new(Activity::new(Windows, sink))
}

struct Watched {
    device: String,
    every: Duration,
    /// Opened by the watcher; None until then
    last: Option<bool>,
}

#[derive(Default)]
struct State {
    processes: HashSet<u32>,
    recordings: HashMap<u64, Watched>,
}

struct Shared<B> {
    backend: B,
    sink: Sink,
    state: Mutex<State>,
    stop: AtomicBool,
}

struct Activity<B: Backend> {
    shared: Arc<Shared<B>>,
    watcher: Option<JoinHandle<()>>,
}

impl<B: Backend> Activity<B> {
    fn new(backend: B, sink: Sink) -> Activity<B> {
        let shared = Arc::new(Shared { backend, sink, state: Mutex::default(), stop: AtomicBool::new(false) });
        Activity { shared, watcher: None }
    }

    fn watching(&mut self) {
        if self.watcher.is_some() {
            return;
        }
        let shared = self.shared.clone();
        match std::thread::Builder::new().name("agent-activity-watch".into()).spawn(move || watch(&shared)) {
            Ok(t) => self.watcher = Some(t),
            Err(e) => tracing::warn!("activity: no watcher: {e}"),
        }
    }
}

impl<B: Backend> Handler for Activity<B> {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        let backend = &self.shared.backend;
        match decode(body)? {
            Request::FindProcess { exe } => reply(backend.find_process(&exe)),
            Request::WatchProcess { pid } => {
                let exited = backend.process_exited(pid);
                if !exited {
                    lock(&self.shared.state).processes.insert(pid);
                    self.watching();
                }
                reply(exited)
            }
            Request::MicCaptureActive { device } => match backend.mic_capture_active(&device) {
                Ok(active) => reply(active),
                Err(e) => Err(WireError::failed("capture sessions", &audio_capture::CaptureWireError::from(&e))),
            },
            Request::MicRecorders { device } => reply(backend.mic_recorders(&device)),
            Request::WatchRecording { watch, device, every_ms } => {
                let now = match backend.open_recording(&device) {
                    Ok(Some(r)) => r.active(),
                    Ok(None) => return reply(None::<bool>),
                    Err(e) => return Err(WireError::new(ErrorCode::Failed, e)),
                };
                let every = Duration::from_millis(u64::from(every_ms.max(1)));
                lock(&self.shared.state).recordings.insert(watch, Watched { device, every, last: Some(now) });
                self.watching();
                reply(Some(now))
            }
            Request::Unwatch { watch } => {
                lock(&self.shared.state).recordings.remove(&watch);
                reply(())
            }
        }
    }
}

impl<B: Backend> Drop for Activity<B> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.watcher.take() {
            let _ = t.join();
        }
    }
}

/// The watcher: processes every [`PROCESS_EVERY`], each recording watch at
/// its own pace. Its capture sessions stay open here (COM objects of this
/// thread).
fn watch<B: Backend>(shared: &Shared<B>) {
    let mut open: HashMap<u64, (Box<dyn Recording>, Instant)> = HashMap::new();
    let mut failed: HashMap<u64, Instant> = HashMap::new();
    let mut processes_at = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        std::thread::sleep(STEP);
        let now = Instant::now();
        if now.duration_since(processes_at) >= PROCESS_EVERY {
            processes_at = now;
            let pids: Vec<u32> = lock(&shared.state).processes.iter().copied().collect();
            for pid in pids {
                if shared.backend.process_exited(pid) {
                    lock(&shared.state).processes.remove(&pid);
                    (shared.sink)(&Event::Exited { pid });
                }
            }
        }
        let mut state = lock(&shared.state);
        open.retain(|w, _| state.recordings.contains_key(w));
        failed.retain(|w, at| state.recordings.contains_key(w) && at.elapsed() < Duration::from_secs(1));
        for (watch, w) in state.recordings.iter_mut() {
            if !open.contains_key(watch) {
                if failed.contains_key(watch) {
                    continue;
                }
                match shared.backend.open_recording(&w.device) {
                    Ok(Some(r)) => {
                        open.insert(*watch, (r, now));
                    }
                    _ => {
                        failed.insert(*watch, now);
                        continue;
                    }
                }
            }
            let Some((r, looked)) = open.get_mut(watch) else { continue };
            if now.duration_since(*looked) < w.every {
                continue;
            }
            *looked = now;
            let active = r.active();
            if w.last != Some(active) {
                w.last = Some(active);
                (shared.sink)(&Event::Recording { watch: *watch, active });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        running: Mutex<HashSet<u32>>,
        /// Device -> someone records (absent: no capture side)
        recording: Arc<Mutex<HashMap<String, bool>>>,
    }

    struct FakeRecording(Arc<Mutex<HashMap<String, bool>>>, String);

    impl Recording for FakeRecording {
        fn active(&self) -> bool {
            lock(&self.0).get(&self.1).copied().unwrap_or(false)
        }
    }

    impl Backend for Arc<Fake> {
        fn find_process(&self, exe: &str) -> Option<u32> {
            (exe == "app.exe").then_some(40).filter(|p| lock(&self.running).contains(p))
        }
        fn process_exited(&self, pid: u32) -> bool {
            !lock(&self.running).contains(&pid)
        }
        fn mic_capture_active(&self, device: &str) -> Result<Option<bool>, audio_capture::CaptureError> {
            Ok(lock(&self.recording).get(device).copied())
        }
        fn mic_recorders(&self, device: &str) -> Vec<String> {
            if lock(&self.recording).get(device) == Some(&true) { vec!["rec.exe".into()] } else { Vec::new() }
        }
        fn open_recording(&self, device: &str) -> Result<Option<Box<dyn Recording>>, String> {
            Ok(lock(&self.recording)
                .contains_key(device)
                .then(|| Box::new(FakeRecording(self.recording.clone(), device.to_string())) as Box<dyn Recording>))
        }
    }

    fn handler(fake: &Arc<Fake>) -> (Activity<Arc<Fake>>, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        (Activity::new(fake.clone(), Arc::new(move |ev: &Event| lock(&e).push(ev.clone()))), events)
    }

    fn ask<T: serde::de::DeserializeOwned>(h: &mut impl Handler, r: Request) -> T {
        serde_json::from_value(h.request(to_body(&r)).unwrap()).unwrap()
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_watched_process_is_reported_when_it_exits() {
        let fake = Arc::new(Fake::default());
        let (mut h, events) = handler(&fake);
        assert_eq!(ask::<Option<u32>>(&mut h, Request::FindProcess { exe: "app.exe".into() }), None);
        lock(&fake.running).insert(40);
        assert_eq!(ask::<Option<u32>>(&mut h, Request::FindProcess { exe: "app.exe".into() }), Some(40));
        assert!(!ask::<bool>(&mut h, Request::WatchProcess { pid: 40 }));
        assert!(ask::<bool>(&mut h, Request::WatchProcess { pid: 41 }), "no such process: exited");
        lock(&fake.running).remove(&40);
        wait_for("the exit", || lock(&events).contains(&Event::Exited { pid: 40 }));
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(lock(&events).len(), 1, "reported once");
    }

    #[test]
    fn a_recording_watch_reports_starts_and_ends() {
        let fake = Arc::new(Fake::default());
        lock(&fake.recording).insert("cable".into(), false);
        let (mut h, events) = handler(&fake);
        let none: Option<bool> = ask(&mut h, Request::WatchRecording { watch: 1, device: "x".into(), every_ms: 20 });
        assert_eq!(none, None);
        let now: Option<bool> = ask(&mut h, Request::WatchRecording { watch: 2, device: "cable".into(), every_ms: 20 });
        assert_eq!(now, Some(false));
        lock(&fake.recording).insert("cable".into(), true);
        wait_for("the start", || lock(&events).contains(&Event::Recording { watch: 2, active: true }));
        assert_eq!(ask::<Vec<String>>(&mut h, Request::MicRecorders { device: "cable".into() }), ["rec.exe"]);
        assert_eq!(ask::<Option<bool>>(&mut h, Request::MicCaptureActive { device: "cable".into() }), Some(true));
        lock(&fake.recording).insert("cable".into(), false);
        wait_for("the end", || lock(&events).contains(&Event::Recording { watch: 2, active: false }));
        // Unwatched: no more events
        ask::<()>(&mut h, Request::Unwatch { watch: 2 });
        lock(&events).clear();
        lock(&fake.recording).insert("cable".into(), true);
        std::thread::sleep(Duration::from_millis(100));
        assert!(lock(&events).is_empty());
    }

    #[test]
    fn the_real_handler_answers_as_windows() {
        let mut h = new(AgentContext::detached());
        let exited: bool = serde_json::from_value(
            h.request(serde_json::json!({"op": "watch_process", "pid": std::process::id()})).unwrap(),
        )
        .unwrap();
        assert!(!exited);
        assert_eq!(h.request(serde_json::json!({"op": "find_process", "exe": ""})).unwrap(), Value::Null);
        assert!(
            h.request(serde_json::json!({"op": "mic_capture_active", "device": "{0.0.0.00000000}.{none}"})).is_err()
        );
    }
}
