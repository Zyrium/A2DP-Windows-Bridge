//! Group `volume` in the agent: the outputs the core watches and the mutes
//! it holds, watched here on one thread ([`watch`]) that sends the core an
//! event when something changes; the Volume Mixer.
//!
//! Dropping the handler (the core went away, exited or died) unmutes every
//! output it holds muted, as the core's own output mute does when a session
//! ends. Volumes are not given back: the agent cannot tell a core that died
//! from a broken connection, and the core would take a volume given back
//! for a change made in Windows and send it to the headphones.
//!
//! The Windows calls are behind [`Backend`]; the tests use a fake one.
//! With `BLUECASTER_TEST_FAKE_VOLUME=1` the agent of a debug build uses a
//! fake as well (the session agent's exe tests: they never touch this PC's
//! volume or mute). A release build ignores the variable: anyone can set
//! it in the user's environment, and the agent would then hold no mute.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio_capture::MixerApp;
use serde_json::Value;

use crate::host::volume::{Event, GROUP, Level, Request, SetVolumeReply};
use crate::wire::{ErrorCode, WireError, decode, reply, to_body};
use crate::{AgentContext, Handler};

/// The watcher's step: how often a held mute is looked at (Windows' mute
/// key unmutes; the speakers should hardly sound)
const STEP: Duration = Duration::from_millis(15);
/// Watched volumes are read every this many steps (60 ms)
const LEVEL_STEPS: u64 = 4;
/// The default output is read every this many steps (195 ms)
const DEFAULT_STEPS: u64 = 13;
/// A device that cannot be opened is tried again after this
const REOPEN_AFTER: Duration = Duration::from_secs(1);

/// The environment variable that gives the agent a fake [`Backend`] (debug
/// builds only)
pub const FAKE_ENV: &str = "BLUECASTER_TEST_FAKE_VOLUME";

/// [`FAKE_ENV`] is set, in a debug build
fn fake_wanted() -> bool {
    cfg!(debug_assertions) && std::env::var_os(FAKE_ENV).is_some_and(|v| v == "1")
}

/// An opened output's volume (may be bound to the thread that opened it)
pub(crate) trait Volume {
    fn volume(&self) -> Option<f32>;
    fn set_volume(&self, scalar: f32) -> bool;
    fn muted(&self) -> Option<bool>;
    fn set_muted(&self, muted: bool) -> bool;
}

/// What the handler asks Windows
pub(crate) trait Backend: Send + Sync + 'static {
    fn default_output(&self) -> Option<String>;
    /// The message goes to the core's log: no device IDs
    fn open(&self, device: &str) -> Result<Box<dyn Volume>, String>;
    fn mixer_apps(&self) -> Vec<MixerApp>;
    fn set_mixer_app(&self, key: &str, volume: Option<u8>, muted: Option<bool>) -> bool;
}

struct Windows;

impl Volume for audio_capture::EndpointVolume {
    fn volume(&self) -> Option<f32> {
        audio_capture::EndpointVolume::volume(self).ok()
    }
    fn set_volume(&self, scalar: f32) -> bool {
        audio_capture::EndpointVolume::set_volume(self, scalar).is_ok()
    }
    fn muted(&self) -> Option<bool> {
        audio_capture::EndpointVolume::muted(self).ok()
    }
    fn set_muted(&self, muted: bool) -> bool {
        audio_capture::EndpointVolume::set_muted(self, muted).is_ok()
    }
}

impl Backend for Windows {
    fn default_output(&self) -> Option<String> {
        audio_capture::default_render_device()
    }

    fn open(&self, device: &str) -> Result<Box<dyn Volume>, String> {
        match audio_capture::EndpointVolume::open(Some(device)) {
            Ok(v) => Ok(Box::new(v)),
            Err(audio_capture::CaptureError::DeviceNotFound(_)) => Err("no such output device".into()),
            Err(e) => Err(format!("the output's volume cannot be opened (HRESULT {:08x?})", e.hresult())),
        }
    }

    fn mixer_apps(&self) -> Vec<MixerApp> {
        audio_capture::mixer_apps()
    }

    fn set_mixer_app(&self, key: &str, volume: Option<u8>, muted: Option<bool>) -> bool {
        audio_capture::set_mixer_app(key, volume, muted)
    }
}

/// Outputs in memory: volume scalar and mute per device, the default
#[derive(Debug, Default)]
pub(crate) struct FakeOutputs {
    pub(crate) devices: Mutex<HashMap<String, (f32, bool)>>,
    pub(crate) default: Mutex<Option<String>>,
}

