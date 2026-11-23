//! The core's side: [`HelperSink`], the HID engine's sink when input goes
//! through the helper.
//!
//! The BTstack thread only queues ([`crate::queue`], a short lock) and
//! wakes the sender thread. The sender thread (`input-helper`, its own
//! single-threaded runtime) owns the helper: it makes a pipe with a fresh
//! random name that only SYSTEM and this process's user may open, starts
//! the helper through [`Launcher`] (in a service: as SYSTEM in the console
//! session), accepts the connection only from that very process, writes
//! the queue's frames and reads the helper's acks, lock keys, desktop
//! states and Ctrl+Alt+Del requests.
//!
//! Why this pipe and not handles duplicated into the helper: the helper is
//! created by session_launch, which inherits no handles into a process of
//! another session; a pipe the core creates before the helper starts
//! cannot be taken over (first instance, protected DACL, a name nobody
//! can guess), and both ends check the other's process id with Windows
//! (the helper: its parent; the core: the process it started).
//!
//! When the helper dies, is killed or hangs (no ack for
//! [`ACK_TIMEOUT`]), input is dropped and counted until the next one
//! runs, and the next one first releases everything the old one may have
//! held ([`crate::queue::SendQueue::connect`]). When the console session
//! changes (fast user switching, sign-out), the helper is told to release
//! and stop, and one is started in the new session; while no session is
//! at the console, no helper runs and input is dropped.

use std::io;
use std::os::windows::io::AsHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hid_input::sink::{Delivery, DeliveryLog, HidCounters, InputSink, SELF_TEST_INPUT};
pub use hid_input::sink::{SelfTest, SelfTestFailure};
use tokio::io::AsyncWriteExt;
use win_sys::input_inject::Inject;

use crate::helper::{HELPER_ARG, PIPE_PREFIX, RECORD_ARG};
use crate::protocol::{self, DesktopState, ToCore, ToHelper};
use crate::queue::{Origin, SendQueue, UNCONFIRMED_MAX};

/// How long the helper may take to connect and say hello
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// A helper that confirms nothing this long while items wait is hung
pub const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// A frame the helper does not read within this is a hung helper
const WRITE_WAIT: Duration = Duration::from_secs(2);
/// How long a helper told to stop may take to exit
const STOP_WAIT: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(250);
const BACKOFF_FIRST: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// A helper that ran this long resets the backoff
const HEALTHY: Duration = Duration::from_secs(30);

/// A helper process the core started
pub trait HelperProcess: Send {
    fn pid(&self) -> u32;
    fn exited(&self) -> bool;
    fn kill(&self);
}

impl HelperProcess for win_sys::session_launch::Child {
    fn pid(&self) -> u32 {
        win_sys::session_launch::Child::pid(self)
    }

    fn exited(&self) -> bool {
        !matches!(self.try_wait(), Ok(None))
    }

    fn kill(&self) {
        let _ = win_sys::session_launch::Child::kill(self);
    }
}

/// Starts helpers, and says in which session one belongs
pub trait Launcher: Send + Sync + 'static {
    /// The session a helper must run in now; None: none (nobody at the
    /// console)
    fn session(&self) -> Option<u32>;
    fn launch(&self, exe: &Path, args: Vec<String>, session: u32) -> io::Result<Box<dyn HelperProcess>>;
}

/// The real one: win_sys::session_launch with [`Identity::System`]. In a
/// service, the console session; as an ordinary process, its own session
/// (the helper is then a plain child running as this user).
///
/// [`Identity::System`]: win_sys::session_launch::Identity::System
pub struct SessionLauncher;

impl Launcher for SessionLauncher {
    fn session(&self) -> Option<u32> {
        if win_sys::session_launch::in_service_session() {
            win_sys::session_launch::active_session()
        } else {
            win_sys::session_launch::own_session()
        }
    }

