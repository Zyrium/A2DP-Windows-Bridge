//! Where decoded input goes: the HID engine (bt-host) hands every batch of
//! Windows inputs to one [`InputSink`], chosen by the core when the stack
//! starts:
//!
//! - [`LocalSink`]: `SendInput` in this process, with the guard's copy of
//!   the held keys (win_sys::hid_guard). The resident app and the command
//!   line.
//! - [`DryRunSink`]: counted only (the emulator test).
//! - the input helper's sink (crate input-helper): queued for a helper
//!   process that passes the input to Windows on every desktop (a headless
//!   core, later the service).
//!
//! A sink never blocks the BTstack thread for longer than a short lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use win_sys::hid_guard::{HeldSection, HeldSet};
use win_sys::input_inject::Inject;

/// What the HID host did, for diagnostics (counts only)
#[derive(Debug, Default)]
pub struct HidCounters {
    /// Input reports received
    pub reports: AtomicU64,
    /// Reports dropped: no usable report descriptor yet
    pub undecoded: AtomicU64,
    /// Key, button, move and wheel events decoded from the reports
    pub events: AtomicU64,
    /// Events Windows took
    pub injected: AtomicU64,
    /// Events Windows refused (a window of higher privilege has the focus,
    /// or the secure desktop is shown)
    pub blocked: AtomicU64,
    /// Events that never reached Windows: no input helper was running, or
    /// pointer moves dropped while the helper fell behind
    pub dropped: AtomicU64,
    /// Connections refused: the device is not on the list
    pub refused: AtomicU64,
    /// Keyboard light reports sent to keyboards
    pub leds: AtomicU64,
    /// Passing input to Windows is on
    pub active: AtomicBool,
}

/// When input of one device last reached Windows ([`InputSink::delivered`])
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub at: Instant,
    /// Windows registered input in the injecting process's session since
    /// then (GetLastInputInfo): a hint, not a proof. None: not checked (no
    /// input goes to Windows, or the check is not available).
    pub windows_saw: Option<bool>,
}

/// Why [`InputSink::self_test`] failed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTestFailure {
    /// No input helper took input within the wait
    HelperNotStarted,
    /// Windows did not take the test input
    Refused,
    /// The test input was not answered within the wait
    TimedOut,
}

/// What [`InputSink::self_test`] found
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfTest {
    /// Windows took the test input; `windows_saw` as in [`Delivery`]
    Passed {
        windows_saw: Option<bool>,
    },
    /// Nothing goes to Windows (dry run): nothing to test, passes
    NotTested,
    Failed(SelfTestFailure),
}

impl SelfTest {
    pub fn failed(&self) -> bool {
        matches!(self, SelfTest::Failed(_))
    }
}

/// The input every self-test injects: a relative pointer move by zero in
/// both directions. The pointer stays where it is, no key or button
/// changes and no window gets a key; Windows still handles it as mouse
/// input (the window under the pointer may get a mouse-move message).
pub const SELF_TEST_INPUT: Inject = Inject::Move { dx: 0, dy: 0 };

/// Devices remembered at most by a [`DeliveryLog`] (the list has 8)
const LOG_MAX: usize = 64;

#[derive(Debug, Clone, Copy)]
struct LogEntry {
    at: Instant,
    /// The tick count just before the injection, to compare with
    /// GetLastInputInfo later; None: not checked
    tick: Option<u32>,
    saw: Option<bool>,
}

/// When input of each device reached Windows: what the sinks record and
/// [`InputSink::delivered`] reads. A short lock, never a wait.
#[derive(Debug, Default)]
pub struct DeliveryLog {
    map: Mutex<HashMap<[u8; 6], LogEntry>>,
}