impl FakeOutputs {
    /// The fake of [`FAKE_ENV`]: one output, the default, at half volume
    pub(crate) const DEVICE: &'static str = "fake-output";

    pub(crate) fn with(devices: &[(&str, f32, bool)]) -> Arc<FakeOutputs> {
        let fake = FakeOutputs::default();
        for (id, v, m) in devices {
            lock(&fake.devices).insert(id.to_string(), (*v, *m));
        }
        *lock(&fake.default) = devices.first().map(|d| d.0.to_string());
        Arc::new(fake)
    }

    pub(crate) fn get(&self, id: &str) -> Option<(f32, bool)> {
        lock(&self.devices).get(id).copied()
    }

    #[cfg(test)]
    pub(crate) fn set(&self, id: &str, volume: f32, muted: bool) {
        lock(&self.devices).insert(id.to_string(), (volume, muted));
    }
}

struct FakeVolume {
    outputs: Arc<FakeOutputs>,
    id: String,
}

impl Volume for FakeVolume {
    fn volume(&self) -> Option<f32> {
        self.outputs.get(&self.id).map(|d| d.0)
    }
    fn set_volume(&self, scalar: f32) -> bool {
        match lock(&self.outputs.devices).get_mut(&self.id) {
            Some(d) => {
                d.0 = scalar.clamp(0.0, 1.0);
                true
            }
            None => false,
        }
    }
    fn muted(&self) -> Option<bool> {
        self.outputs.get(&self.id).map(|d| d.1)
    }
    fn set_muted(&self, muted: bool) -> bool {
        match lock(&self.outputs.devices).get_mut(&self.id) {
            Some(d) => {
                d.1 = muted;
                true
            }
            None => false,
        }
    }
}

impl Backend for Arc<FakeOutputs> {
    fn default_output(&self) -> Option<String> {
        lock(&self.default).clone()
    }

    fn open(&self, device: &str) -> Result<Box<dyn Volume>, String> {
        if self.get(device).is_none() {
            return Err("no such output device".into());
        }
        Ok(Box::new(FakeVolume { outputs: self.clone(), id: device.to_string() }))
    }

    fn mixer_apps(&self) -> Vec<MixerApp> {
        vec![MixerApp { key: "fake.exe".into(), name: "Fake".into(), volume: 50, muted: false }]
    }

    fn set_mixer_app(&self, key: &str, _volume: Option<u8>, _muted: Option<bool>) -> bool {
        key == "fake.exe"
    }
}

/// The fake of [`FAKE_ENV`], one per agent process (it outlives a
/// connection, as Windows' state does)
static FAKE: LazyLock<Arc<FakeOutputs>> = LazyLock::new(|| FakeOutputs::with(&[(FakeOutputs::DEVICE, 0.5, false)]));

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Sink = Arc<dyn Fn(&Event) + Send + Sync>;

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    let sink: Sink = Arc::new(move |e: &Event| {
        ctx.event(GROUP, to_body(e));
    });
    sink(&Event::Ready);
    if fake_wanted() {
        tracing::info!("volume: a fake output (tests), this PC's volume is not used");
        Box::new(Volumes::new(FAKE.clone(), sink))
    } else {
        Box::new(Volumes::new(Windows, sink))
    }
}

#[derive(Debug, Default)]
struct State {
    /// Some: the default output is watched (its last value)
    default: Option<Option<String>>,
    /// Watch -> device
    watches: HashMap<u64, String>,
    /// Watched device -> its last reading
    levels: HashMap<String, Level>,
    /// Held device -> its volume when last looked at
    holds: HashMap<String, Option<f32>>,
    seq: u64,
}

impl State {
    /// A reading of `device`: a new [`Level`] if it changed
    fn reading(&mut self, device: &str, volume: Option<f32>, muted: Option<bool>) -> (Level, bool) {
        match self.levels.get(device) {
            Some(l) if l.volume == volume && l.muted == muted => (*l, false),
            _ => {
                self.seq += 1;
                let level = Level { seq: self.seq, volume, muted };
                self.levels.insert(device.to_string(), level);
                (level, true)
            }
        }
    }
}

struct Shared<B> {
    backend: B,
    sink: Sink,
    state: Mutex<State>,
    stop: AtomicBool,
}

struct Volumes<B: Backend> {
    shared: Arc<Shared<B>>,
    watcher: Option<JoinHandle<()>>,
}

