//! Group `media` in the agent: one watch of the media session at a time
//! (Windows' through [`WindowsMedia`], or le-audio's simulated player in a
//! dry run) whose states go to the core as events, its commands, and the
//! media keys. Dropping the handler (the core went) ends the watch.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use le_audio::mcp_model::{
    self, MediaBackend, RESULT_COMMAND_CANNOT_BE_COMPLETED, RESULT_MEDIA_PLAYER_INACTIVE, SimulatedPlayer,
};
use serde_json::Value;

use crate::host::media::smtc::WindowsMedia;
use crate::host::media::{Changed, GROUP, Key, Request, WireInfo, press_key};
use crate::wire::{WireError, decode, reply, to_body};
use crate::{AgentContext, Handler};

/// A command's result is waited for this long (within the hub's timeout)
const COMMAND_WAIT: Duration = Duration::from_secs(8);
/// The publisher looks this often whether its watch ended
const PUBLISH_CHECK: Duration = Duration::from_millis(200);

type Emit = Arc<dyn Fn(Value) + Send + Sync>;
type MakeBackend = Box<dyn Fn(bool) -> Box<dyn MediaBackend> + Send>;
type PressKey = Box<dyn Fn(Key, bool) + Send>;

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    let emit: Emit = Arc::new(move |body| {
        ctx.event(GROUP, body);
    });
    Box::new(Media::new(emit, Box::new(backend), Box::new(press_key)))
}

/// The simulated player in a dry run, else Windows' media session
fn backend(simulated: bool) -> Box<dyn MediaBackend> {
    if simulated { Box::new(SimulatedPlayer::new()) } else { Box::new(WindowsMedia::default()) }
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Media {
    emit: Emit,
    make: MakeBackend,
    press: PressKey,
    watch: Option<Watch>,
}

impl Media {
    fn new(emit: Emit, make: MakeBackend, press: PressKey) -> Media {
        Media { emit, make, press, watch: None }
    }

    /// The backend of watch `id`, if it is the current one
    fn backend(&self, id: u64) -> Option<&Arc<Mutex<Box<dyn MediaBackend>>>> {
        self.watch.as_ref().filter(|w| w.id == id).map(|w| &w.backend)
    }
}

impl Handler for Media {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::Watch { watch, simulated } => {
                // The old one ends first (one session watch at a time)
                self.watch = None;
                let (w, now) = Watch::start(watch, (self.make)(simulated), self.emit.clone());
                self.watch = Some(w);
                tracing::info!("media: watching the media session (dry run {simulated})");
                reply(now)
            }
            Request::Unwatch { watch } => {
                if self.backend(watch).is_some() {
                    self.watch = None;
                    tracing::info!("media: the watch ended");
                }
                reply(())
            }
            Request::Command { watch, opcode, param } => {
                let Some(backend) = self.backend(watch) else { return reply(RESULT_MEDIA_PLAYER_INACTIVE) };
                let (tx, rx) = mpsc::sync_channel(1);
                // The simulated player answers inside the call, Windows'
                // from its thread
                lock(backend).command(
                    opcode,
                    param,
                    Box::new(move |r| {
                        let _ = tx.try_send(r);
                    }),
                );
                let code = rx.recv_timeout(COMMAND_WAIT).unwrap_or_else(|_| {
                    tracing::warn!("media: {} got no result in time", mcp_model::opcode_name(opcode));
                    RESULT_COMMAND_CANNOT_BE_COMPLETED
                });
                reply(code)
            }
            Request::Seek { watch, position } => {
                if let Some(backend) = self.backend(watch) {
                    lock(backend).seek(position);
                }
                reply(())
            }
            Request::Key { key, dry_run } => {
                (self.press)(key, dry_run);
                reply(())
            }
        }
    }

    fn event(&mut self, body: Value) {
        match decode(body) {
            Ok(Request::Key { key, dry_run }) => (self.press)(key, dry_run),
            Ok(_) => tracing::debug!("media: an event that is not a key, ignored"),
            Err(_) => tracing::debug!("media: an event that does not decode, ignored"),
        }
    }
}

/// One watch: the backend, and a thread that sends each of its states
struct Watch {
    id: u64,
    backend: Arc<Mutex<Box<dyn MediaBackend>>>,
    closing: Arc<AtomicBool>,
    publisher: Option<JoinHandle<()>>,
}

