//! Group `media`: the Windows media session for LE Audio's media control
//! (MCP: what plays, the headset's Play / Pause / Next ... commands) and
//! the media keys of the headphones' AVRCP buttons. Both act in the
//! signed-in user's session (System Media Transport Controls, SendInput).
//!
//! With the local host the core keeps [`smtc::WindowsMedia`] (its own
//! thread reads the session and carries out the commands) and sends media
//! keys itself, as before. With the agent the core's backend is a
//! [`RemoteMedia`]: bt-host calls it on the BTstack thread, so nothing in
//! it waits for the agent.
//!
//! - [`MediaBackend::info`] answers from a cached state that the agent's
//!   events ([`Changed`]) update;
//! - commands and seeks go to a thread of the backend's own
//!   ("mcp-media-agent"), which sends them as requests and hands the result
//!   to the module's `done`;
//! - that thread also watches the agent: when one connects it asks it to
//!   watch the media session ([`Request::Watch`]: the agent's handler
//!   starts its own [`smtc::WindowsMedia`], or le-audio's simulated player
//!   in a dry run) and takes the state from the reply. When the agent goes,
//!   the cached state becomes "no player" until the next agent's watch.
//! - a media key is an event to the agent ([`Request::Key`]), sent without
//!   waiting.
//!
//! When the core goes, the agent's handler is dropped and its watch ends;
//! nothing else is held for the user.

pub(crate) mod smtc;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use le_audio::mcp_model::{self, ChangedFn, DoneFn, RESULT_COMMAND_CANNOT_BE_COMPLETED, RESULT_MEDIA_PLAYER_INACTIVE};
/// The backend's trait and state, as bt-host takes them
pub use le_audio::mcp_model::{MediaBackend, MediaInfo};
use serde::{Deserialize, Serialize};

use crate::hub::Hub;
use crate::wire::{decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "media";

/// How often the backend's thread looks whether the agent changed
const AGENT_CHECK: Duration = Duration::from_millis(500);
/// A watch the agent refused is asked for again after
const WATCH_RETRY: Duration = Duration::from_secs(5);

/// A media key (win_sys::media_keys::MediaKey on the wire)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    PlayPause,
    Stop,
    Next,
    Previous,
}