impl<B: Backend> Volumes<B> {
    fn new(backend: B, sink: Sink) -> Volumes<B> {
        let shared = Arc::new(Shared { backend, sink, state: Mutex::default(), stop: AtomicBool::new(false) });
        Volumes { shared, watcher: None }
    }

    fn watching(&mut self) {
        if self.watcher.is_some() {
            return;
        }
        let shared = self.shared.clone();
        match std::thread::Builder::new().name("agent-volume-watch".into()).spawn(move || watch(&shared)) {
            Ok(t) => self.watcher = Some(t),
            Err(e) => tracing::warn!("volume: no watcher: {e}"),
        }
    }

    fn open(&self, device: &str) -> Result<Box<dyn Volume>, WireError> {
        self.shared.backend.open(device).map_err(|e| WireError::new(ErrorCode::Failed, e))
    }
}

impl<B: Backend> Handler for Volumes<B> {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::WatchDefault => {
                let device = self.shared.backend.default_output();
                lock(&self.shared.state).default = Some(device.clone());
                self.watching();
                reply(device)
            }
            Request::Watch { watch, device } => {
                let v = self.open(&device)?;
                let mut state = lock(&self.shared.state);
                let (level, _) = state.reading(&device, v.volume(), v.muted());
                state.watches.insert(watch, device);
                drop(state);
                self.watching();
                reply(level)
            }
            Request::Unwatch { watch } => {
                let mut state = lock(&self.shared.state);
                if let Some(device) = state.watches.remove(&watch)
                    && !state.watches.values().any(|d| *d == device)
                {
                    state.levels.remove(&device);
                }
                reply(())
            }
            Request::SetVolume { watch, volume } => {
                let mut state = lock(&self.shared.state);
                let Some(device) = state.watches.get(&watch).cloned() else {
                    return Err(WireError::new(ErrorCode::Failed, "not watched"));
                };
                // Under the lock: the watcher's next reading is this one,
                // not an event that overtakes the reply
                let v = self.open(&device)?;
                let ok = v.set_volume(volume);
                let (level, _) = state.reading(&device, v.volume(), v.muted());
                reply(SetVolumeReply { ok, level })
            }
            Request::HoldMute { device } => {
                let v = self.open(&device)?;
                let mut state = lock(&self.shared.state);
                let muted = match v.muted() {
                    Some(true) => true,
                    Some(false) => v.set_muted(true),
                    None => false,
                };
                if muted {
                    state.holds.insert(device, v.volume());
                    tracing::info!("volume: an output held muted for the core ({} held)", state.holds.len());
                }
                drop(state);
                if muted {
                    self.watching();
                }
                reply(muted)
            }
            Request::ReleaseMute { device } => {
                let mut state = lock(&self.shared.state);
                if state.holds.remove(&device).is_some() {
                    tracing::info!("volume: an output let go of ({} held)", state.holds.len());
                }
                let unmuted = self.shared.backend.open(&device).is_ok_and(|v| v.set_muted(false));
                reply(unmuted)
            }
            Request::MixerApps => reply(self.shared.backend.mixer_apps()),
            Request::SetMixerApp { key, volume, muted } => {
                reply(self.shared.backend.set_mixer_app(&key, volume, muted))
            }
        }
    }
}

impl<B: Backend> Drop for Volumes<B> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.watcher.take() {
            let _ = t.join();
        }
        let holds = std::mem::take(&mut lock(&self.shared.state).holds);
        if holds.is_empty() {
            return;
        }
        let unmuted = holds.keys().filter(|d| self.shared.backend.open(d).is_ok_and(|v| v.set_muted(false))).count();
        tracing::info!("volume: the core went away, {unmuted} of {} held output(s) unmuted", holds.len());
    }
}

