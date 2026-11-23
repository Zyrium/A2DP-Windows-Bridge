//! Group `volume`: an output device's master volume and mute (volume sync,
//! the output mute while streaming), the default output as watched for
//! them, and the per-app Volume Mixer.
//!
//! Volume sync and the output mute tick on the session thread a few times
//! a second. Through the agent nothing of that is a request per tick: the
//! agent watches what the core opened ([`Request::Watch`],
//! [`Request::WatchDefault`]) and sends an event when the volume, the mute
//! or the default output changes; the core reads its copy ([`Mirror`]).
//! Only a change the core makes (a new device, a volume to set, a mute) is
//! a request, and those wait at most [`OP_TIMEOUT`] and not at all while no
//! agent is connected.
//!
//! The output mute is held by the agent: [`OutputVolume::set_muted`]`(true)`
//! asks it to mute the device and keep it muted. The agent watches Windows'
//! mute and volume keys on it itself (every few milliseconds, as the core's
//! own watcher did) and mutes it again, telling the core which key it was
//! ([`on_mute_key`]). When the core goes away (exits or dies) the agent
//! unmutes what it holds. The core keeps the same in its mirror: a new
//! agent gets the held mutes, an unmute the old agent did not get, the
//! watches and the default watch again (its handler says
//! [`Event::Ready`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use audio_capture::MixerApp;
use serde::{Deserialize, Serialize};

use crate::hub::{AgentInfo, Hub};
use crate::wire::{decode, decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "volume";

/// How long a change waits for the agent (the session thread waits on it)
pub const OP_TIMEOUT: Duration = Duration::from_secs(2);

/// A watched output's volume and mute. `seq` counts the agent's readings,
/// so an older one never replaces a newer one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub seq: u64,
    /// Master volume scalar 0.0..=1.0; None if it cannot be read
    pub volume: Option<f32>,
    pub muted: Option<bool>,
}