impl From<Key> for win_sys::media_keys::MediaKey {
    fn from(k: Key) -> Self {
        use win_sys::media_keys::MediaKey as M;
        match k {
            Key::PlayPause => M::PlayPause,
            Key::Stop => M::Stop,
            Key::Next => M::Next,
            Key::Previous => M::Previous,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Starts watching the media session (a watch before it ends). Reply:
    /// [`Changed`], the state now; later states come as [`Changed`] events
    Watch { watch: u64, simulated: bool },
    /// Ends a watch (sent without waiting; another watch's id is ignored).
    /// Reply: null
    Unwatch { watch: u64 },
    /// A Media Control Point opcode. Reply: the MCS result code (u8)
    Command { watch: u64, opcode: u8, param: i32 },
    /// A Track Position write (0.01 s). Reply: null
    Seek { watch: u64, position: i32 },
    /// A media key, as an event (no reply). `dry_run`: only logged
    Key { key: Key, dry_run: bool },
}

/// [`MediaInfo`] on the wire: its time as an age, since each process has
/// its own clock ([`mcp_model::now_ms`])
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireInfo {
    pub session: bool,
    pub state: u8,
    pub player: String,
    pub title: String,
    pub duration: i32,
    pub position: i32,
    /// How long ago `position` was read (ms), None: never
    pub age_ms: Option<u64>,
    pub rate: f64,
    pub can_seek: bool,
    pub track: u32,
}

impl WireInfo {
    /// `mi` as of `now_ms` (the sender's clock)
    pub fn from_info(mi: &MediaInfo, now_ms: u64) -> WireInfo {
        WireInfo {
            session: mi.session,
            state: mi.state,
            player: mi.player.clone(),
            title: mi.title.clone(),
            duration: mi.duration,
            position: mi.position,
            age_ms: (mi.updated_ms > 0).then(|| now_ms.saturating_sub(mi.updated_ms)),
            rate: mi.rate,
            can_seek: mi.can_seek,
            track: mi.track,
        }
    }

    /// The state on the receiver's clock (`now_ms`)
    pub fn to_info(&self, now_ms: u64) -> MediaInfo {
        let mut mi = MediaInfo {
            session: self.session,
            state: self.state,
            player: self.player.clone(),
            title: self.title.clone(),
            duration: self.duration,
            position: self.position,
            updated_ms: 0,
            rate: self.rate,
            can_seek: self.can_seek,
            track: self.track,
        };
        if let Some(age) = self.age_ms {
            if age < now_ms {
                mi.updated_ms = now_ms - age;
            } else {
                // Older than this process's clock: move the position on now
                mi.updated_ms = now_ms.max(1);
                mi.position = MediaInfo { updated_ms: 0, ..mi.clone() }.position_now(age);
            }
        }
        mi
    }
}

/// A watch's state: the reply of [`Request::Watch`] and its events
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    pub watch: u64,
    /// Counts the watch's states: an older one never replaces a newer
    pub seq: u64,
    pub info: WireInfo,
}

pub trait MediaHost {
    /// The LE Audio media control backend for the next stack. `dry_run`
    /// (`--mcp-dry-run`, `--hci-tcp`): nothing in Windows is read or
    /// controlled; the local host then gives None (bt-host's simulated
    /// player), the agent runs the simulated player itself.
    fn media_backend(&self, dry_run: bool) -> Option<Box<dyn MediaBackend>>;
    /// Presses and releases a media key in the user's session. Never
    /// blocks on the agent (an event). `dry_run`: only logged.
    fn send_media_key(&self, key: Key, dry_run: bool);
}

/// [`MediaHost::media_backend`] of this process's host
pub fn media_backend(dry_run: bool) -> Option<Box<dyn MediaBackend>> {
    crate::host().media_backend(dry_run)
}

/// [`MediaHost::send_media_key`] of this process's host
pub fn send_media_key(key: Key, dry_run: bool) {
    crate::host().send_media_key(key, dry_run)
}

/// A media key in this process: SendInput, or only logged
pub(crate) fn press_key(key: Key, dry_run: bool) {
    if dry_run {
        tracing::info!("media key {key:?}: dry run, not sent");
        return;
    }
    if let Err(e) = win_sys::media_keys::send(key.into()) {
        tracing::warn!("media key {key:?}: {e}");
    }
}

impl MediaHost for LocalHost {
    fn media_backend(&self, dry_run: bool) -> Option<Box<dyn MediaBackend>> {
        if dry_run { None } else { Some(Box::new(smtc::WindowsMedia::default())) }
    }

    fn send_media_key(&self, key: Key, dry_run: bool) {
        press_key(key, dry_run);
    }
}

impl MediaHost for RemoteHost {
    fn media_backend(&self, dry_run: bool) -> Option<Box<dyn MediaBackend>> {
        Some(Box::new(RemoteMedia::new(self.hub().clone(), dry_run)))
    }

    fn send_media_key(&self, key: Key, dry_run: bool) {
        static NOT_SENT: AtomicU64 = AtomicU64::new(0);
        if !self.hub().event(GROUP, to_body(&Request::Key { key, dry_run })) {
            let n = NOT_SENT.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!("media key {key:?}: no session agent connected, dropped ({n} so far)");
        }
    }
}

// --- The core's backend with the agent ----------------------------------------

enum Task {
    Command(u8, i32, DoneFn),
    Seek(i32),
}

/// The agent's watch: its id and the agent it was asked of
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Watching {
    id: u64,
    generation: u64,
}

struct State {
    stopping: bool,
    tasks: VecDeque<Task>,
    info: MediaInfo,
    /// The newest [`Changed::seq`] taken
    seq: u64,
    watching: Option<Watching>,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    changed: ChangedFn,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A state from the agent: taken if it is of the current watch and
    /// newer than the one held; then `changed`
    fn take(&self, c: &Changed) -> bool {
        {
            let mut s = self.lock();
            if s.stopping || s.watching.map(|w| w.id) != Some(c.watch) || c.seq <= s.seq {
                return false;
            }
            s.seq = c.seq;
            s.info = c.info.to_info(mcp_model::now_ms());
        }
        (self.changed)();
        true
    }