/// The watcher: held mutes every [`STEP`], watched volumes and the default
/// output less often. Its devices stay open here (COM objects of this
/// thread).
fn watch<B: Backend>(shared: &Shared<B>) {
    let mut open: HashMap<String, Box<dyn Volume>> = HashMap::new();
    let mut failed: HashMap<String, Instant> = HashMap::new();
    let mut step: u64 = 0;
    while !shared.stop.load(Ordering::Acquire) {
        std::thread::sleep(STEP);
        step += 1;
        let mut state = lock(&shared.state);
        open.retain(|d, _| state.holds.contains_key(d) || state.levels.contains_key(d));
        failed.retain(|_, at| at.elapsed() < REOPEN_AFTER);
        let mut get = |device: &str| -> bool {
            if open.contains_key(device) {
                return true;
            }
            if failed.contains_key(device) {
                return false;
            }
            match shared.backend.open(device) {
                Ok(v) => {
                    open.insert(device.to_string(), v);
                    true
                }
                Err(_) => {
                    failed.insert(device.to_string(), Instant::now());
                    false
                }
            }
        };
        let held: Vec<String> = state.holds.keys().cloned().collect();
        let watched: Vec<String> =
            if step.is_multiple_of(LEVEL_STEPS) { state.levels.keys().cloned().collect() } else { Vec::new() };
        let usable: Vec<String> = held.iter().chain(&watched).filter(|d| get(d.as_str())).cloned().collect();
        for device in held.iter().filter(|d| usable.contains(d)) {
            let v = &open[device];
            let volume = v.volume();
            let Some(last) = state.holds.get_mut(device) else { continue };
            let changed = volume != *last;
            *last = volume;
            if v.muted() != Some(false) {
                continue;
            }
            // Unmuted in Windows: muted again; with the volume as it was,
            // the mute key, else a volume key
            if v.set_muted(true) {
                tracing::debug!("volume: a held output was unmuted in Windows, muted again");
                (shared.sink)(&Event::MuteKey { device: device.clone(), volume_changed: changed });
            } else {
                tracing::warn!("volume: a held output was unmuted in Windows and cannot be muted again");
            }
        }
        for device in watched.iter().filter(|d| usable.contains(d)) {
            let v = &open[device];
            let (level, changed) = state.reading(device, v.volume(), v.muted());
            if changed {
                (shared.sink)(&Event::Level { device: device.clone(), level });
            }
        }
        if step.is_multiple_of(DEFAULT_STEPS) && state.default.is_some() {
            let device = shared.backend.default_output();
            if state.default.as_ref() != Some(&device) {
                state.default = Some(device.clone());
                (shared.sink)(&Event::Default { device });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(fake: &Arc<FakeOutputs>) -> (Volumes<Arc<FakeOutputs>>, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let sink: Sink = Arc::new(move |ev: &Event| lock(&e).push(ev.clone()));
        (Volumes::new(fake.clone(), sink), events)
    }

    fn ask<T: serde::de::DeserializeOwned>(h: &mut impl Handler, r: Request) -> Result<T, WireError> {
        h.request(to_body(&r)).map(|v| serde_json::from_value(v).unwrap())
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn has(events: &Mutex<Vec<Event>>, f: impl Fn(&Event) -> bool) -> bool {
        lock(events).iter().any(f)
    }

    #[test]
    fn watches_report_changes_made_in_windows() {
        let fake = FakeOutputs::with(&[("spk", 0.5, false), ("usb", 0.2, false)]);
        let (mut h, events) = handler(&fake);
        let level: Level = ask(&mut h, Request::Watch { watch: 1, device: "spk".into() }).unwrap();
        assert_eq!((level.volume, level.muted), (Some(0.5), Some(false)));
        assert_eq!(ask::<Option<String>>(&mut h, Request::WatchDefault).unwrap().as_deref(), Some("spk"));
        // The volume keys
        fake.set("spk", 0.7, false);
        wait_for("the level event", || {
            has(&events, |e| matches!(e, Event::Level { level, .. } if level.volume == Some(0.7)))
        });
        // Another default output
        *lock(&fake.default) = Some("usb".into());
        wait_for("the default event", || has(&events, |e| *e == Event::Default { device: Some("usb".into()) }));
        // Nothing is reported for a volume the core set itself
        lock(&events).clear();
        let set: SetVolumeReply = ask(&mut h, Request::SetVolume { watch: 1, volume: 0.25 }).unwrap();
        assert!(set.ok && set.level.volume == Some(0.25) && set.level.seq > level.seq);
        assert_eq!(fake.get("spk"), Some((0.25, false)));
        std::thread::sleep(Duration::from_millis(150));
        assert!(!has(&events, |e| matches!(e, Event::Level { .. })));
        // Unwatched: no more events
        ask::<()>(&mut h, Request::Unwatch { watch: 1 }).unwrap();
        assert!(ask::<SetVolumeReply>(&mut h, Request::SetVolume { watch: 1, volume: 0.1 }).is_err());
        fake.set("spk", 0.9, false);
        std::thread::sleep(Duration::from_millis(150));
        assert!(!has(&events, |e| matches!(e, Event::Level { .. })));
        assert!(ask::<Level>(&mut h, Request::Watch { watch: 2, device: "gone".into() }).is_err());
    }

    #[test]
    fn a_held_mute_is_kept_against_windows_keys() {
        let fake = FakeOutputs::with(&[("spk", 0.5, false)]);
        let (mut h, events) = handler(&fake);
        assert!(ask::<bool>(&mut h, Request::HoldMute { device: "spk".into() }).unwrap());
        assert_eq!(fake.get("spk"), Some((0.5, true)));
        // The mute key: muted again, reported as the mute key
        fake.set("spk", 0.5, false);
        wait_for("the mute key", || has(&events, |e| matches!(e, Event::MuteKey { volume_changed: false, .. })));
        assert_eq!(fake.get("spk"), Some((0.5, true)));
        // A volume key unmutes with a new volume
        lock(&events).clear();
        fake.set("spk", 0.6, false);
        wait_for("the volume key", || has(&events, |e| matches!(e, Event::MuteKey { volume_changed: true, .. })));
        assert_eq!(fake.get("spk"), Some((0.6, true)));
        // Let go of: unmuted, no longer kept
        assert!(ask::<bool>(&mut h, Request::ReleaseMute { device: "spk".into() }).unwrap());
        assert_eq!(fake.get("spk"), Some((0.6, false)));
        lock(&events).clear();
        fake.set("spk", 0.6, true);
        fake.set("spk", 0.6, false);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!has(&events, |e| matches!(e, Event::MuteKey { .. })));
        assert!(ask::<bool>(&mut h, Request::HoldMute { device: "gone".into() }).is_err());
    }

    #[test]
    fn the_core_going_away_unmutes_what_it_held() {
        let fake = FakeOutputs::with(&[("spk", 0.5, false), ("usb", 0.5, true), ("cable", 0.5, false)]);
        let (mut h, _) = handler(&fake);
        assert!(ask::<bool>(&mut h, Request::HoldMute { device: "spk".into() }).unwrap());
        // One the user had muted is held as well (the core decides what is
        // its own), so it is unmuted too
        assert!(ask::<bool>(&mut h, Request::HoldMute { device: "usb".into() }).unwrap());
        ask::<Level>(&mut h, Request::Watch { watch: 1, device: "cable".into() }).unwrap();
        ask::<SetVolumeReply>(&mut h, Request::SetVolume { watch: 1, volume: 0.9 }).unwrap();
        drop(h);
        assert_eq!(fake.get("spk"), Some((0.5, false)));
        assert_eq!(fake.get("usb"), Some((0.5, false)));
        // Volumes stay as set
        assert_eq!(fake.get("cable"), Some((0.9, false)));
    }

    #[test]
    fn a_device_that_cannot_be_muted_is_not_held() {
        struct Stuck;
        impl Volume for Stuck {
            fn volume(&self) -> Option<f32> {
                Some(0.5)
            }
            fn set_volume(&self, _: f32) -> bool {
                false
            }
            fn muted(&self) -> Option<bool> {
                Some(false)
            }
            fn set_muted(&self, _: bool) -> bool {
                false
            }
        }
        struct StuckBackend;
        impl Backend for StuckBackend {
            fn default_output(&self) -> Option<String> {
                None
            }
            fn open(&self, _: &str) -> Result<Box<dyn Volume>, String> {
                Ok(Box::new(Stuck))
            }
            fn mixer_apps(&self) -> Vec<MixerApp> {
                Vec::new()
            }
            fn set_mixer_app(&self, _: &str, _: Option<u8>, _: Option<bool>) -> bool {
                false
            }
        }
        let mut h = Volumes::new(StuckBackend, Arc::new(|_: &Event| {}));
        assert!(!ask::<bool>(&mut h, Request::HoldMute { device: "x".into() }).unwrap());
        assert!(lock(&h.shared.state).holds.is_empty());
    }

    #[test]
    fn the_mixer_goes_to_the_backend() {
        let fake = FakeOutputs::with(&[("spk", 0.5, false)]);
        let (mut h, _) = handler(&fake);
        assert_eq!(ask::<Vec<MixerApp>>(&mut h, Request::MixerApps).unwrap().len(), 1);
        let set = Request::SetMixerApp { key: "fake.exe".into(), volume: Some(20), muted: None };
        assert!(ask::<bool>(&mut h, set).unwrap());
        assert_eq!(h.request(serde_json::json!({"op": "nonsense"})).unwrap_err().code, ErrorCode::BadRequest);
    }

    #[test]
    fn the_real_handler_refuses_an_unknown_device() {
        // Windows' backend, read only; the ready event goes nowhere (detached)
        let mut h = new(AgentContext::detached());
        assert!(h.request(serde_json::json!({"op": "watch", "watch": 1, "device": "no such device"})).is_err());
    }
}