impl DeliveryLog {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 6], LogEntry>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Input of `device` reached Windows now. `tick`: the tick count before
    /// it was injected, checked against Windows' last input later; `saw`:
    /// what is known of Windows' registration already (None: not checked)
    pub fn record(&self, device: [u8; 6], tick: Option<u32>, saw: Option<bool>) {
        let mut m = self.lock();
        if m.len() >= LOG_MAX && !m.contains_key(&device) {
            m.clear();
        }
        m.insert(device, LogEntry { at: Instant::now(), tick, saw });
    }

    /// Windows registered input after the last delivery of `device`
    pub fn saw(&self, device: &[u8; 6]) {
        if let Some(e) = self.lock().get_mut(device) {
            e.saw = Some(true);
        }
    }

    /// The last delivery of `device`; `check` tells from a tick count
    /// whether Windows registered input since then (asked until it does)
    pub fn get(&self, device: &[u8; 6], check: &dyn Fn(u32) -> Option<bool>) -> Option<Delivery> {
        let mut m = self.lock();
        let e = m.get_mut(device)?;
        if e.saw != Some(true)
            && let Some(tick) = e.tick
            && let Some(now) = check(tick)
        {
            e.saw = Some(now);
        }
        Some(Delivery { at: e.at, windows_saw: e.saw })
    }

    pub fn forget(&self, device: &[u8; 6]) {
        self.lock().remove(device);
    }
}

/// Takes the Windows inputs of the HID engine
pub trait InputSink: std::fmt::Debug + Send + Sync {
    /// `injects` in order, as one batch, decoded from a report of `from`
    /// (None: not the user's input, such as the key-ups of a release).
    /// Counts what Windows took (`injected`), refused (`blocked`) or what
    /// never got there (`dropped`) in `counters`, now or later. Must not
    /// block.
    fn pass(&self, from: Option<[u8; 6]>, injects: &[Inject], counters: &Arc<HidCounters>);

    /// Windows' Num, Caps and Scroll Lock state (a `crate::leds` mask);
    /// None: not known (nothing is sent to the keyboards then)
    fn lock_keys(&self) -> Option<u8>;

    /// The stack stopped: let go of anything still held that the key-ups
    /// already passed did not cover (the input helper's sink; the others
    /// pass every key-up themselves)
    fn release_all(&self) {}

    /// When input of `device` last reached Windows, as this sink defines
    /// it: [`LocalSink`] when `SendInput` inserted every input of a batch,
    /// [`DryRunSink`] when a batch was counted, the input helper's sink
    /// when the helper confirmed a batch with nothing refused. Only input
    /// passed with `from` set counts. None: never.
    fn delivered(&self, device: &[u8; 6]) -> Option<Delivery>;

    /// Passes [`SELF_TEST_INPUT`] to Windows the way device input goes,
    /// and says whether Windows took it. Blocks up to about `wait`; never
    /// call it on the BTstack thread.
    fn self_test(&self, wait: Duration) -> SelfTest;
}

/// Input counted only, never passed to Windows
#[derive(Debug, Default)]
pub struct DryRunSink {
    log: DeliveryLog,
}

impl InputSink for DryRunSink {
    fn pass(&self, from: Option<[u8; 6]>, injects: &[Inject], _counters: &Arc<HidCounters>) {
        if let Some(device) = from.filter(|_| !injects.is_empty()) {
            self.log.record(device, None, None);
        }
    }

    fn lock_keys(&self) -> Option<u8> {
        None
    }

    fn delivered(&self, device: &[u8; 6]) -> Option<Delivery> {
        self.log.get(device, &|_| None)
    }

    fn self_test(&self, _wait: Duration) -> SelfTest {
        SelfTest::NotTested
    }
}

/// What [`LocalSink`] calls in Windows: a seam, so that tests never call
/// `SendInput`
pub trait LocalInjector: Send + Sync {
    /// Ok(all inserted) or Err((inserted, why))
    fn send(&self, batch: &[Inject]) -> Result<usize, (usize, String)>;
    fn lock_keys(&self) -> u8;
    /// The tick count now; None: Windows' last input is not checked
    fn tick(&self) -> Option<u32>;
    /// Whether Windows registered input at or after `tick`
    fn input_since(&self, tick: u32) -> Option<bool>;
}

/// The real one: `SendInput`, `GetKeyState`, `GetLastInputInfo`
pub struct WindowsInjector;

impl LocalInjector for WindowsInjector {
    fn send(&self, batch: &[Inject]) -> Result<usize, (usize, String)> {
        win_sys::input_inject::send(batch).map_err(|e| {
            let why = e.to_string();
            let win_sys::input_inject::InjectError::Partial { inserted, .. } = e;
            (inserted, why)
        })
    }

    fn lock_keys(&self) -> u8 {
        win_sys::input_inject::lock_keys()
    }

    fn tick(&self) -> Option<u32> {
        Some(win_sys::input_inject::tick_now())
    }

    fn input_since(&self, tick: u32) -> Option<bool> {
        win_sys::input_inject::input_since(tick)
    }
}

