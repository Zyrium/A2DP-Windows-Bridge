//! The two checks that keep a change from leaving the user without a
//! working keyboard or mouse, as Windows' display settings do ("Keep these
//! display settings?"):
//!
//! - [`Check`]: a device the user adds stays only when its input reaches
//!   Windows. The countdown starts once the device is ready for input
//!   (`HidEvent::Ready`). Without input from that very device before the
//!   countdown ends, or when the device went away and did not come back in
//!   time, the device is removed again as hid.remove removes it. The list of
//!   unconfirmed devices is kept on disk, so a core that restarts during the
//!   countdown removes them at its start.
//! - [`Health`]: the self-test of the path to Windows
//!   (`InputSink::self_test`). After hid.enable a failure turns the feature
//!   off again; at a later start it only warns.
//!
//! Only input that reached Windows confirms a device; nothing a client
//! sends can. Logs: counts and states only.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hid_input::InputKind;
use hid_input::sink::{SelfTest, SelfTestFailure};

/// How long a device the user adds has to send input once it is ready
pub const WINDOW: Duration = Duration::from_secs(30);
/// How long a device the user adds may take to become ready (pairing with
/// a number included) before it is removed again
pub const CONNECT_LIMIT: Duration = Duration::from_secs(120);

/// Why a device the user added was removed again
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Ready, but no input reached Windows in time
    NoInput,
    /// It went away during the countdown and did not come back
    Disconnected,
    /// It never became ready
    NotConnected,
    /// The user removed it during the countdown
    Cancelled,
}

impl Reason {
    /// The IPC form
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NoInput => "no_input",
            Reason::Disconnected => "disconnected",
            Reason::NotConnected => "not_connected",
            Reason::Cancelled => "cancelled",
        }
    }
}

/// What to do with a device under check now
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Wait,
    Confirmed,
    RollBack(Reason),
}

/// A device the user added, until its input reaches Windows
#[derive(Debug, Clone)]
pub struct Check {
    /// Upper case, as on the list
    pub address: String,
    pub name: String,
    /// LE: its address type
    pub le: Option<u8>,
    /// Added on the command line (not saved)
    pub cli: bool,
    /// Its address, and the identity address an LE device told since
    addrs: Vec<[u8; 6]>,
    /// When it was added: input since then counts
    started: Instant,
    window: Duration,
    deadline: Option<Instant>,
    connected: bool,
    kind: Option<InputKind>,
}

impl Check {
    pub fn new(addr: [u8; 6], address: String, name: String, le: Option<u8>, window: Duration, now: Instant) -> Check {
        Check {
            address,
            name,
            le,
            cli: false,
            addrs: vec![addr],
            started: now,
            window,
            deadline: None,
            connected: false,
            kind: None,
        }
    }

    pub fn is(&self, addr: &[u8; 6]) -> bool {
        self.addrs.contains(addr)
    }

    pub fn addrs(&self) -> &[[u8; 6]] {
        &self.addrs
    }

    /// Ready for input: the countdown starts (once; a device that comes
    /// back keeps the deadline it had)
    pub fn on_ready(&mut self, kind: InputKind, now: Instant) {
        self.connected = true;
        self.kind = Some(kind);
        self.deadline.get_or_insert(now + self.window);
    }

    pub fn on_disconnected(&mut self) {
        self.connected = false;
    }

    /// An LE device listed with a private address told its identity
    pub fn on_identity(&mut self, from: &[u8; 6], to: [u8; 6], address: String) {
        if self.is(from) && !self.is(&to) {
            self.addrs.push(to);
            self.address = address;
        }
    }

    /// `delivered`: when input of an address last reached Windows
    pub fn decide(&self, now: Instant, delivered: &dyn Fn(&[u8; 6]) -> Option<Instant>) -> Verdict {
        if self.addrs.iter().any(|a| delivered(a).is_some_and(|at| at >= self.started)) {
            return Verdict::Confirmed;
        }
        match self.deadline {
            Some(d) if now >= d => {
                Verdict::RollBack(if self.connected { Reason::NoInput } else { Reason::Disconnected })
            }
            None if now >= self.started + CONNECT_LIMIT => Verdict::RollBack(Reason::NotConnected),
            _ => Verdict::Wait,
        }
    }

    /// The countdown runs
    pub fn waiting(&self) -> bool {
        self.deadline.is_some()
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.map_or(self.window, |d| d.saturating_duration_since(now))
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    /// What its descriptor says: "keyboard", "mouse", "both", or "" while
    /// not known
    pub fn kind(&self) -> &'static str {
        match self.kind.map(|k| (k.keyboard, k.pointer)) {
            Some((true, true)) => "both",
            Some((true, false)) => "keyboard",
            Some((false, true)) => "mouse",
            _ => "",
        }
    }
}