    /// The agent went: no player until the next watch
    fn lost(&self) {
        {
            let mut s = self.lock();
            s.watching = None;
            if s.info == MediaInfo::default() {
                return;
            }
            s.info = MediaInfo::default();
        }
        (self.changed)();
    }
}

static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

/// The backend whose events the hub hands on (one MCP module at a time)
fn active() -> &'static Mutex<Weak<Shared>> {
    static ACTIVE: OnceLock<Mutex<Weak<Shared>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Weak::new()))
}

/// Subscribes to the group's events on `hub` (again: the same handler)
fn listen(hub: &Hub) {
    // On the hub's thread: takes a lock held only briefly, never waits
    hub.on_event(GROUP, |body| {
        let Ok(c) = serde_json::from_value::<Changed>(body) else {
            tracing::debug!("MCP media: an event that does not decode, ignored");
            return;
        };
        let target = active().lock().unwrap_or_else(|e| e.into_inner()).upgrade();
        if let Some(shared) = target {
            shared.take(&c);
        }
    });
}

/// The MCP backend of a core whose session host is the agent
pub struct RemoteMedia {
    hub: Arc<Hub>,
    simulated: bool,
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteMedia {
    pub fn new(hub: Arc<Hub>, simulated: bool) -> RemoteMedia {
        RemoteMedia { hub, simulated, shared: None, thread: None }
    }

    fn post(&self, task: Task) -> Result<(), Task> {
        let Some(shared) = &self.shared else { return Err(task) };
        {
            let mut s = shared.lock();
            if s.stopping {
                return Err(task);
            }
            s.tasks.push_back(task);
        }
        shared.cv.notify_all();
        Ok(())
    }
}

impl MediaBackend for RemoteMedia {
    fn start(&mut self, changed: ChangedFn) {
        self.stop();
        listen(&self.hub);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                stopping: false,
                tasks: VecDeque::new(),
                info: MediaInfo::default(),
                seq: 0,
                watching: None,
            }),
            cv: Condvar::new(),
            changed,
        });
        *active().lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&shared);
        let worker = Worker { hub: self.hub.clone(), shared: shared.clone(), simulated: self.simulated };
        self.thread = std::thread::Builder::new()
            .name("mcp-media-agent".into())
            .spawn(move || worker.run())
            .map_err(|e| tracing::warn!("MCP: cannot start the media thread: {e}"))
            .ok();
        self.shared = Some(shared);
        tracing::debug!("MCP media: through the session agent (dry run {})", self.simulated);
    }

    /// Never waits: the thread ends by itself after the request it may be
    /// in (up to the hub's timeout)
    fn stop(&mut self) {
        let Some(shared) = self.shared.take() else { return };
        let watching = {
            let mut s = shared.lock();
            tracing::debug!("MCP media: stopping, {} task(s) dropped", s.tasks.len());
            s.stopping = true;
            s.tasks.clear();
            s.info = MediaInfo::default();
            s.watching.take()
        };
        shared.cv.notify_all();
        if let Some(w) = watching {
            self.hub.notify(w.generation, GROUP, to_body(&Request::Unwatch { watch: w.id }));
        }
        let mut active = active().lock().unwrap_or_else(|e| e.into_inner());
        if active.ptr_eq(&Arc::downgrade(&shared)) {
            *active = Weak::new();
        }
        drop(active);
        // Detached; it holds its own reference
        self.thread = None;
    }

    fn dry_run(&self) -> bool {
        self.simulated
    }

    fn info(&self) -> MediaInfo {
        self.shared.as_ref().map(|s| s.lock().info.clone()).unwrap_or_default()
    }

    fn command(&mut self, opcode: u8, param: i32, done: DoneFn) {
        tracing::debug!("MCP media: command {} (param {param}) to the agent", mcp_model::opcode_name(opcode));
        if let Err(Task::Command(_, _, done)) = self.post(Task::Command(opcode, param, done)) {
            done(RESULT_MEDIA_PLAYER_INACTIVE);
        }
    }

    fn seek(&mut self, position: i32) {
        tracing::debug!("MCP media: seek to {position} cs to the agent");
        let _ = self.post(Task::Seek(position));
    }
}