/// The backend's state now, numbered (under the backend's lock: states
/// are numbered in the order they were read)
fn snapshot(id: u64, backend: &Mutex<Box<dyn MediaBackend>>, seq: &AtomicU64) -> Changed {
    let b = lock(backend);
    let info = b.info();
    let seq = seq.fetch_add(1, Ordering::Relaxed) + 1;
    Changed { watch: id, seq, info: WireInfo::from_info(&info, mcp_model::now_ms()) }
}

impl Watch {
    /// Starts `backend`; also the state right after the start
    fn start(id: u64, backend: Box<dyn MediaBackend>, emit: Emit) -> (Watch, Changed) {
        let backend = Arc::new(Mutex::new(backend));
        let seq = Arc::new(AtomicU64::new(0));
        let closing = Arc::new(AtomicBool::new(false));
        let (ping, rx) = mpsc::channel::<()>();
        let (b, s, c) = (backend.clone(), seq.clone(), closing.clone());
        let publisher = std::thread::Builder::new()
            .name("agent-media-publish".into())
            .spawn(move || {
                while !c.load(Ordering::Acquire) {
                    match rx.recv_timeout(PUBLISH_CHECK) {
                        Ok(()) => {
                            // Many changes at once: one state
                            while rx.try_recv().is_ok() {}
                            let changed = snapshot(id, &b, &s);
                            if !c.load(Ordering::Acquire) {
                                emit(to_body(&changed));
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| tracing::warn!("media: cannot start the publisher: {e}"))
            .ok();
        let ping = Mutex::new(ping);
        lock(&backend).start(Box::new(move || {
            let _ = lock(&ping).send(());
        }));
        let now = snapshot(id, &backend, &seq);
        (Watch { id, backend, closing, publisher }, now)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Release);
        lock(&self.backend).stop();
        if let Some(t) = self.publisher.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use le_audio::mcp_model::{
        ChangedFn, DoneFn, MediaInfo, OP_NEXT_TRACK, OP_PLAY, RESULT_SUCCESS, SIM_PLAYER, STATE_PAUSED, STATE_PLAYING,
    };
    use serde_json::json;

    /// A backend that only records what it was asked
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        info: MediaInfo,
        changed: Option<ChangedFn>,
    }

    impl MediaBackend for Fake {
        fn start(&mut self, changed: ChangedFn) {
            lock(&self.log).push("start".into());
            self.info = MediaInfo { session: true, state: STATE_PAUSED, player: "fake".into(), ..MediaInfo::default() };
            self.changed = Some(changed);
        }
        fn stop(&mut self) {
            lock(&self.log).push("stop".into());
            self.changed = None;
        }
        fn dry_run(&self) -> bool {
            false
        }
        fn info(&self) -> MediaInfo {
            self.info.clone()
        }
        fn command(&mut self, opcode: u8, _param: i32, done: DoneFn) {
            lock(&self.log).push(format!("command {}", mcp_model::opcode_name(opcode)));
            self.info.state = STATE_PLAYING;
            if let Some(c) = &self.changed {
                c();
            }
            // From another thread, as Windows' backend does
            std::thread::spawn(move || done(RESULT_SUCCESS));
        }
        fn seek(&mut self, position: i32) {
            lock(&self.log).push(format!("seek {position}"));
        }
    }

    type Events = mpsc::Receiver<Changed>;

    fn handler(
        fake_log: &Arc<Mutex<Vec<String>>>,
        keys: &Arc<Mutex<Vec<(Key, bool)>>>,
    ) -> (Media, Events, Arc<Mutex<Vec<bool>>>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let emit: Emit = Arc::new(move |body| {
            let _ = lock(&tx).send(serde_json::from_value::<Changed>(body).unwrap());
        });
        let made = Arc::new(Mutex::new(Vec::new()));
        let (log, m) = (fake_log.clone(), made.clone());
        let make: MakeBackend = Box::new(move |simulated| -> Box<dyn MediaBackend> {
            lock(&m).push(simulated);
            if simulated {
                Box::new(SimulatedPlayer::new())
            } else {
                Box::new(Fake { log: log.clone(), info: MediaInfo::default(), changed: None })
            }
        });
        let k = keys.clone();
        let press: PressKey = Box::new(move |key, dry_run| lock(&k).push((key, dry_run)));
        (Media::new(emit, make, press), rx, made)
    }

    fn call(h: &mut Media, body: Value) -> Value {
        h.request(body).unwrap()
    }

    /// The next state of watch `watch` that satisfies `f`
    fn wait_state(rx: &Events, watch: u64, f: impl Fn(&WireInfo) -> bool) -> Changed {
        loop {
            let c = rx.recv_timeout(Duration::from_secs(5)).expect("a state event");
            if c.watch == watch && f(&c.info) {
                return c;
            }
        }
    }

    #[test]
    fn a_dry_run_watch_runs_the_simulated_player() {
        let (log, keys) = (Arc::default(), Arc::default());
        let (mut h, rx, made) = handler(&log, &keys);
        let now: Changed =
            serde_json::from_value(call(&mut h, json!({"op": "watch", "watch": 4, "simulated": true}))).unwrap();
        assert_eq!(*lock(&made), [true]);
        assert_eq!((now.watch, now.info.player.as_str(), now.info.state), (4, SIM_PLAYER, STATE_PAUSED));
        assert!(now.info.title.starts_with("Test track 1"));
        let r = call(&mut h, json!({"op": "command", "watch": 4, "opcode": OP_PLAY, "param": 0}));
        assert_eq!(r, json!(RESULT_SUCCESS));
        let playing = wait_state(&rx, 4, |i| i.state == STATE_PLAYING);
        assert!(playing.seq > now.seq);
        assert_eq!(call(&mut h, json!({"op": "command", "watch": 4, "opcode": OP_NEXT_TRACK, "param": 0})), json!(1));
        wait_state(&rx, 4, |i| i.title.starts_with("Test track 2"));
        call(&mut h, json!({"op": "seek", "watch": 4, "position": 1500}));
        wait_state(&rx, 4, |i| i.position == 1500);
        // Another watch's command: no player
        let r = call(&mut h, json!({"op": "command", "watch": 3, "opcode": OP_PLAY, "param": 0}));
        assert_eq!(r, json!(RESULT_MEDIA_PLAYER_INACTIVE));
        // Another watch's end is ignored, its own ends it
        call(&mut h, json!({"op": "unwatch", "watch": 3}));
        assert!(h.watch.is_some());
        call(&mut h, json!({"op": "unwatch", "watch": 4}));
        assert!(h.watch.is_none());
        let r = call(&mut h, json!({"op": "command", "watch": 4, "opcode": OP_PLAY, "param": 0}));
        assert_eq!(r, json!(RESULT_MEDIA_PLAYER_INACTIVE));
        assert!(lock(&keys).is_empty());
    }

    #[test]
    fn a_watch_ends_with_the_handler_and_with_the_next_watch() {
        let (log, keys) = (Arc::default(), Arc::default());
        let (mut h, rx, made) = handler(&log, &keys);
        let now: Changed =
            serde_json::from_value(call(&mut h, json!({"op": "watch", "watch": 1, "simulated": false}))).unwrap();
        assert_eq!(now.info.player, "fake");
        let r = call(&mut h, json!({"op": "command", "watch": 1, "opcode": OP_PLAY, "param": 0}));
        assert_eq!(r, json!(RESULT_SUCCESS));
        wait_state(&rx, 1, |i| i.state == STATE_PLAYING);
        call(&mut h, json!({"op": "seek", "watch": 1, "position": 30}));
        // A new watch replaces it
        call(&mut h, json!({"op": "watch", "watch": 2, "simulated": false}));
        assert_eq!(*lock(&log), ["start", "command PLAY", "seek 30", "stop", "start"]);
        // The core went: the handler drops, the watch ends
        drop(h);
        assert_eq!(lock(&log).last().map(String::as_str), Some("stop"));
        assert_eq!(*lock(&made), [false, false]);
    }

    #[test]
    fn media_keys_are_pressed_as_asked() {
        let (log, keys) = (Arc::default(), Arc::default());
        let (mut h, _rx, _) = handler(&log, &keys);
        h.event(json!({"op": "key", "key": "play_pause", "dry_run": true}));
        h.event(json!({"op": "key", "key": "next", "dry_run": false}));
        h.event(json!({"op": "nonsense"}));
        call(&mut h, json!({"op": "key", "key": "stop", "dry_run": true}));
        assert_eq!(*lock(&keys), [(Key::PlayPause, true), (Key::Next, false), (Key::Stop, true)]);
        assert!(lock(&log).is_empty());
    }
}