    fn launch(&self, exe: &Path, args: Vec<String>, session: u32) -> io::Result<Box<dyn HelperProcess>> {
        let l = win_sys::session_launch::Launch {
            exe: exe.to_path_buf(),
            args,
            session,
            identity: win_sys::session_launch::Identity::System,
        };
        Ok(Box::new(win_sys::session_launch::launch(&l)?))
    }
}

/// What the sink starts
#[derive(Debug, Clone)]
pub struct HelperConfig {
    /// bluecaster-core.exe
    pub exe: PathBuf,
    /// Debug builds only: the helper records to this inherited pipe handle
    /// instead of passing input to Windows ([`RECORD_ARG`])
    pub record: Option<usize>,
}

/// Called when the user pressed Ctrl+Alt+Del on a keyboard of ours
pub type SasFn = Box<dyn Fn() + Send + Sync>;
/// Called with the self-test of each helper after the first one (a new
/// session, a helper that died)
pub type TestedFn = Box<dyn Fn(SelfTest) + Send + Sync>;

/// How long a self-test waits for Windows' last input after the helper
/// took the test input
const SEEN_WAIT: Duration = Duration::from_millis(400);
/// How long the self-test of a new helper waits
const NEW_HELPER_TEST_WAIT: Duration = Duration::from_secs(5);

/// The self-test running now (one at a time)
#[derive(Default)]
struct TestSlot {
    /// The test input is queued, its ack awaited
    waiting: bool,
    /// Its outcome: Windows took it (true) or refused it
    taken: Option<bool>,
    /// The ack's item count that covered it
    seq: u64,
    /// Windows registered input after it
    seen: bool,
}

struct Queued {
    queue: SendQueue,
    /// The running stack's counters (from its last pass)
    counters: Option<Arc<HidCounters>>,
}

struct Shared {
    q: Mutex<Queued>,
    wake: tokio::sync::Notify,
    /// Bit 8: known; bits 0..2: the lock keys
    lock_keys: AtomicU16,
    stop: AtomicBool,
    connected: AtomicBool,
    /// Helpers started, in all
    started: AtomicU64,
    /// The running helper's process id (0: none)
    pid: AtomicU32,
    /// The queue overflowed once (logged once)
    overflow_logged: AtomicBool,
    on_sas: Mutex<Option<SasFn>>,
    on_tested: Mutex<Option<TestedFn>>,
    /// Helpers that connected, in all
    connections: AtomicU64,
    /// When each device's input was confirmed by a helper
    log: DeliveryLog,
    /// The helper records instead of injecting: Windows is never asked
    record: bool,
    /// The ack's item count when each device's input was confirmed last
    /// (this connection's numbers), for `ToCore::Seen`
    seqs: Mutex<std::collections::HashMap<[u8; 6], u64>>,
    test: Mutex<TestSlot>,
    test_done: std::sync::Condvar,
    /// Held for the whole of a self-test
    test_running: Mutex<()>,
}

impl Shared {
    fn queue(&self) -> std::sync::MutexGuard<'_, Queued> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn count(&self, f: impl FnOnce(&HidCounters)) {
        if let Some(c) = self.queue().counters.clone() {
            f(&c);
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, TestSlot> {
        self.test.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The helper confirmed items up to `processed`, of `origins`, with
    /// `blocked` refused among them
    fn confirmed(&self, origins: &[Origin], processed: u64, blocked: u32) {
        for o in origins {
            match o {
                Origin::Device(d) if blocked == 0 => {
                    self.log.record(*d, None, (!self.record).then_some(false));
                    self.seqs.lock().unwrap_or_else(|e| e.into_inner()).insert(*d, processed);
                }
                Origin::SelfTest => {
                    let mut t = self.slot();
                    if t.waiting && t.taken.is_none() {
                        t.taken = Some(blocked == 0);
                        t.seq = processed;
                        self.test_done.notify_all();
                    }
                }
                _ => {}
            }
        }
    }

    /// Windows registered input after items up to `processed`
    fn seen(&self, processed: u64) {
        for (d, _) in self.seqs.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, s)| **s <= processed) {
            self.log.saw(d);
        }
        let mut t = self.slot();
        if t.taken == Some(true) && t.seq <= processed {
            t.seen = true;
            self.test_done.notify_all();
        }
    }