impl Drop for RemoteMedia {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The backend's thread: watches the agent, sends the commands
struct Worker {
    hub: Arc<Hub>,
    shared: Arc<Shared>,
    simulated: bool,
}

impl Worker {
    fn run(self) {
        let mut next_try = Instant::now();
        let mut failures = 0u32;
        loop {
            let tasks: Vec<Task> = {
                let guard = self.shared.lock();
                let (mut s, _) = self
                    .shared
                    .cv
                    .wait_timeout_while(guard, AGENT_CHECK, |s| !s.stopping && s.tasks.is_empty())
                    .unwrap_or_else(|e| e.into_inner());
                if s.stopping {
                    break;
                }
                s.tasks.drain(..).collect()
            };
            let agent = self.hub.agent().map(|a| a.generation);
            let watching = self.shared.lock().watching;
            if let Some(w) = watching
                && agent != Some(w.generation)
            {
                tracing::info!("MCP: the session agent went, no media player until the next one");
                self.shared.lost();
                next_try = Instant::now();
            }
            if self.shared.lock().watching.is_none() && agent.is_some() && Instant::now() >= next_try {
                match self.watch() {
                    Ok(()) => failures = 0,
                    Err(e) => {
                        failures += 1;
                        if failures == 1 {
                            tracing::warn!("MCP: the session agent does not watch the media session: {e}");
                        }
                        next_try = Instant::now() + WATCH_RETRY;
                    }
                }
            }
            for t in tasks {
                self.carry_out(t);
            }
        }
    }

    /// Asks the connected agent to watch, and takes the state it answers
    fn watch(&self) -> Result<(), HostError> {
        let id = NEXT_WATCH.fetch_add(1, Ordering::Relaxed);
        let request = Request::Watch { watch: id, simulated: self.simulated };
        let Some(agent) = self.hub.agent() else { return Err(HostError::NoAgent) };
        {
            // Before the request: its events may come before the reply
            let mut s = self.shared.lock();
            if s.stopping {
                return Ok(());
            }
            s.watching = Some(Watching { id, generation: agent.generation });
            s.seq = 0;
        }
        let result = self
            .hub
            .request_on(GROUP, to_body(&request), self.hub.request_timeout())
            .and_then(|(v, a)| Ok((decode_reply::<Changed>(v)?, a)));
        match result {
            Ok((changed, a)) if a.generation == agent.generation => {
                self.shared.take(&changed);
                tracing::info!("MCP: watching the media session through the session agent (agent {})", a.generation);
                Ok(())
            }
            Ok(_) => {
                self.shared.lock().watching = None;
                Err(HostError::AgentLost)
            }
            Err(e) => {
                self.shared.lock().watching = None;
                Err(e)
            }
        }
    }