/// The devices added and not confirmed yet: one address per line. A core
/// that finds any at its start removes them.
pub fn unconfirmed_file(config: &Path) -> PathBuf {
    crate::paths::devices_dir(config).join("hid_unconfirmed.txt")
}

/// At most this many lines are read (the list has 8)
const UNCONFIRMED_MAX: usize = 16;

pub fn load_unconfirmed(config: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(unconfirmed_file(config)) else { return Vec::new() };
    text.lines()
        .map(str::trim)
        .filter(|l| bt_host::parse_addr(l).is_some())
        .map(str::to_uppercase)
        .take(UNCONFIRMED_MAX)
        .collect()
}

fn save_unconfirmed(config: &Path, list: &[String]) -> std::io::Result<()> {
    let file = unconfirmed_file(config);
    if list.is_empty() {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, list.iter().map(|a| format!("{a}\n")).collect::<String>())
}

/// `address` (upper case) is unconfirmed (true) or not any more (false)
pub fn mark_unconfirmed(config: &Path, address: &str, on: bool) {
    let mut list = load_unconfirmed(config);
    let had = list.iter().any(|a| a == address);
    if on == had {
        return;
    }
    if on {
        list.push(address.to_string());
    } else {
        list.retain(|a| a != address);
    }
    if let Err(e) = save_unconfirmed(config, &list) {
        tracing::warn!("keyboards and mice: the list of unconfirmed devices was not saved ({})", e.kind());
    }
}

/// The self-test's outcome, as the UI shows it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Windows took the test input
    Ok,
    /// Nothing goes to Windows (dry run)
    NotTested,
    Failed(SelfTestFailure),
}

impl From<SelfTest> for Health {
    fn from(t: SelfTest) -> Health {
        match t {
            SelfTest::Passed { .. } => Health::Ok,
            SelfTest::NotTested => Health::NotTested,
            SelfTest::Failed(f) => Health::Failed(f),
        }
    }
}

impl Health {
    /// The IPC form: state and reason
    pub fn parts(self) -> (&'static str, Option<&'static str>) {
        match self {
            Health::Ok => ("ok", None),
            Health::NotTested => ("not_tested", None),
            Health::Failed(f) => ("failed", Some(failure_str(f))),
        }
    }
}

pub fn failure_str(f: SelfTestFailure) -> &'static str {
    match f {
        SelfTestFailure::HelperNotStarted => "helper",
        SelfTestFailure::Refused => "refused",
        SelfTestFailure::TimedOut => "timeout",
    }
}

/// When the self-test runs: right after hid.enable, or at a later start
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMoment {
    Enable,
    Later,
}

/// Whether a self-test result turns the feature off: only a failure
/// right after hid.enable. Later a failure only warns: turning the feature
/// off cannot help someone whose input already fails, and a false alarm
/// would cut a keyboard that works.
pub fn turns_off(moment: TestMoment, result: SelfTest) -> bool {
    moment == TestMoment::Enable && result.failed()
}