/// The reply to [`Request::SetVolume`]
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SetVolumeReply {
    /// Windows took it
    pub ok: bool,
    /// The level afterwards (the device may have volume steps)
    pub level: Level,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Watches the default output from now on ([`Event::Default`]).
    /// Reply: `Option<String>`, the default output now
    WatchDefault,
    /// Watches an output's volume and mute ([`Event::Level`]). Reply:
    /// [`Level`], or a failure when the device cannot be opened
    Watch { watch: u64, device: String },
    /// Ends a watch (sent without waiting). Reply: null
    Unwatch { watch: u64 },
    /// Sets a watched output's master volume. Reply: [`SetVolumeReply`]
    SetVolume { watch: u64, volume: f32 },
    /// Mutes an output and keeps it muted for the core (Windows' keys
    /// muted again, [`Event::MuteKey`]); unmuted when the core goes away.
    /// Reply: bool, muted now
    HoldMute { device: String },
    /// Lets go of a held mute and unmutes the output. Reply: bool, unmuted
    ReleaseMute { device: String },
    /// Reply: `Vec<MixerApp>`
    MixerApps,
    /// Reply: bool, as [`audio_capture::set_mixer_app`]
    SetMixerApp { key: String, volume: Option<u8>, muted: Option<bool> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A new agent's handler is ready: the core gives it what it holds
    Ready,
    /// The default output changed (after [`Request::WatchDefault`])
    Default { device: Option<String> },
    /// A watched output's volume or mute changed
    Level { device: String, level: Level },
    /// Windows unmuted a held output: muted again. `volume_changed`: with a
    /// new volume (a volume key), else the mute key.
    MuteKey { device: String, volume_changed: bool },
}

/// The facade's volume methods. Opening and setting block until Windows (or
/// the agent, at most [`OP_TIMEOUT`]) answers: not for the BTstack thread
/// or an audio thread. Reading an opened volume never waits.
pub trait VolumeHost {
    /// The default output device (Console role), for volume sync and the
    /// output mute. Through the agent: as it last reported (watched), None
    /// while no agent is connected.
    fn volume_default_output(&self) -> Option<String>;
    /// Opens an output device's master volume and mute
    fn open_output_volume(&self, device_id: &str) -> Result<OutputVolume, String>;
    /// The apps in the Volume Mixer ([`audio_capture::mixer_apps`])
    fn mixer_apps(&self) -> Vec<MixerApp>;
    /// [`audio_capture::set_mixer_app`]
    fn set_mixer_app(&self, key: &str, volume: Option<u8>, muted: Option<bool>) -> bool;
    /// Whether a held mute is kept muted by the host itself (the agent
    /// watches Windows' keys, [`on_mute_key`]); false: the caller watches
    fn host_keeps_mute(&self) -> bool;
}

impl VolumeHost for LocalHost {
    fn volume_default_output(&self) -> Option<String> {
        audio_capture::default_render_device()
    }

    fn open_output_volume(&self, device_id: &str) -> Result<OutputVolume, String> {
        audio_capture::EndpointVolume::open(Some(device_id))
            .map(|v| OutputVolume(Inner::Local(v)))
            .map_err(|e| e.to_string())
    }

    fn mixer_apps(&self) -> Vec<MixerApp> {
        audio_capture::mixer_apps()
    }

    fn set_mixer_app(&self, key: &str, volume: Option<u8>, muted: Option<bool>) -> bool {
        audio_capture::set_mixer_app(key, volume, muted)
    }

    fn host_keeps_mute(&self) -> bool {
        false
    }
}

/// [`VolumeHost::volume_default_output`] of this process's host
pub fn default_output() -> Option<String> {
    crate::host().volume_default_output()
}

/// [`VolumeHost::open_output_volume`] of this process's host
pub fn open_output_volume(device_id: &str) -> Result<OutputVolume, String> {
    crate::host().open_output_volume(device_id)
}

/// [`VolumeHost::mixer_apps`] of this process's host
pub fn mixer_apps() -> Vec<MixerApp> {
    crate::host().mixer_apps()
}

/// [`VolumeHost::set_mixer_app`] of this process's host
pub fn set_mixer_app(key: &str, volume: Option<u8>, muted: Option<bool>) -> bool {
    crate::host().set_mixer_app(key, volume, muted)
}

/// [`VolumeHost::host_keeps_mute`] of this process's host
pub fn host_keeps_mute() -> bool {
    crate::host().host_keeps_mute()
}

type KeyFn = Arc<dyn Fn(bool) + Send + Sync>;
static MUTE_KEY: Mutex<Option<KeyFn>> = Mutex::new(None);

/// Called (on the hub's thread: it must not block) with `volume_changed`
/// each time the agent mutes a held output again after Windows' mute key
/// (false) or a volume key (true). Replaces an earlier callback.
pub fn on_mute_key(f: impl Fn(bool) + Send + Sync + 'static) {
    *lock(&MUTE_KEY) = Some(Arc::new(f));
}

/// An output device's master volume and mute, in this process or in the
/// agent. Not `Send` in this process (COM): keep it on the thread that
/// opened it.
#[derive(Debug)]
pub struct OutputVolume(Inner);

#[derive(Debug)]
enum Inner {
    Local(audio_capture::EndpointVolume),
    Remote(RemoteVolume),
}

impl OutputVolume {
    /// The endpoint ID it belongs to
    pub fn device_id(&self) -> &str {
        match &self.0 {
            Inner::Local(v) => v.device_id(),
            Inner::Remote(r) => &r.device,
        }
    }

    /// Master volume scalar 0.0..=1.0; None if it cannot be read (or, in
    /// the agent, while no agent watches it)
    pub fn volume(&self) -> Option<f32> {
        match &self.0 {
            Inner::Local(v) => v.volume().ok(),
            Inner::Remote(r) => r.level().and_then(|l| l.volume),
        }
    }

    /// Sets the master volume scalar; false if it failed
    pub fn set_volume(&self, scalar: f32) -> bool {
        match &self.0 {
            Inner::Local(v) => v.set_volume(scalar).is_ok(),
            Inner::Remote(r) => r.set_volume(scalar),
        }
    }

    pub fn muted(&self) -> Option<bool> {
        match &self.0 {
            Inner::Local(v) => v.muted().ok(),
            Inner::Remote(r) => r.level().and_then(|l| l.muted),
        }
    }

    /// Mutes or unmutes; false if it failed. Through the agent, a mute is
    /// held ([`Request::HoldMute`]) and an unmute lets go of it.
    pub fn set_muted(&self, muted: bool) -> bool {
        match &self.0 {
            Inner::Local(v) => v.set_muted(muted).is_ok(),
            Inner::Remote(r) => r.set_muted(muted),
        }
    }

    /// Whether a held mute is kept by the host ([`VolumeHost::host_keeps_mute`])
    pub fn keeps_mute(&self) -> bool {
        matches!(self.0, Inner::Remote(_))
    }
}

// --- Through the agent ------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// Held muted by the agent
    Muted,
    /// To be unmuted (the agent did not confirm yet)
    Unmuted,
}