/// How long the self-test waits before it asks Windows for its last input
const SELF_TEST_SETTLE: Duration = Duration::from_millis(100);

/// `SendInput` in this process; the keys held are written to the guard's
/// section ([`HeldSection`]) when one is given
pub struct LocalSink {
    held: Option<Arc<HeldSection>>,
    held_set: Mutex<HeldSet>,
    injector: Box<dyn LocalInjector>,
    log: DeliveryLog,
}

impl std::fmt::Debug for LocalSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSink").field("guarded", &self.held.is_some()).finish_non_exhaustive()
    }
}

impl LocalSink {
    pub fn new(held: Option<Arc<HeldSection>>) -> LocalSink {
        LocalSink::with_injector(held, Box::new(WindowsInjector))
    }

    pub fn with_injector(held: Option<Arc<HeldSection>>, injector: Box<dyn LocalInjector>) -> LocalSink {
        LocalSink { held, held_set: Mutex::default(), injector, log: DeliveryLog::default() }
    }

    /// Follows what Windows took; the guard's copy is written when it changed
    fn track_held(&self, injected: &[Inject]) {
        let Some(section) = &self.held else { return };
        let mut set = self.held_set.lock().unwrap_or_else(|e| e.into_inner());
        if set.apply(injected) {
            section.store(&set);
        }
    }
}

impl InputSink for LocalSink {
    fn pass(&self, from: Option<[u8; 6]>, injects: &[Inject], c: &Arc<HidCounters>) {
        if injects.is_empty() {
            return;
        }
        let tick = from.and_then(|_| self.injector.tick());
        let taken = match self.injector.send(injects) {
            Ok(n) => n,
            Err((inserted, why)) => {
                // Not per event: a held key repeats this many times a second
                if c.blocked.load(Ordering::Relaxed) == 0 {
                    tracing::warn!("hid: Windows refused input ({why}); further refusals are only counted");
                }
                inserted
            }
        };
        let taken = taken.min(injects.len());
        // The first `taken` went in, in order
        self.track_held(&injects[..taken]);
        c.injected.fetch_add(taken as u64, Ordering::Relaxed);
        c.blocked.fetch_add((injects.len() - taken) as u64, Ordering::Relaxed);
        if let Some(device) = from.filter(|_| taken == injects.len()) {
            self.log.record(device, tick, tick.map(|_| false));
        }
    }

    fn lock_keys(&self) -> Option<u8> {
        Some(self.injector.lock_keys())
    }

    fn delivered(&self, device: &[u8; 6]) -> Option<Delivery> {
        self.log.get(device, &|tick| self.injector.input_since(tick))
    }

