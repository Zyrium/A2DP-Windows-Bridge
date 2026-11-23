//! Group `activity`: what the user's applications do that the core
//! follows. The Application source's process (looked for by its executable
//! name, watched until it exits) and whether an application records from
//! the headset microphone's output (Auto headset mode).
//!
//! The session thread asks [`ActivityHost::process_exited`] on every tick
//! and Auto's watcher reads the recording every 50 ms: through the agent
//! neither is a request per call. The first call for a process (or the
//! watch's start) is a request, the agent then watches and sends an event
//! when the process exits ([`Event::Exited`]) or the recording starts or
//! stops ([`Event::Recording`]), and the calls read the core's copy. A new
//! agent gets the watches again (its handler says [`Event::Ready`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use audio_capture::CaptureError;
use serde::{Deserialize, Serialize};

use crate::host::devices::capture_error;
use crate::hub::{AgentInfo, Hub};
use crate::wire::{decode, decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "activity";

/// How long a request waits for the agent (the session thread waits on it)
pub const OP_TIMEOUT: Duration = Duration::from_secs(2);

/// Above this many processes known, the exited ones are forgotten
const PRUNE_ABOVE: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply: `Option<u32>`, as [`audio_capture::find_process`]
    FindProcess { exe: String },
    /// Watches a process until it exits ([`Event::Exited`]). Reply: bool,
    /// exited already (or no such process)
    WatchProcess { pid: u32 },
    /// Reply: `Option<bool>` as [`audio_capture::mic_capture_active`], or
    /// a failure with the capture error as its detail
    MicCaptureActive { device: String },
    /// Reply: `Vec<String>`, as [`audio_capture::mic_recorders`]
    MicRecorders { device: String },
    /// Watches whether an application records from the capture side of
    /// the render endpoint `device`, every `every_ms` ([`Event::Recording`]).
    /// Reply: `Option<bool>`, None: it has no capture side (not watched)
    WatchRecording { watch: u64, device: String, every_ms: u32 },
    /// Ends a recording watch (sent without waiting). Reply: null
    Unwatch { watch: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A new agent's handler is ready: the core gives it its watches
    Ready,
    Exited {
        pid: u32,
    },
    Recording {
        watch: u64,
        active: bool,
    },
}

/// The facade's activity methods. Requests block (at most [`OP_TIMEOUT`]
/// through the agent): not for the BTstack thread or an audio thread.
pub trait ActivityHost {
    /// [`audio_capture::find_process`]
    fn find_process(&self, exe: &str) -> Option<u32>;
    /// [`audio_capture::process_exited`]. Through the agent: watched, so
    /// only the first call for a process waits; false while no agent knows
    /// (no agent connected).
    fn process_exited(&self, process_id: u32) -> bool;
    /// [`audio_capture::mic_capture_active`]
    fn mic_capture_active(&self, device_id: &str) -> Result<Option<bool>, CaptureError>;
    /// [`audio_capture::mic_recorders`]
    fn mic_recorders(&self, device_id: &str) -> Vec<String>;
    /// Watches whether an application records from the capture side of
    /// the render endpoint `device_id`, looked at every `every`. Ok(None):
    /// it has no capture side.
    fn watch_recording(&self, device_id: &str, every: Duration) -> Result<Option<RecordingWatch>, String>;
}

impl ActivityHost for LocalHost {
    fn find_process(&self, exe: &str) -> Option<u32> {
        audio_capture::find_process(exe)
    }

    fn process_exited(&self, process_id: u32) -> bool {
        audio_capture::process_exited(process_id)
    }

    fn mic_capture_active(&self, device_id: &str) -> Result<Option<bool>, CaptureError> {
        audio_capture::mic_capture_active(device_id)
    }

    fn mic_recorders(&self, device_id: &str) -> Vec<String> {
        audio_capture::mic_recorders(device_id)
    }

    fn watch_recording(&self, device_id: &str, _every: Duration) -> Result<Option<RecordingWatch>, String> {
        let com = win_sys::audio_policy::ComApartment::init_mta().map_err(|e| e.to_string())?;
        match win_sys::audio_policy::CaptureSessions::open(device_id) {
            Ok(Some(sessions)) => Ok(Some(RecordingWatch(Watch::Local { sessions, _com: com }))),
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// [`ActivityHost::find_process`] of this process's host
pub fn find_process(exe: &str) -> Option<u32> {
    crate::host().find_process(exe)
}

/// [`ActivityHost::process_exited`] of this process's host
pub fn process_exited(process_id: u32) -> bool {
    crate::host().process_exited(process_id)
}

/// [`ActivityHost::mic_capture_active`] of this process's host
pub fn mic_capture_active(device_id: &str) -> Result<Option<bool>, CaptureError> {
    crate::host().mic_capture_active(device_id)
}

/// [`ActivityHost::mic_recorders`] of this process's host
pub fn mic_recorders(device_id: &str) -> Vec<String> {
    crate::host().mic_recorders(device_id)
}

/// [`ActivityHost::watch_recording`] of this process's host
pub fn watch_recording(device_id: &str, every: Duration) -> Result<Option<RecordingWatch>, String> {
    crate::host().watch_recording(device_id, every)
}

/// Whether an application records from a capture side; the watch ends
/// when it is dropped. In this process it is bound to the thread that
/// opened it (COM).
pub struct RecordingWatch(Watch);

enum Watch {
    // Dropped before the apartment
    Local { sessions: win_sys::audio_policy::CaptureSessions, _com: win_sys::audio_policy::ComApartment },
    Remote { hub: Arc<Hub>, watch: u64, active: Arc<AtomicBool> },
}

impl std::fmt::Debug for RecordingWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingWatch").field("active", &self.active()).finish()
    }
}

impl RecordingWatch {
    /// Someone records now. Through the agent: as it last reported (kept
    /// while no agent is connected); never waits.
    pub fn active(&self) -> bool {
        match &self.0 {
            Watch::Local { sessions, .. } => sessions.active(),
            Watch::Remote { active, .. } => active.load(Ordering::Acquire),
        }
    }
}

impl Drop for RecordingWatch {
    fn drop(&mut self) {
        if let Watch::Remote { hub, watch, .. } = &self.0 {
            lock(&MIRROR).recordings.remove(watch);
            if let Some(agent) = hub.agent() {
                hub.notify(agent.generation, GROUP, to_body(&Request::Unwatch { watch: *watch }));
            }
        }
    }
}

// --- Through the agent ------------------------------------------------------

struct Recording {
    device: String,
    every_ms: u32,
    active: Arc<AtomicBool>,
}

/// The core's watches in the agent, and what the agent last reported
#[derive(Default)]
struct Mirror {
    /// Process -> (the agent generation that knows it, exited)
    processes: HashMap<u32, (u64, bool)>,
    recordings: HashMap<u64, Recording>,
}

impl Mirror {
    fn event(&mut self, generation: u64, event: &Event) {
        match event {
            Event::Ready => {}
            Event::Exited { pid } => {
                if let Some(p) = self.processes.get_mut(pid) {
                    *p = (generation, true);
                }
            }
            Event::Recording { watch, active } => {
                if let Some(r) = self.recordings.get(watch) {
                    r.active.store(*active, Ordering::Release);
                }
            }
        }
    }

    /// What is known of a process: Some(exited) once an agent watched it
    fn process(&self, pid: u32) -> Option<bool> {
        self.processes.get(&pid).map(|(_, exited)| *exited)
    }

    fn watched(&mut self, generation: u64, pid: u32, exited: bool) {
        if self.processes.len() >= PRUNE_ABOVE {
            self.processes.retain(|_, (_, exited)| !*exited);
        }
        self.processes.insert(pid, (generation, exited));
    }
}

static MIRROR: LazyLock<Mutex<Mirror>> = LazyLock::new(|| Mutex::new(Mirror::default()));
static ATTACHED: Mutex<Option<Weak<Hub>>> = Mutex::new(None);
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A request that waits at most [`OP_TIMEOUT`], and not at all while no
/// agent is connected
fn quick<T: serde::de::DeserializeOwned>(hub: &Hub, request: &Request) -> Result<(T, AgentInfo), HostError> {
    if hub.agent().is_none() {
        return Err(HostError::NoAgent);
    }
    let timeout = OP_TIMEOUT.min(hub.request_timeout());
    let (body, agent) = hub.request_on(GROUP, to_body(request), timeout)?;
    Ok((decode_reply(body)?, agent))
}

/// Takes in the agent's events and gives each new agent the watches (once
/// per hub)
fn attach(hub: &Arc<Hub>) {
    let mut attached = lock(&ATTACHED);
    if attached.as_ref().and_then(Weak::upgrade).is_some_and(|h| Arc::ptr_eq(&h, hub)) {
        return;
    }
    *attached = Some(Arc::downgrade(hub));
    *lock(&MIRROR) = Mirror::default();
    let weak = Arc::downgrade(hub);
    // On the hub's thread: no requests here
    hub.on_event(GROUP, move |body| {
        let Ok(event) = decode::<Event>(body) else { return };
        let Some(hub) = weak.upgrade() else { return };
        if event == Event::Ready {
            let weak = weak.clone();
            let spawned = std::thread::Builder::new().name("activity-resync".into()).spawn(move || {
                if let Some(hub) = weak.upgrade() {
                    resync(&hub);
                }
            });
            if let Err(e) = spawned {
                tracing::warn!("activity: not given to the new session agent: {e}");
            }
            return;
        }
        if let Some(agent) = hub.agent() {
            lock(&MIRROR).event(agent.generation, &event);
        }
    });
}

/// A new agent: the processes not exited and the recording watches again
fn resync(hub: &Hub) {
    let (processes, recordings) = {
        let m = lock(&MIRROR);
        let processes: Vec<u32> = m.processes.iter().filter(|(_, (_, exited))| !exited).map(|(p, _)| *p).collect();
        let recordings: Vec<(u64, String, u32)> =
            m.recordings.iter().map(|(w, r)| (*w, r.device.clone(), r.every_ms)).collect();
        (processes, recordings)
    };
    if processes.is_empty() && recordings.is_empty() {
        return;
    }
    let mut failed = 0;
    for pid in &processes {
        match quick::<bool>(hub, &Request::WatchProcess { pid: *pid }) {
            Ok((exited, agent)) => {
                let mut m = lock(&MIRROR);
                if m.processes.contains_key(pid) {
                    m.watched(agent.generation, *pid, exited);
                }
            }
            Err(_) => failed += 1,
        }
    }
    for (watch, device, every_ms) in &recordings {
        let request = Request::WatchRecording { watch: *watch, device: device.clone(), every_ms: *every_ms };
        match quick::<Option<bool>>(hub, &request) {
            Ok((Some(active), _)) => {
                if let Some(r) = lock(&MIRROR).recordings.get(watch) {
                    r.active.store(active, Ordering::Release);
                }
            }
            Ok((None, _)) | Err(_) => failed += 1,
        }
    }
    tracing::info!(
        "activity: given to the new session agent ({} process(es), {} recording watch(es), {failed} failed)",
        processes.len(),
        recordings.len()
    );
}

impl ActivityHost for RemoteHost {
    fn find_process(&self, exe: &str) -> Option<u32> {
        quick(self.hub(), &Request::FindProcess { exe: exe.to_string() }).map(|(p, _)| p).unwrap_or_else(|e| {
            tracing::debug!("activity: the process was not looked for: {e}");
            None
        })
    }

    fn process_exited(&self, process_id: u32) -> bool {
        let hub = self.hub();
        attach(hub);
        if let Some(exited) = lock(&MIRROR).process(process_id) {
            // An agent that went: the next gets it again (resync)
            return exited;
        }
        match quick::<bool>(hub, &Request::WatchProcess { pid: process_id }) {
            Ok((exited, agent)) => {
                lock(&MIRROR).watched(agent.generation, process_id, exited);
                exited
            }
            Err(e) => {
                tracing::debug!("activity: the process is not watched: {e}");
                false
            }
        }
    }

    fn mic_capture_active(&self, device_id: &str) -> Result<Option<bool>, CaptureError> {
        quick(self.hub(), &Request::MicCaptureActive { device: device_id.to_string() })
            .map(|(a, _)| a)
            .map_err(capture_error)
    }

    fn mic_recorders(&self, device_id: &str) -> Vec<String> {
        quick(self.hub(), &Request::MicRecorders { device: device_id.to_string() }).map(|(r, _)| r).unwrap_or_default()
    }

    fn watch_recording(&self, device_id: &str, every: Duration) -> Result<Option<RecordingWatch>, String> {
        let hub = self.hub();
        attach(hub);
        let watch = NEXT_WATCH.fetch_add(1, Ordering::Relaxed);
        let every_ms = u32::try_from(every.as_millis()).unwrap_or(u32::MAX).max(1);
        let active = Arc::new(AtomicBool::new(false));
        let recording = Recording { device: device_id.to_string(), every_ms, active: active.clone() };
        lock(&MIRROR).recordings.insert(watch, recording);
        let request = Request::WatchRecording { watch, device: device_id.to_string(), every_ms };
        let result = quick::<Option<bool>>(hub, &request);
        match result {
            Ok((Some(now), _)) => {
                active.store(now, Ordering::Release);
                Ok(Some(RecordingWatch(Watch::Remote { hub: hub.clone(), watch, active })))
            }
            Ok((None, _)) => {
                lock(&MIRROR).recordings.remove(&watch);
                Ok(None)
            }
            Err(e) => {
                lock(&MIRROR).recordings.remove(&watch);
                Err(e.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_have_a_stable_shape() {
        assert_eq!(serde_json::to_string(&Event::Exited { pid: 4 }).unwrap(), r#"{"kind":"exited","pid":4}"#);
        let r = Request::WatchRecording { watch: 1, device: "d".into(), every_ms: 50 };
        assert_eq!(decode::<Request>(to_body(&r)).unwrap(), r);
    }

    #[test]
    fn the_mirror_follows_the_agents_events() {
        let mut m = Mirror::default();
        assert_eq!(m.process(10), None);
        m.watched(1, 10, false);
        assert_eq!(m.process(10), Some(false));
        // Only watched processes are taken
        m.event(1, &Event::Exited { pid: 11 });
        assert_eq!(m.process(11), None);
        m.event(1, &Event::Exited { pid: 10 });
        assert_eq!(m.process(10), Some(true));
        let active = Arc::new(AtomicBool::new(false));
        m.recordings.insert(3, Recording { device: "d".into(), every_ms: 50, active: active.clone() });
        m.event(1, &Event::Recording { watch: 3, active: true });
        assert!(active.load(Ordering::Acquire));
        m.event(1, &Event::Recording { watch: 4, active: false });
        assert!(active.load(Ordering::Acquire));
    }

    #[test]
    fn exited_processes_are_pruned() {
        let mut m = Mirror::default();
        for pid in 0..PRUNE_ABOVE as u32 {
            m.watched(1, pid, pid % 2 == 0);
        }
        m.watched(1, 1000, false);
        assert!(m.processes.len() <= PRUNE_ABOVE / 2 + 1);
        assert_eq!(m.process(1), Some(false));
        assert_eq!(m.process(1000), Some(false));
    }

    #[test]
    fn the_local_host_asks_windows() {
        assert_eq!(LocalHost.find_process(""), None);
        assert!(LocalHost.process_exited(u32::MAX - 3));
        assert!(!LocalHost.process_exited(std::process::id()));
        assert!(LocalHost.watch_recording("no such device", Duration::from_millis(50)).is_err_or_none());
    }

    trait ErrOrNone {
        fn is_err_or_none(&self) -> bool;
    }

    impl<T, E> ErrOrNone for Result<Option<T>, E> {
        fn is_err_or_none(&self) -> bool {
            !matches!(self, Ok(Some(_)))
        }
    }
}