/// What the core holds and watches in the agent, and what the agent last
/// reported (each value with the agent generation that reported it)
#[derive(Debug, Default)]
struct Mirror {
    default_watched: bool,
    default: Option<(u64, Option<String>)>,
    /// Watch -> device
    watches: HashMap<u64, String>,
    levels: HashMap<String, (u64, Level)>,
    holds: HashMap<String, Want>,
}

impl Mirror {
    /// A reading of `generation`: newer than what is kept, or from a newer
    /// agent
    fn level(&mut self, generation: u64, device: &str, level: Level) {
        match self.levels.get_mut(device) {
            Some((g, l)) if *g == generation && l.seq >= level.seq => {}
            Some(entry) => *entry = (generation, level),
            None => {
                self.levels.insert(device.to_string(), (generation, level));
            }
        }
    }

    /// An event of the agent of `generation`; true for a mute key of a
    /// held output
    fn event(&mut self, generation: u64, event: &Event) -> bool {
        match event {
            Event::Ready => false,
            Event::Default { device } => {
                if self.default_watched {
                    self.default = Some((generation, device.clone()));
                }
                false
            }
            Event::Level { device, level } => {
                if self.watches.values().any(|d| d == device) {
                    self.level(generation, device, *level);
                }
                false
            }
            Event::MuteKey { device, .. } => self.holds.get(device) == Some(&Want::Muted),
        }
    }

    fn current_level(&self, generation: u64, device: &str) -> Option<Level> {
        self.levels.get(device).filter(|(g, _)| *g == generation).map(|(_, l)| *l)
    }
}

static MIRROR: LazyLock<Mutex<Mirror>> = LazyLock::new(|| Mutex::new(Mirror::default()));
/// The hub whose agents get [`MIRROR`]
static ATTACHED: Mutex<Option<Weak<Hub>>> = Mutex::new(None);
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A request that waits at most [`OP_TIMEOUT`], and not at all while no
/// agent is connected; also says which agent answered
fn quick<T: serde::de::DeserializeOwned>(hub: &Hub, request: &Request) -> Result<(T, AgentInfo), HostError> {
    if hub.agent().is_none() {
        return Err(HostError::NoAgent);
    }
    let timeout = OP_TIMEOUT.min(hub.request_timeout());
    let (body, agent) = hub.request_on(GROUP, to_body(request), timeout)?;
    Ok((decode_reply(body)?, agent))
}

fn generation(hub: &Hub) -> Option<u64> {
    hub.agent().map(|a| a.generation)
}

/// Gives the mirror to each new agent of `hub` and takes in the agent's
/// events (once per hub; a new hub starts with an empty mirror)
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
            let spawned = std::thread::Builder::new().name("volume-resync".into()).spawn(move || {
                if let Some(hub) = weak.upgrade() {
                    resync(&hub);
                }
            });
            if let Err(e) = spawned {
                tracing::warn!("volume: not given to the new session agent: {e}");
            }
            return;
        }
        let Some(generation) = generation(&hub) else { return };
        let key = lock(&MIRROR).event(generation, &event);
        if let (true, Event::MuteKey { volume_changed, .. }) = (key, &event) {
            let f = lock(&MUTE_KEY).clone();
            if let Some(f) = f {
                f(*volume_changed);
            }
        }
    });
}

/// A new agent: the held mutes, the unmutes not confirmed, the default
/// watch and the watches again
fn resync(hub: &Hub) {
    let (holds, default, watches) = {
        let m = lock(&MIRROR);
        let holds: Vec<(String, Want)> = m.holds.iter().map(|(d, w)| (d.clone(), *w)).collect();
        let watches: Vec<(u64, String)> = m.watches.iter().map(|(w, d)| (*w, d.clone())).collect();
        (holds, m.default_watched, watches)
    };
    if holds.is_empty() && !default && watches.is_empty() {
        return;
    }
    let mut failed = 0;
    for (device, want) in &holds {
        match want {
            Want::Muted => {
                if !quick::<bool>(hub, &Request::HoldMute { device: device.clone() }).is_ok_and(|(m, _)| m) {
                    failed += 1;
                }
            }
            Want::Unmuted => match quick::<bool>(hub, &Request::ReleaseMute { device: device.clone() }) {
                Ok(_) => {
                    let mut m = lock(&MIRROR);
                    if m.holds.get(device) == Some(&Want::Unmuted) {
                        m.holds.remove(device);
                    }
                }
                Err(_) => failed += 1,
            },
        }
    }
    if default {
        match quick::<Option<String>>(hub, &Request::WatchDefault) {
            Ok((d, agent)) => lock(&MIRROR).default = Some((agent.generation, d)),
            Err(_) => failed += 1,
        }
    }
    for (watch, device) in &watches {
        match quick::<Level>(hub, &Request::Watch { watch: *watch, device: device.clone() }) {
            Ok((level, agent)) => {
                let mut m = lock(&MIRROR);
                if m.watches.contains_key(watch) {
                    m.level(agent.generation, device, level);
                }
            }
            Err(_) => failed += 1,
        }
    }
    tracing::info!(
        "volume: given to the new session agent ({} mute(s), {} watch(es), default watched: {default}, {failed} failed)",
        holds.len(),
        watches.len()
    );
}