    fn self_test(&self, _wait: Duration) -> SelfTest {
        let tick = self.injector.tick();
        match self.injector.send(&[SELF_TEST_INPUT]) {
            Ok(1) => {
                let windows_saw = tick.and_then(|t| {
                    std::thread::sleep(SELF_TEST_SETTLE);
                    self.injector.input_since(t)
                });
                SelfTest::Passed { windows_saw }
            }
            _ => SelfTest::Failed(SelfTestFailure::Refused),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 6] = [0, 0x11, 0x22, 0x33, 0x44, 0x55];
    const B: [u8; 6] = [0, 0x11, 0x22, 0x33, 0x44, 0x66];

    fn key() -> Inject {
        Inject::Key { scan: 0x1E, extended: false, up: false }
    }

    #[test]
    fn dry_run_counts_nothing_and_knows_no_lights() {
        let c = Arc::new(HidCounters::default());
        let sink = DryRunSink::default();
        sink.pass(Some(A), &[key()], &c);
        assert_eq!(c.injected.load(Ordering::Relaxed) + c.blocked.load(Ordering::Relaxed), 0);
        assert_eq!(sink.lock_keys(), None);
        assert_eq!(sink.self_test(Duration::ZERO), SelfTest::NotTested);
    }

    /// Dry run: a counted batch from a device is its delivery; releases
    /// (no device) and empty batches are not
    #[test]
    fn dry_run_delivery_is_per_device() {
        let c = Arc::new(HidCounters::default());
        let sink = DryRunSink::default();
        sink.pass(None, &[key()], &c);
        sink.pass(Some(B), &[], &c);
        assert_eq!(sink.delivered(&A), None);
        assert_eq!(sink.delivered(&B), None);
        sink.pass(Some(A), &[key()], &c);
        assert_eq!(sink.delivered(&A).unwrap().windows_saw, None);
        assert_eq!(sink.delivered(&B), None);
    }

    /// A fake Windows: takes `take` inputs of each batch (None: all) and
    /// records every call; never calls SendInput
    #[derive(Default)]
    struct Fake {
        take: Mutex<Option<usize>>,
        sent: Mutex<Vec<Inject>>,
        last_input: Mutex<u32>,
        now: Mutex<u32>,
    }

    struct FakeRef(Arc<Fake>);

    impl LocalInjector for FakeRef {
        fn send(&self, batch: &[Inject]) -> Result<usize, (usize, String)> {
            self.0.sent.lock().unwrap().extend_from_slice(batch);
            match *self.0.take.lock().unwrap() {
                Some(n) if n < batch.len() => Err((n, "refused".into())),
                _ => Ok(batch.len()),
            }
        }
        fn lock_keys(&self) -> u8 {
            5
        }
        fn tick(&self) -> Option<u32> {
            Some(*self.0.now.lock().unwrap())
        }
        fn input_since(&self, tick: u32) -> Option<bool> {
            Some(win_sys::input_inject::tick_not_older(*self.0.last_input.lock().unwrap(), tick))
        }
    }

    #[test]
    fn local_delivery_needs_every_input_taken_and_follows_windows() {
        let fake = Arc::new(Fake::default());
        *fake.now.lock().unwrap() = 1000;
        let sink = LocalSink::with_injector(None, Box::new(FakeRef(fake.clone())));
        let c = Arc::new(HidCounters::default());
        // An empty batch never reaches Windows
        sink.pass(Some(A), &[], &c);
        assert!(fake.sent.lock().unwrap().is_empty());
        // Refused in part: counted, not delivered
        *fake.take.lock().unwrap() = Some(1);
        sink.pass(Some(A), &[key(), key()], &c);
        assert_eq!((c.injected.load(Ordering::Relaxed), c.blocked.load(Ordering::Relaxed)), (1, 1));
        assert_eq!(sink.delivered(&A), None);
        // Taken: delivered; Windows' last input is older at first, then not
        *fake.take.lock().unwrap() = None;
        *fake.last_input.lock().unwrap() = 900;
        sink.pass(Some(A), &[key()], &c);
        assert_eq!(sink.delivered(&A).unwrap().windows_saw, Some(false));
        *fake.last_input.lock().unwrap() = 1010;
        assert_eq!(sink.delivered(&A).unwrap().windows_saw, Some(true));
        // A release is nobody's delivery
        sink.pass(None, &[key()], &c);
        assert_eq!(sink.delivered(&B), None);
        assert_eq!(sink.lock_keys(), Some(5));
    }

    #[test]
    fn local_self_test_injects_a_zero_move() {
        let fake = Arc::new(Fake::default());
        *fake.now.lock().unwrap() = 50;
        *fake.last_input.lock().unwrap() = 50;
        let sink = LocalSink::with_injector(None, Box::new(FakeRef(fake.clone())));
        assert_eq!(sink.self_test(Duration::ZERO), SelfTest::Passed { windows_saw: Some(true) });
        assert_eq!(*fake.sent.lock().unwrap(), [SELF_TEST_INPUT]);
        *fake.take.lock().unwrap() = Some(0);
        assert_eq!(sink.self_test(Duration::ZERO), SelfTest::Failed(SelfTestFailure::Refused));
        // The test input is nobody's delivery
        assert_eq!(sink.delivered(&A), None);
    }

    #[test]
    fn the_log_is_bounded() {
        let log = DeliveryLog::default();
        let dev = |n: u8| [n, 0, 0, 0, 0, 0];
        for n in 0..=LOG_MAX as u8 {
            log.record(dev(n), None, None);
        }
        assert!(log.get(&dev(LOG_MAX as u8), &|_| None).is_some());
        assert!(log.lock().len() <= LOG_MAX);
        log.saw(&dev(LOG_MAX as u8));
        assert_eq!(log.get(&dev(LOG_MAX as u8), &|_| None).unwrap().windows_saw, Some(true));
        log.forget(&dev(LOG_MAX as u8));
        assert!(log.get(&dev(LOG_MAX as u8), &|_| None).is_none());
    }
}