    /// See `InputSink::self_test`; the sender thread must run
    fn self_test(&self, wait: Duration) -> SelfTest {
        let _one = self.test_running.lock().unwrap_or_else(|e| e.into_inner());
        let until = Instant::now() + wait;
        while !self.connected.load(Ordering::Acquire) {
            if Instant::now() >= until || self.stop.load(Ordering::Acquire) {
                return SelfTest::Failed(SelfTestFailure::HelperNotStarted);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        *self.slot() = TestSlot { waiting: true, ..TestSlot::default() };
        if self.queue().queue.push_from(Origin::SelfTest, &[SELF_TEST_INPUT]).dropped > 0 {
            *self.slot() = TestSlot::default();
            return SelfTest::Failed(SelfTestFailure::HelperNotStarted);
        }
        self.wake.notify_one();
        let mut t = self.slot();
        while t.taken.is_none() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                *t = TestSlot::default();
                return SelfTest::Failed(SelfTestFailure::TimedOut);
            }
            t = self.test_done.wait_timeout(t, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        if t.taken == Some(false) {
            *t = TestSlot::default();
            return SelfTest::Failed(SelfTestFailure::Refused);
        }
        let windows_saw = if self.record {
            None
        } else {
            let seen_until = Instant::now() + SEEN_WAIT;
            while !t.seen && Instant::now() < seen_until {
                let left = seen_until.saturating_duration_since(Instant::now());
                t = self.test_done.wait_timeout(t, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            Some(t.seen)
        };
        *t = TestSlot::default();
        SelfTest::Passed { windows_saw }
    }
}

/// The input helper's sink. [`HelperSink::start`] starts the sender thread
/// (and with it the helper); [`HelperSink::stop`] releases everything and
/// stops both.
pub struct HelperSink {
    shared: Arc<Shared>,
    config: HelperConfig,
    launcher: Arc<dyn Launcher>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for HelperSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperSink").field("connected", &self.connected()).finish()
    }
}

impl HelperSink {
    pub fn new(config: HelperConfig, launcher: Arc<dyn Launcher>) -> Arc<HelperSink> {
        Arc::new(HelperSink {
            shared: Arc::new(Shared {
                q: Mutex::new(Queued { queue: SendQueue::default(), counters: None }),
                wake: tokio::sync::Notify::new(),
                lock_keys: AtomicU16::new(0),
                stop: AtomicBool::new(false),
                connected: AtomicBool::new(false),
                started: AtomicU64::new(0),
                pid: AtomicU32::new(0),
                overflow_logged: AtomicBool::new(false),
                on_sas: Mutex::new(None),
                on_tested: Mutex::new(None),
                connections: AtomicU64::new(0),
                log: DeliveryLog::default(),
                record: config.record.is_some(),
                seqs: Mutex::default(),
                test: Mutex::default(),
                test_done: std::sync::Condvar::new(),
                test_running: Mutex::new(()),
            }),
            config,
            launcher,
            thread: Mutex::new(None),
        })
    }

    /// What to do when the user presses Ctrl+Alt+Del
    pub fn on_sas(&self, f: SasFn) {
        *self.shared.on_sas.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// What to do with the self-test of each helper after the first
    pub fn on_tested(&self, f: TestedFn) {
        *self.shared.on_tested.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// A helper is connected and takes input
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    /// Helpers started so far
    pub fn started(&self) -> u64 {
        self.shared.started.load(Ordering::Relaxed)
    }

    /// The running helper's process id
    pub fn helper_pid(&self) -> Option<u32> {
        Some(self.shared.pid.load(Ordering::Acquire)).filter(|&p| p != 0)
    }

    /// Starts the sender thread unless it runs
    pub fn start(&self) {
        let mut t = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if t.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        self.shared.stop.store(false, Ordering::Release);
        let (shared, config, launcher) = (self.shared.clone(), self.config.clone(), self.launcher.clone());
        match std::thread::Builder::new()
            .name("input-helper".into())
            .spawn(move || sender(&shared, &config, &*launcher))
        {
            Ok(h) => *t = Some(h),
            Err(e) => tracing::warn!("input helper: the sender thread did not start ({})", e.kind()),
        }
    }

    /// Releases everything held, stops the helper and the sender thread
    /// (waits up to about two seconds)
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_one();
        let handle = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handle {
            let until = Instant::now() + STOP_WAIT * 2;
            while !h.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

impl InputSink for HelperSink {
    fn pass(&self, from: Option<[u8; 6]>, injects: &[Inject], counters: &Arc<HidCounters>) {
        let origin = from.map_or(Origin::None, Origin::Device);
        let pushed = {
            let mut q = self.shared.queue();
            if !q.counters.as_ref().is_some_and(|c| Arc::ptr_eq(c, counters)) {
                q.counters = Some(counters.clone());
            }
            q.queue.push_from(origin, injects)
        };
        if pushed.dropped > 0 {
            counters.dropped.fetch_add(pushed.dropped, Ordering::Relaxed);
        }
        if pushed.overflowed && !self.shared.overflow_logged.swap(true, Ordering::Relaxed) {
            tracing::warn!("input helper: fell behind; queued input dropped and every key released");
        }
        self.shared.wake.notify_one();
    }

    fn lock_keys(&self) -> Option<u8> {
        let v = self.shared.lock_keys.load(Ordering::Acquire);
        (v & 0x100 != 0).then_some((v & 0xFF) as u8)
    }

    fn release_all(&self) {
        self.shared.queue().queue.push_release_all();
        self.shared.wake.notify_one();
    }

    /// When a helper confirmed a batch with input of `device` and nothing
    /// refused in it (its ack); `windows_saw` from the helper's
    /// [`ToCore::Seen`] (None in record mode)
    fn delivered(&self, device: &[u8; 6]) -> Option<Delivery> {
        self.shared.log.get(device, &|_| None)
    }

    /// The test input through the queue, the pipe and the helper: fails
    /// when no helper connects within `wait` (HelperNotStarted), when the
    /// helper's ack says Windows refused it, or when no ack comes in time
    fn self_test(&self, wait: Duration) -> SelfTest {
        self.start();
        self.shared.self_test(wait)
    }
}

/// Why a helper ended
#[derive(Debug)]
enum End {
    Stopped,
    SessionChanged,
    Failed(&'static str),
}

impl From<io::Error> for End {
    fn from(_: io::Error) -> End {
        End::Failed("a pipe or process call failed")
    }
}

/// Kills the helper when its connection ends, however it ends
struct Running(Box<dyn HelperProcess>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.kill();
    }
}

fn sender(shared: &Arc<Shared>, config: &HelperConfig, launcher: &dyn Launcher) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        tracing::warn!("input helper: no runtime for the sender thread");
        return;
    };
    rt.block_on(async {
        let mut backoff = BACKOFF_FIRST;
        let mut no_session_logged = false;
        while !shared.stop.load(Ordering::Acquire) {
            let Some(session) = launcher.session() else {
                if !std::mem::replace(&mut no_session_logged, true) {
                    tracing::info!("input helper: nobody at the console, input is dropped");
                }
                wait(shared, BACKOFF_FIRST).await;
                continue;
            };
            no_session_logged = false;
            let began = Instant::now();
            let end = one_helper(shared, config, launcher, session).await;
            shared.connected.store(false, Ordering::Release);
            shared.pid.store(0, Ordering::Release);
            let dropped = shared.queue().queue.disconnect();
            if dropped > 0 {
                shared.count(|c| {
                    c.dropped.fetch_add(dropped, Ordering::Relaxed);
                });
            }
            match end {
                End::Stopped => break,
                End::SessionChanged => {
                    tracing::info!("input helper: the console session changed, starting one there");
                    backoff = BACKOFF_FIRST;
                    continue;
                }
                End::Failed(why) => {
                    tracing::warn!("input helper: {why} ({dropped} input(s) dropped); starting it again");
                }
            }
            if began.elapsed() > HEALTHY {
                backoff = BACKOFF_FIRST;
            }
            wait(shared, backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
        tracing::info!("input helper: stopped");
    });
}

/// Sleeps `d`, or less when told to stop
async fn wait(shared: &Shared, d: Duration) {
    let until = tokio::time::Instant::now() + d;
    while !shared.stop.load(Ordering::Acquire) && tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout_at(until, shared.wake.notified()).await;
    }
}

async fn one_helper(shared: &Arc<Shared>, config: &HelperConfig, launcher: &dyn Launcher, session: u32) -> End {
    match connect_and_serve(shared, config, launcher, session).await {
        Ok(end) | Err(end) => end,
    }
}

async fn connect_and_serve(
    shared: &Arc<Shared>,
    config: &HelperConfig,
    launcher: &dyn Launcher,
    session: u32,
) -> Result<End, End> {
    let name = format!("{PIPE_PREFIX}{}", win_sys::pipe_peer::random_hex(16)?);
    // This user and SYSTEM only, the first instance: nobody else serves it
    let server = win_sys::pipe::create_server(&name, true)?;
    let mut args = vec![HELPER_ARG.to_string(), name];
    if let Some(h) = config.record.filter(|_| crate::helper::RECORD_AVAILABLE) {
        args.extend([RECORD_ARG.to_string(), h.to_string()]);
    }
    let child = Running(launcher.launch(&config.exe, args, session)?);
    shared.started.fetch_add(1, Ordering::Relaxed);
    let pid = child.0.pid();
    shared.pid.store(pid, Ordering::Release);
    // Connected, gone, or too slow
    let exited = async {
        while !child.0.exited() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::select! {
        r = tokio::time::timeout(CONNECT_WAIT, server.connect()) => {
            r.map_err(|_| End::Failed("it did not connect"))??;
        }
        _ = exited => return Err(End::Failed("it exited before it connected")),
    }
    // The process at the other end is the one just started, as Windows
    // says (a client this user's other processes started is refused)
    if win_sys::pipe::client_process_id(server.as_handle())? != pid {
        return Err(End::Failed("another process connected to its pipe"));
    }
    let (mut rd, mut wr) = tokio::io::split(server);
    let hello = tokio::time::timeout(CONNECT_WAIT, protocol::read_frame(&mut rd))
        .await
        .map_err(|_| End::Failed("it did not say hello"))??
        .ok_or(End::Failed("it closed the pipe"))?;
    match protocol::decode_to_core(&hello) {
        Ok(ToCore::Hello { version, pid: p }) if version == protocol::VERSION && p == pid => {}
        _ => return Err(End::Failed("its hello was wrong")),
    }
    let released = shared.queue().queue.connect();
    shared.seqs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    shared.connected.store(true, Ordering::Release);
    // Every helper after the first is tested by itself (the core tests the
    // first one): a warning only, never a reason to stop
    if shared.connections.fetch_add(1, Ordering::AcqRel) > 0 {
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("input-helper-test".into()).spawn(move || {
            let result = s.self_test(NEW_HELPER_TEST_WAIT);
            tracing::info!(
                "input helper: the new helper's self-test {}",
                if result.failed() { "failed" } else { "passed" }
            );
            if let Some(f) = s.on_tested.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                f(result);
            }
        });
    }
    // The releases go out at once
    shared.wake.notify_one();
    tracing::info!(
        "input helper: running ({} started so far), releasing {released} key(s) or button(s) left by the one before",
        shared.started.load(Ordering::Relaxed)
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<Result<ToCore, protocol::ProtocolError>>>();
    let reader = tokio::spawn(async move {
        loop {
            match protocol::read_frame(&mut rd).await {
                Ok(Some(body)) => {
                    if tx.send(Some(protocol::decode_to_core(&body))).is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
    });
    let _reader = AbortOnDrop(reader);
    let mut tick = tokio::time::interval(TICK);
    let mut waiting_since: Option<Instant> = None;
    loop {
        tokio::select! {
            _ = shared.wake.notified() => {}
            m = rx.recv() => match m.flatten() {
                Some(Ok(msg)) => on_message(shared, msg, &mut waiting_since)?,
                Some(Err(_)) => return Err(End::Failed("it broke the protocol")),
                None => return Err(End::Failed("it went away")),
            },
            _ = tick.tick() => {
                if child.0.exited() {
                    return Err(End::Failed("it exited"));
                }
                if launcher.session() != Some(session) {
                    stop_helper(shared, &mut wr, &child).await;
                    return Ok(End::SessionChanged);
                }
                if waiting_since.is_some_and(|t| t.elapsed() > ACK_TIMEOUT) {
                    return Err(End::Failed("it stopped answering"));
                }
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            stop_helper(shared, &mut wr, &child).await;
            return Ok(End::Stopped);
        }
        loop {
            let (frame, unconfirmed) = {
                let mut q = shared.queue();
                (q.queue.take(), q.queue.unconfirmed())
            };
            let Some(frame) = frame else { break };
            if unconfirmed > UNCONFIRMED_MAX {
                return Err(End::Failed("it stopped answering"));
            }
            write(&mut wr, &frame).await?;
            waiting_since.get_or_insert_with(Instant::now);
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn write<W: tokio::io::AsyncWrite + Unpin>(wr: &mut W, m: &ToHelper) -> Result<(), End> {
    match tokio::time::timeout(WRITE_WAIT, wr.write_all(&protocol::encode_to_helper(m))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(End::Failed("it went away")),
        Err(_) => Err(End::Failed("it stopped reading")),
    }
}

/// Everything released, then the helper exits (killed if it does not)
async fn stop_helper<W: tokio::io::AsyncWrite + Unpin>(shared: &Shared, wr: &mut W, child: &Running) {
    // What is queued still goes (key-ups of a stop), then the release
    let frames: Vec<ToHelper> = {
        let mut q = shared.queue();
        q.queue.push_release_all();
        std::iter::from_fn(|| q.queue.take()).collect()
    };
    for f in frames.iter().chain([&ToHelper::Stop]) {
        if write(wr, f).await.is_err() {
            break;
        }
    }
    let until = Instant::now() + STOP_WAIT;
    while !child.0.exited() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn on_message(shared: &Shared, msg: ToCore, waiting_since: &mut Option<Instant>) -> Result<(), End> {
    match msg {
        ToCore::Hello { .. } => return Err(End::Failed("it said hello twice")),
        ToCore::Ack { processed, injected, blocked } => {
            let (origins, counters, left) = {
                let mut q = shared.queue();
                let origins = q.queue.ack_items(processed);
                (origins, q.counters.clone(), q.queue.unconfirmed())
            };
            let Some(origins) = origins else {
                return Err(End::Failed("it confirmed more than it was sent"));
            };
            shared.confirmed(&origins, processed, blocked);
            *waiting_since = (left > 0).then(Instant::now);
            if let Some(c) = counters {
                c.injected.fetch_add(u64::from(injected), Ordering::Relaxed);
                if blocked > 0 && c.blocked.fetch_add(u64::from(blocked), Ordering::Relaxed) == 0 {
                    tracing::warn!("input helper: Windows refused input; further refusals are only counted");
                }
            }
        }
        ToCore::Seen { processed } => shared.seen(processed),
        ToCore::LockKeys(mask) => {
            shared.lock_keys.store(0x100 | u16::from(mask), Ordering::Release);
            tracing::debug!("input helper: lock keys changed");
        }
        ToCore::Sas => {
            tracing::info!("input helper: Ctrl+Alt+Del pressed");
            if let Some(f) = shared.on_sas.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                f();
            }
        }
        ToCore::Desktop(state) => tracing::info!(
            "input helper: {}",
            match state {
                DesktopState::Default => "on the user's desktop",
                DesktopState::Secure => "on a secure desktop (sign-in, lock screen or UAC prompt)",
                DesktopState::Unavailable => "the input desktop is not available, Windows refuses input",
            }
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No helper can start: input is dropped and counted, the BTstack
    /// thread never waits
    #[test]
    fn without_a_helper_input_is_dropped_and_counted() {
        struct Nobody;
        impl Launcher for Nobody {
            fn session(&self) -> Option<u32> {
                None
            }
            fn launch(&self, _: &Path, _: Vec<String>, _: u32) -> io::Result<Box<dyn HelperProcess>> {
                Err(io::ErrorKind::Unsupported.into())
            }
        }
        let sink = HelperSink::new(HelperConfig { exe: PathBuf::new(), record: None }, Arc::new(Nobody));
        sink.start();
        let c = Arc::new(HidCounters::default());
        let t = Instant::now();
        for _ in 0..1000 {
            sink.pass(Some([1; 6]), &[Inject::Key { scan: 0x1E, extended: false, up: false }], &c);
        }
        assert!(t.elapsed() < Duration::from_secs(1));
        assert_eq!(c.dropped.load(Ordering::Relaxed), 1000);
        // Dropped input is nobody's delivery; no helper fails the self-test
        assert_eq!(sink.delivered(&[1; 6]), None);
        assert_eq!(sink.self_test(Duration::from_millis(200)), SelfTest::Failed(SelfTestFailure::HelperNotStarted));
        assert_eq!(sink.lock_keys(), None);
        assert!(!sink.connected());
        sink.stop();
        assert_eq!(sink.started(), 0);
    }

    /// A launcher whose "helper" is a process that never connects: the
    /// core gives up on it, kills it and tries again later
    #[test]
    fn a_helper_that_never_connects_is_killed() {
        struct Ping(Arc<AtomicU64>);
        struct PingChild(win_sys::session_launch::Child, Arc<AtomicU64>);
        impl HelperProcess for PingChild {
            fn pid(&self) -> u32 {
                self.0.pid()
            }
            fn exited(&self) -> bool {
                HelperProcess::exited(&self.0)
            }
            fn kill(&self) {
                self.1.fetch_add(1, Ordering::Relaxed);
                HelperProcess::kill(&self.0)
            }
        }
        impl Launcher for Ping {
            fn session(&self) -> Option<u32> {
                Some(1)
            }
            fn launch(&self, _: &Path, args: Vec<String>, _: u32) -> io::Result<Box<dyn HelperProcess>> {
                assert_eq!(args[0], HELPER_ARG);
                assert!(crate::helper::valid_pipe_name(&args[1]));
                let l = win_sys::session_launch::Launch {
                    exe: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                    args: vec!["/c".into(), "exit 9".into()],
                    session: 0,
                    identity: win_sys::session_launch::Identity::User,
                };
                Ok(Box::new(PingChild(win_sys::session_launch::launch(&l)?, self.0.clone())))
            }
        }
        let kills = Arc::new(AtomicU64::new(0));
        let sink = HelperSink::new(HelperConfig { exe: PathBuf::new(), record: None }, Arc::new(Ping(kills.clone())));
        sink.start();
        let until = Instant::now() + Duration::from_secs(20);
        while kills.load(Ordering::Relaxed) < 1 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        sink.stop();
        assert!(kills.load(Ordering::Relaxed) >= 1);
        assert!(sink.started() >= 1);
        assert!(!sink.connected());
    }
}