impl VolumeHost for RemoteHost {
    fn volume_default_output(&self) -> Option<String> {
        let hub = self.hub();
        attach(hub);
        let current = generation(hub)?;
        {
            let mut m = lock(&MIRROR);
            if m.default_watched {
                return m.default.as_ref().filter(|(g, _)| *g == current).and_then(|(_, d)| d.clone());
            }
            m.default_watched = true;
        }
        match quick::<Option<String>>(hub, &Request::WatchDefault) {
            Ok((d, agent)) => {
                lock(&MIRROR).default = Some((agent.generation, d.clone()));
                d
            }
            Err(e) => {
                // Asked again next time
                lock(&MIRROR).default_watched = false;
                tracing::debug!("volume: the default output is not watched: {e}");
                None
            }
        }
    }

    fn open_output_volume(&self, device_id: &str) -> Result<OutputVolume, String> {
        let hub = self.hub();
        attach(hub);
        let watch = NEXT_WATCH.fetch_add(1, Ordering::Relaxed);
        lock(&MIRROR).watches.insert(watch, device_id.to_string());
        match quick::<Level>(hub, &Request::Watch { watch, device: device_id.to_string() }) {
            Ok((level, agent)) => {
                lock(&MIRROR).level(agent.generation, device_id, level);
                Ok(OutputVolume(Inner::Remote(RemoteVolume { hub: hub.clone(), watch, device: device_id.to_string() })))
            }
            Err(e) => {
                lock(&MIRROR).watches.remove(&watch);
                Err(e.to_string())
            }
        }
    }

    fn mixer_apps(&self) -> Vec<MixerApp> {
        self.call(GROUP, &Request::MixerApps).unwrap_or_else(|e| {
            tracing::warn!("session host: the mixer was not read: {e}");
            Vec::new()
        })
    }

    fn set_mixer_app(&self, key: &str, volume: Option<u8>, muted: Option<bool>) -> bool {
        let request = Request::SetMixerApp { key: key.to_string(), volume, muted };
        self.call(GROUP, &request).unwrap_or_else(|e| {
            tracing::warn!("session host: the mixer was not set: {e}");
            false
        })
    }

    fn host_keeps_mute(&self) -> bool {
        true
    }
}

/// An output's volume watched by the agent
struct RemoteVolume {
    hub: Arc<Hub>,
    watch: u64,
    device: String,
}

impl std::fmt::Debug for RemoteVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteVolume").field("watch", &self.watch).finish()
    }
}

impl RemoteVolume {
    /// What the connected agent last reported; None from an agent before
    fn level(&self) -> Option<Level> {
        let generation = generation(&self.hub)?;
        lock(&MIRROR).current_level(generation, &self.device)
    }

    fn set_volume(&self, scalar: f32) -> bool {
        match quick::<SetVolumeReply>(&self.hub, &Request::SetVolume { watch: self.watch, volume: scalar }) {
            Ok((reply, agent)) => {
                lock(&MIRROR).level(agent.generation, &self.device, reply.level);
                reply.ok
            }
            Err(e) => {
                tracing::debug!("volume: not set: {e}");
                false
            }
        }
    }