    fn carry_out(&self, task: Task) {
        let watching = self.shared.lock().watching;
        match task {
            Task::Command(opcode, param, done) => {
                let Some(w) = watching else {
                    tracing::info!("MCP: {}: no session agent", mcp_model::opcode_name(opcode));
                    return done(RESULT_MEDIA_PLAYER_INACTIVE);
                };
                let request = Request::Command { watch: w.id, opcode, param };
                let result = self.hub.request(GROUP, to_body(&request)).and_then(decode_reply::<u8>);
                let code = match result {
                    Ok(code) => code,
                    Err(HostError::Timeout) => RESULT_COMMAND_CANNOT_BE_COMPLETED,
                    Err(_) => RESULT_MEDIA_PLAYER_INACTIVE,
                };
                tracing::debug!(
                    "MCP media: {} -> {} (agent)",
                    mcp_model::opcode_name(opcode),
                    mcp_model::result_name(code)
                );
                done(code);
            }
            Task::Seek(position) => {
                let Some(w) = watching else { return };
                if let Err(e) = self.hub.request(GROUP, to_body(&Request::Seek { watch: w.id, position })) {
                    tracing::debug!("MCP media: seek not carried out: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use le_audio::mcp_model::{STATE_PAUSED, STATE_PLAYING};

    #[test]
    fn requests_have_a_stable_shape() {
        let k = Request::Key { key: Key::PlayPause, dry_run: true };
        assert_eq!(serde_json::to_string(&k).unwrap(), r#"{"op":"key","key":"play_pause","dry_run":true}"#);
        let w = Request::Watch { watch: 3, simulated: false };
        assert_eq!(serde_json::to_string(&w).unwrap(), r#"{"op":"watch","watch":3,"simulated":false}"#);
        let c = Request::Command { watch: 3, opcode: 1, param: 0 };
        assert_eq!(serde_json::from_str::<Request>(&serde_json::to_string(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn the_position_keeps_its_age_across_clocks() {
        let mi = MediaInfo {
            session: true,
            state: STATE_PLAYING,
            player: "p".into(),
            duration: 18000,
            position: 1000,
            updated_ms: 5_000,
            ..MediaInfo::default()
        };
        // Read 2 s ago on the agent's clock
        let wire = WireInfo::from_info(&mi, 7_000);
        assert_eq!(wire.age_ms, Some(2_000));
        let back = wire.to_info(100_000);
        assert_eq!(back.updated_ms, 98_000);
        assert_eq!(back.position_now(100_000), 1200);
        // The receiver's clock is younger than the age: position moved on
        let young = wire.to_info(500);
        assert_eq!(young.position_now(young.updated_ms), 1200);
        // Never read
        let none = WireInfo::from_info(&MediaInfo::default(), 7_000);
        assert_eq!(none.age_ms, None);
        assert_eq!(none.to_info(9_000), MediaInfo::default());
    }

    #[test]
    fn only_newer_states_of_the_current_watch_are_taken() {
        let changes = Arc::new(AtomicU64::new(0));
        let c = changes.clone();
        let shared = Shared {
            state: Mutex::new(State {
                stopping: false,
                tasks: VecDeque::new(),
                info: MediaInfo::default(),
                seq: 0,
                watching: Some(Watching { id: 7, generation: 1 }),
            }),
            cv: Condvar::new(),
            changed: Box::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }),
        };
        let info = |state| WireInfo::from_info(&MediaInfo { session: true, state, ..MediaInfo::default() }, 0);
        assert!(shared.take(&Changed { watch: 7, seq: 2, info: info(STATE_PLAYING) }));
        assert!(!shared.take(&Changed { watch: 7, seq: 1, info: info(STATE_PAUSED) }));
        assert!(!shared.take(&Changed { watch: 6, seq: 9, info: info(STATE_PAUSED) }));
        assert_eq!(shared.lock().info.state, STATE_PLAYING);
        shared.lost();
        assert_eq!(shared.lock().info, MediaInfo::default());
        assert!(shared.lock().watching.is_none());
        // Nothing changes on a second loss
        shared.lost();
        assert_eq!(changes.load(Ordering::Relaxed), 2);
    }
}