/// The message hid.enable fails with when the self-test turned the
/// feature off again
pub fn enable_failure_key(f: SelfTestFailure) -> &'static str {
    match f {
        SelfTestFailure::HelperNotStarted => "ui.dev.hid_test_off_start",
        SelfTestFailure::Refused => "ui.dev.hid_test_off_refused",
        SelfTestFailure::TimedOut => "ui.dev.hid_test_off_timeout",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 6] = [0, 0x11, 0x22, 0x33, 0x44, 0x55];
    const B: [u8; 6] = [0, 0x11, 0x22, 0x33, 0x44, 0x66];
    const W: Duration = Duration::from_secs(30);

    fn check(t0: Instant) -> Check {
        Check::new(A, "00:11:22:33:44:55".into(), "K".into(), None, W, t0)
    }

    fn never(_: &[u8; 6]) -> Option<Instant> {
        None
    }

    #[test]
    fn input_of_that_device_confirms() {
        let t0 = Instant::now();
        let mut c = check(t0);
        c.on_ready(InputKind { keyboard: true, pointer: false }, t0);
        assert_eq!(c.kind(), "keyboard");
        assert!(c.waiting());
        let t = t0 + Duration::from_secs(3);
        // Another device's input does not confirm
        assert_eq!(c.decide(t, &|a| (*a == B).then_some(t)), Verdict::Wait);
        // Input of A from before it was added does not either
        let before = t0.checked_sub(Duration::from_secs(1)).unwrap_or(t0);
        if before < t0 {
            assert_eq!(c.decide(t, &|a| (*a == A).then_some(before)), Verdict::Wait);
        }
        assert_eq!(c.decide(t, &|a| (*a == A).then_some(t)), Verdict::Confirmed);
    }

    #[test]
    fn no_input_in_time_rolls_back() {
        let t0 = Instant::now();
        let mut c = check(t0);
        let ready = t0 + Duration::from_secs(10);
        c.on_ready(InputKind::default(), ready);
        assert_eq!(c.kind(), "");
        assert_eq!(c.remaining(ready + Duration::from_secs(5)), Duration::from_secs(25));
        assert_eq!(c.decide(ready + W - Duration::from_millis(1), &never), Verdict::Wait);
        assert_eq!(c.decide(ready + W, &never), Verdict::RollBack(Reason::NoInput));
    }

    #[test]
    fn a_device_that_comes_back_in_time_keeps_its_deadline() {
        let t0 = Instant::now();
        let mut c = check(t0);
        c.on_ready(InputKind { keyboard: false, pointer: true }, t0);
        c.on_disconnected();
        assert_eq!(c.decide(t0 + Duration::from_secs(5), &never), Verdict::Wait);
        // Back: the countdown goes on, not anew
        c.on_ready(InputKind { keyboard: false, pointer: true }, t0 + Duration::from_secs(20));
        assert_eq!(c.kind(), "mouse");
        assert_eq!(c.remaining(t0 + Duration::from_secs(20)), Duration::from_secs(10));
        let t = t0 + Duration::from_secs(25);
        assert_eq!(c.decide(t, &|a| (*a == A).then_some(t)), Verdict::Confirmed);
        // Gone and not back at the deadline
        let mut d = check(t0);
        d.on_ready(InputKind::default(), t0);
        d.on_disconnected();
        assert_eq!(d.decide(t0 + W, &never), Verdict::RollBack(Reason::Disconnected));
    }

    #[test]
    fn a_device_that_never_gets_ready_rolls_back() {
        let t0 = Instant::now();
        let c = check(t0);
        assert!(!c.waiting());
        assert_eq!(c.remaining(t0), W);
        assert_eq!(c.decide(t0 + CONNECT_LIMIT - Duration::from_millis(1), &never), Verdict::Wait);
        assert_eq!(c.decide(t0 + CONNECT_LIMIT, &never), Verdict::RollBack(Reason::NotConnected));
    }

    #[test]
    fn the_identity_address_counts_too() {
        let t0 = Instant::now();
        let mut c = check(t0);
        c.on_identity(&A, B, "00:11:22:33:44:66".into());
        assert!(c.is(&A) && c.is(&B));
        assert_eq!(c.address, "00:11:22:33:44:66");
        let t = t0 + Duration::from_secs(1);
        assert_eq!(c.decide(t, &|a| (*a == B).then_some(t)), Verdict::Confirmed);
        // Another device's identity is not taken
        let mut d = check(t0);
        d.on_identity(&B, [9; 6], "x".into());
        assert!(!d.is(&[9; 6]));
    }

    /// A core that restarts during the countdown finds the device in the
    /// file (and removes it, Inner::hid_remove_unconfirmed)
    #[test]
    fn unconfirmed_devices_survive_a_restart_on_disk() {
        let dir = std::env::temp_dir().join(format!("bc-hid-unconfirmed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_unconfirmed(&dir).is_empty());
        mark_unconfirmed(&dir, "00:11:22:33:44:55", true);
        mark_unconfirmed(&dir, "00:11:22:33:44:55", true);
        mark_unconfirmed(&dir, "C0:11:22:33:44:66", true);
        assert_eq!(load_unconfirmed(&dir), ["00:11:22:33:44:55", "C0:11:22:33:44:66"]);
        mark_unconfirmed(&dir, "00:11:22:33:44:55", false);
        assert_eq!(load_unconfirmed(&dir), ["C0:11:22:33:44:66"]);
        // Garbage is dropped
        std::fs::write(unconfirmed_file(&dir), "x\nc0:11:22:33:44:77\n\n").unwrap();
        assert_eq!(load_unconfirmed(&dir), ["C0:11:22:33:44:77"]);
        mark_unconfirmed(&dir, "C0:11:22:33:44:77", false);
        assert!(!unconfirmed_file(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_failure_at_enable_turns_the_feature_off() {
        let fail = SelfTest::Failed(SelfTestFailure::Refused);
        let pass = SelfTest::Passed { windows_saw: Some(false) };
        assert!(turns_off(TestMoment::Enable, fail));
        assert!(!turns_off(TestMoment::Later, fail));
        assert!(!turns_off(TestMoment::Enable, pass));
        assert!(!turns_off(TestMoment::Enable, SelfTest::NotTested));
        assert_eq!(Health::from(fail).parts(), ("failed", Some("refused")));
        assert_eq!(Health::from(pass).parts(), ("ok", None));
        assert_eq!(Health::from(SelfTest::NotTested).parts(), ("not_tested", None));
        assert_eq!(enable_failure_key(SelfTestFailure::TimedOut), "ui.dev.hid_test_off_timeout");
    }
}