    fn set_muted(&self, muted: bool) -> bool {
        let device = self.device.clone();
        if muted {
            lock(&MIRROR).holds.insert(device.clone(), Want::Muted);
            let held = quick::<bool>(&self.hub, &Request::HoldMute { device: device.clone() }).map(|(m, _)| m);
            if !held.as_ref().is_ok_and(|m| *m) {
                let mut m = lock(&MIRROR);
                if m.holds.get(&device) == Some(&Want::Muted) {
                    m.holds.remove(&device);
                }
            }
            held.unwrap_or_else(|e| {
                tracing::debug!("volume: the mute was not held: {e}");
                false
            })
        } else {
            lock(&MIRROR).holds.insert(device.clone(), Want::Unmuted);
            match quick::<bool>(&self.hub, &Request::ReleaseMute { device: device.clone() }) {
                Ok((unmuted, _)) => {
                    let mut m = lock(&MIRROR);
                    if m.holds.get(&device) == Some(&Want::Unmuted) {
                        m.holds.remove(&device);
                    }
                    unmuted
                }
                Err(e) => {
                    // The next agent unmutes it (resync)
                    tracing::debug!("volume: the unmute waits for the next session agent: {e}");
                    false
                }
            }
        }
    }
}

impl Drop for RemoteVolume {
    fn drop(&mut self) {
        lock(&MIRROR).watches.remove(&self.watch);
        if let Some(agent) = self.hub.agent() {
            self.hub.notify(agent.generation, GROUP, to_body(&Request::Unwatch { watch: self.watch }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(seq: u64, volume: f32) -> Level {
        Level { seq, volume: Some(volume), muted: Some(false) }
    }

    #[test]
    fn requests_and_events_have_a_stable_shape() {
        let r = Request::Watch { watch: 3, device: "d".into() };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"op":"watch","watch":3,"device":"d"}"#);
        assert_eq!(serde_json::to_string(&Event::Ready).unwrap(), r#"{"kind":"ready"}"#);
        let e = Event::MuteKey { device: "d".into(), volume_changed: true };
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"kind":"mute_key","device":"d","volume_changed":true}"#);
        for r in [Request::WatchDefault, Request::MixerApps, Request::SetVolume { watch: 1, volume: 0.5 }] {
            assert_eq!(decode::<Request>(to_body(&r)).unwrap(), r);
        }
    }

    #[test]
    fn the_mirror_keeps_the_newest_reading_of_the_current_agent() {
        let mut m = Mirror::default();
        m.watches.insert(1, "spk".into());
        m.level(1, "spk", level(5, 0.5));
        // An older reading (an event overtaken by a reply) is ignored
        assert!(!m.event(1, &Event::Level { device: "spk".into(), level: level(4, 0.2) }));
        assert_eq!(m.current_level(1, "spk").unwrap().volume, Some(0.5));
        m.event(1, &Event::Level { device: "spk".into(), level: level(6, 0.7) });
        assert_eq!(m.current_level(1, "spk").unwrap().volume, Some(0.7));
        // Nothing from the agent before counts for the next one
        assert_eq!(m.current_level(2, "spk"), None);
        // A new agent starts counting anew
        m.event(2, &Event::Level { device: "spk".into(), level: level(1, 0.3) });
        assert_eq!(m.current_level(2, "spk").unwrap().volume, Some(0.3));
        // Devices nobody watches are not kept
        m.event(2, &Event::Level { device: "other".into(), level: level(9, 0.1) });
        assert_eq!(m.current_level(2, "other"), None);
    }

    #[test]
    fn mute_keys_count_only_for_held_outputs() {
        let mut m = Mirror::default();
        let key = Event::MuteKey { device: "spk".into(), volume_changed: false };
        assert!(!m.event(1, &key));
        m.holds.insert("spk".into(), Want::Muted);
        assert!(m.event(1, &key));
        // Let go of (an unmute on its way): a late key is not taken
        m.holds.insert("spk".into(), Want::Unmuted);
        assert!(!m.event(1, &key));
    }

    #[test]
    fn the_default_is_kept_only_while_watched() {
        let mut m = Mirror::default();
        m.event(1, &Event::Default { device: Some("a".into()) });
        assert_eq!(m.default, None);
        m.default_watched = true;
        m.event(1, &Event::Default { device: Some("b".into()) });
        assert_eq!(m.default, Some((1, Some("b".into()))));
    }

    #[test]
    fn the_local_host_reads_windows() {
        assert_eq!(LocalHost.volume_default_output(), audio_capture::default_render_device());
        assert!(!LocalHost.host_keeps_mute());
        assert!(LocalHost.open_output_volume("no such device").is_err());
        if let Some(id) = audio_capture::default_render_device() {
            let v = LocalHost.open_output_volume(&id).unwrap();
            assert_eq!(v.device_id(), id);
            assert!(!v.keeps_mute());
            // Read only: this PC's volume and mute stay as they are
            let direct = audio_capture::EndpointVolume::open(Some(&id)).unwrap();
            assert_eq!(v.muted(), direct.muted().ok());
        }
    }
}
