//! The helper's injecting thread, without its pipe: what it passes to
//! Windows, the keys and buttons it holds, when it lets go of them, the
//! lock keys it reports and Ctrl+Alt+Del. Everything Windows does is
//! behind [`Injector`] and [`DesktopApi`], so the rules are tested with
//! fakes; the real helper uses [`SendInputInjector`] and
//! [`crate::desktop::Win32`], the record mode [`RecordInjector`].

use std::io::Write;

use win_sys::hid_guard::HeldSet;
use win_sys::input_inject::Inject;

use crate::desktop::{Check, DesktopApi, DesktopFollow};
use crate::protocol::{DesktopState, ToCore};

/// Windows took only the first `taken`; `access_denied`: because the
/// thread is not on the input desktop (any more)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub taken: usize,
    pub access_denied: bool,
}

/// Passes input to Windows
pub trait Injector {
    fn inject(&mut self, batch: &[Inject]) -> Result<usize, Refused>;
    /// Windows' lock keys (hid_input::leds mask), read on the injecting
    /// thread
    fn lock_keys(&mut self) -> u8;
    /// A line for the record (the record mode only)
    fn note(&mut self, _line: &str) {}
    /// The tick count now, for [`Injector::input_since`]; None: Windows'
    /// last input is not checked (record mode, fakes)
    fn tick(&mut self) -> Option<u32> {
        None
    }
    /// Whether Windows registered input in this session at or after `tick`
    fn input_since(&mut self, _tick: u32) -> Option<bool> {
        None
    }
}

/// `SendInput` (win_sys::input_inject) on the injecting thread
pub struct SendInputInjector;

impl Injector for SendInputInjector {
    fn inject(&mut self, batch: &[Inject]) -> Result<usize, Refused> {
        win_sys::input_inject::send(batch).map_err(|e| {
            let access_denied = win_sys::input_desktop::is_access_denied(&e);
            let win_sys::input_inject::InjectError::Partial { inserted, .. } = e;
            Refused { taken: inserted, access_denied }
        })
    }

    fn lock_keys(&mut self) -> u8 {
        win_sys::input_inject::lock_keys()
    }

    fn tick(&mut self) -> Option<u32> {
        Some(win_sys::input_inject::tick_now())
    }

    fn input_since(&mut self, tick: u32) -> Option<bool> {
        win_sys::input_inject::input_since(tick)
    }
}

/// The test switch: writes what it would pass to Windows, one line per
/// input, to a pipe the test reads; never calls `SendInput`. Lock keys:
/// read from Windows (reading changes nothing).
pub struct RecordInjector<W: Write> {
    out: W,
}

impl<W: Write> RecordInjector<W> {
    pub fn new(out: W) -> Self {
        RecordInjector { out }
    }
}

/// One record line: kind, code and direction; moves with their values
pub fn record_line(i: &Inject) -> String {
    let dir = |up: bool| if up { "up" } else { "down" };
    match *i {
        Inject::Key { scan, extended, up } => {
            format!("key {scan:#04x}{} {}", if extended { " ext" } else { "" }, dir(up))
        }
        Inject::VirtualKey { vk, up } => format!("vk {vk:#04x} {}", dir(up)),
        Inject::Button { button, up } => format!("button {button:?} {}", dir(up)),
        Inject::Move { dx, dy } => format!("move {dx} {dy}"),
        Inject::Wheel(d) => format!("wheel {d}"),
        Inject::HWheel(d) => format!("hwheel {d}"),
    }
}

impl<W: Write> Injector for RecordInjector<W> {
    fn inject(&mut self, batch: &[Inject]) -> Result<usize, Refused> {
        let text: String = batch.iter().map(|i| record_line(i) + "\n").collect();
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
        Ok(batch.len())
    }

    fn lock_keys(&mut self) -> u8 {
        win_sys::input_inject::lock_keys()
    }

    fn note(&mut self, line: &str) {
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }
}

// Scan codes of Ctrl (0x1D, right one extended), Alt (0x38) and Delete
// (0x53: the Delete key extended, the keypad's without)
const CTRL: u16 = 0x1D;
const ALT: u16 = 0x38;
const DELETE: u16 = 0x53;

/// Tells Ctrl+Alt+Del from the input as it goes to Windows
#[derive(Debug, Default)]
struct SasDetector {
    /// Bits: left Ctrl, right Ctrl, left Alt, right Alt
    mods: u8,
}

impl SasDetector {
    /// True when `i` completes Ctrl+Alt+Del
    fn feed(&mut self, i: &Inject) -> bool {
        let Inject::Key { scan, extended, up } = *i else { return false };
        let bit = match (scan, extended) {
            (CTRL, false) => 1,
            (CTRL, true) => 2,
            (ALT, false) => 4,
            (ALT, true) => 8,
            (DELETE, _) => return !up && self.mods & 3 != 0 && self.mods & 12 != 0,
            _ => return false,
        };
        if up {
            self.mods &= !bit;
        } else {
            self.mods |= bit;
        }
        false
    }

    fn clear(&mut self) {
        self.mods = 0;
    }
}

/// The helper's state on its injecting thread
pub struct HelperEngine<I: Injector, D: DesktopApi> {
    injector: I,
    desktop: DesktopFollow<D>,
    /// What this helper holds down in Windows
    held: HeldSet,
    sas: SasDetector,
    /// Items done since the connection started (records and releases)
    processed: u64,
    /// Since the last ack
    injected: u32,
    blocked: u32,
    /// Lock keys last reported (None: none yet)
    lock_keys: Option<u8>,
    desktop_state: Option<DesktopState>,
    /// News for the core not sent yet
    pending: Vec<ToCore>,
    /// The tick count before the last batch Windows took, and the items
    /// done with it: reported as [`ToCore::Seen`] once Windows' last input
    /// is not older
    unseen: Option<(u32, u64)>,
}

impl<I: Injector, D: DesktopApi> HelperEngine<I, D> {
    pub fn new(injector: I, desktop: D) -> Self {
        HelperEngine {
            injector,
            desktop: DesktopFollow::new(desktop),
            held: HeldSet::default(),
            sas: SasDetector::default(),
            processed: 0,
            injected: 0,
            blocked: 0,
            lock_keys: None,
            desktop_state: None,
            pending: Vec::new(),
            unseen: None,
        }
    }

    pub fn injector(&mut self) -> &mut I {
        &mut self.injector
    }

    /// Keys and buttons held now
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Injects `batch` as far as Windows takes it; what was taken is
    /// tracked as held or released
    fn send(&mut self, batch: &[Inject]) -> usize {
        if batch.is_empty() {
            return 0;
        }
        let taken = match self.injector.inject(batch) {
            Ok(n) => n,
            Err(r) if r.access_denied => {
                // The desktop changed since the last check: attach to the
                // new one (releasing there first) and try the rest once
                let done = &batch[..r.taken.min(batch.len())];
                self.held.apply(done);
                let check = self.desktop.on_access_denied();
                self.follow(check);
                let rest = &batch[done.len()..];
                let again = match self.injector.inject(rest) {
                    Ok(n) => n,
                    Err(r) => r.taken,
                };
                self.held.apply(&rest[..again.min(rest.len())]);
                self.injected += (done.len() + again) as u32;
                self.blocked += (rest.len() - again.min(rest.len())) as u32;
                return done.len() + again;
            }
            Err(r) => r.taken,
        };
        let taken = taken.min(batch.len());
        self.held.apply(&batch[..taken]);
        self.injected += taken as u32;
        self.blocked += (batch.len() - taken) as u32;
        taken
    }

    /// Lets go of everything held (on the desktop attached now)
    pub fn release_all(&mut self) -> usize {
        let releases = self.held.releases();
        self.sas.clear();
        if releases.is_empty() {
            return 0;
        }
        let taken = self.send(&releases);
        // Whatever Windows refused cannot be released by us any more; it is
        // forgotten so that the next desktop does not get stray key-ups
        self.held = HeldSet::default();
        taken
    }

    fn follow(&mut self, check: Check) {
        let state = match check {
            Check::Same => return,
            Check::Unavailable => DesktopState::Unavailable,
            Check::Attached | Check::Switched => {
                if self.desktop.current().is_some_and(|n| n.eq_ignore_ascii_case("Default")) {
                    DesktopState::Default
                } else {
                    DesktopState::Secure
                }
            }
        };
        // A key held across the switch would stay down: released on the
        // new desktop before anything else goes there
        if matches!(check, Check::Switched | Check::Attached) && !self.held.is_empty() {
            self.release_all();
        }
        if self.desktop_state != Some(state) {
            self.desktop_state = Some(state);
            self.pending.push(ToCore::Desktop(state));
        }
    }

    /// A batch from the core. The ack (and any other news) to send back.
    pub fn batch(&mut self, batch: &[Inject]) -> Vec<ToCore> {
        let check = self.desktop.check();
        self.follow(check);
        // Ctrl+Alt+Del cannot be passed on with SendInput: the keys go on
        // as they are (Windows ignores them), and the core is asked
        for i in batch {
            if self.sas.feed(i) {
                self.pending.push(ToCore::Sas);
            }
        }
        let tick = self.injector.tick();
        let taken = self.send(batch);
        self.processed += batch.len() as u64;
        if let Some(t) = tick.filter(|_| taken > 0) {
            self.unseen = Some((t, self.processed));
        }
        let mut out = std::mem::take(&mut self.pending);
        out.push(self.ack());
        out
    }

    /// [`crate::protocol::ToHelper::ReleaseAll`] from the core
    pub fn release_request(&mut self) -> Vec<ToCore> {
        self.release_all();
        self.processed += 1;
        let mut out = std::mem::take(&mut self.pending);
        out.push(self.ack());
        out
    }

    fn ack(&mut self) -> ToCore {
        let ack = ToCore::Ack { processed: self.processed, injected: self.injected, blocked: self.blocked };
        self.injected = 0;
        self.blocked = 0;
        ack
    }

    /// The timer: follows the input desktop, reports lock keys that changed
    pub fn tick(&mut self) -> Vec<ToCore> {
        let check = self.desktop.check();
        self.follow(check);
        let mut out = std::mem::take(&mut self.pending);
        if check != Check::Unavailable {
            let now = self.injector.lock_keys() & hid_input::leds::ALL;
            if self.lock_keys != Some(now) {
                self.lock_keys = Some(now);
                out.push(ToCore::LockKeys(now));
            }
        }
        if self.injected + self.blocked > 0 {
            out.push(self.ack());
        }
        if let Some((t, processed)) = self.unseen
            && self.injector.input_since(t) == Some(true)
        {
            self.unseen = None;
            out.push(ToCore::Seen { processed });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::fake::FakeDesktops;
    use win_sys::input_inject::MouseButton;

    /// Records what it was given; refuses with access denied while told to
    #[derive(Default)]
    struct FakeInjector {
        got: Vec<Inject>,
        deny_next: usize,
        refuse_all: bool,
        locks: u8,
        /// Some: the tick count now, and Windows' last input
        clock: Option<(u32, u32)>,
    }

    impl Injector for FakeInjector {
        fn inject(&mut self, batch: &[Inject]) -> Result<usize, Refused> {
            if self.refuse_all {
                return Err(Refused { taken: 0, access_denied: false });
            }
            if self.deny_next > 0 {
                self.deny_next -= 1;
                // Took the first one only
                self.got.push(batch[0]);
                return Err(Refused { taken: 1, access_denied: true });
            }
            self.got.extend_from_slice(batch);
            Ok(batch.len())
        }

        fn lock_keys(&mut self) -> u8 {
            self.locks
        }

        fn tick(&mut self) -> Option<u32> {
            self.clock.map(|(now, _)| now)
        }

        fn input_since(&mut self, tick: u32) -> Option<bool> {
            self.clock.map(|(_, last)| win_sys::input_inject::tick_not_older(last, tick))
        }
    }

    /// Windows' last input not older than the batch: one Seen with the
    /// items done; never without a clock (record mode) or before it is
    #[test]
    fn windows_registering_the_input_is_reported_once() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        e.batch(&[Inject::Move { dx: 0, dy: 0 }]);
        assert!(!e.tick().iter().any(|m| matches!(m, ToCore::Seen { .. })));
        e.injector().clock = Some((500, 400));
        e.batch(&[key(0x1E, false, false), key(0x1E, false, true)]);
        assert!(!e.tick().iter().any(|m| matches!(m, ToCore::Seen { .. })));
        e.injector().clock = Some((600, 510));
        assert_eq!(e.tick(), [ToCore::Seen { processed: 3 }]);
        assert!(e.tick().is_empty());
        // Refused: nothing to see
        e.injector().refuse_all = true;
        e.batch(&[key(0x1E, false, false)]);
        let out = e.tick();
        assert!(!out.iter().any(|m| matches!(m, ToCore::Seen { .. })));
    }

    fn key(scan: u16, extended: bool, up: bool) -> Inject {
        Inject::Key { scan, extended, up }
    }

    fn engine(desks: &FakeDesktops) -> HelperEngine<FakeInjector, FakeDesktops> {
        HelperEngine::new(FakeInjector::default(), desks.clone())
    }

    #[test]
    fn batches_are_passed_in_order_and_acked() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        let out = e.batch(&[key(0x1E, false, false), Inject::Move { dx: 1, dy: 2 }, key(0x1E, false, true)]);
        assert_eq!(
            out,
            [ToCore::Desktop(DesktopState::Default), ToCore::Ack { processed: 3, injected: 3, blocked: 0 }]
        );
        assert_eq!(e.injector().got.len(), 3);
        assert_eq!(e.held(), 0);
        assert_eq!(e.release_request(), [ToCore::Ack { processed: 4, injected: 0, blocked: 0 }]);
    }

    #[test]
    fn a_desktop_switch_releases_what_is_held_on_the_new_desktop_first() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        e.batch(&[key(0x2A, false, false), Inject::Button { button: MouseButton::Left, up: false }]);
        assert_eq!(e.held(), 2);
        // A UAC prompt: the tick finds it, releases there
        d.set(Some("Winlogon"));
        let out = e.tick();
        assert!(out.contains(&ToCore::Desktop(DesktopState::Secure)));
        assert_eq!(e.held(), 0);
        let got = &e.injector().got;
        assert_eq!(got[2..], [key(0x2A, false, true), Inject::Button { button: MouseButton::Left, up: true }]);
        assert_eq!(*d.attached.borrow(), ["Default", "Winlogon"]);
    }

    #[test]
    fn access_denied_retries_the_rest_once() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        e.batch(&[key(0x1D, false, false)]);
        // Refused after the first: still the same desktop, the rest goes
        // in on the second try
        e.injector().deny_next = 1;
        let out = e.batch(&[key(0x1E, false, false), key(0x1E, false, true), key(0x30, false, false)]);
        assert_eq!(out.last(), Some(&ToCore::Ack { processed: 4, injected: 3, blocked: 0 }));
        assert_eq!(e.held(), 2);
        // Refused outright: counted as blocked, nothing more held
        e.injector().refuse_all = true;
        let out = e.batch(&[key(0x31, false, false)]);
        assert_eq!(out.last(), Some(&ToCore::Ack { processed: 5, injected: 0, blocked: 1 }));
        assert_eq!(e.held(), 2);
    }

    /// The check before a batch still saw the user's desktop; the lock
    /// screen came before SendInput: the refusal finds the switch, what is
    /// held is released there, then the rest goes
    #[test]
    fn a_switch_found_by_a_refusal_releases_first() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        e.batch(&[key(0x2A, false, false)]);
        let before = e.injector().got.len();
        assert_eq!(e.desktop.check(), Check::Same);
        d.set(Some("Winlogon"));
        e.injector().deny_next = 1;
        e.send(&[key(0x1E, false, false), key(0x1E, false, true)]);
        let got = e.injector().got[before..].to_vec();
        assert_eq!(
            got,
            [key(0x1E, false, false), key(0x1E, false, true), key(0x2A, false, true), key(0x1E, false, true)]
        );
        assert_eq!(e.held(), 0);
        assert_eq!(e.desktop.current(), Some("Winlogon"));
    }

    #[test]
    fn lock_keys_are_reported_once_per_change() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        assert!(e.tick().contains(&ToCore::LockKeys(0)));
        assert!(!e.tick().iter().any(|m| matches!(m, ToCore::LockKeys(_))));
        e.injector().locks = 0xFA;
        assert!(e.tick().contains(&ToCore::LockKeys(0x02)));
        assert!(!e.tick().iter().any(|m| matches!(m, ToCore::LockKeys(_))));
        // Not read while no desktop can be followed
        d.set(None);
        e.injector().locks = 1;
        assert_eq!(e.tick(), [ToCore::Desktop(DesktopState::Unavailable)]);
    }

    #[test]
    fn ctrl_alt_del_is_detected_in_any_order_of_sides() {
        let d = FakeDesktops::on("Default");
        let mut e = engine(&d);
        let sas = |out: &[ToCore]| out.contains(&ToCore::Sas);
        assert!(!sas(&e.batch(&[key(DELETE, true, false), key(DELETE, true, true)])));
        assert!(sas(&e.batch(&[key(CTRL, false, false), key(ALT, true, false), key(DELETE, true, false)])));
        // Keypad Delete with right Ctrl and left Alt
        let mut e = engine(&d);
        assert!(sas(&e.batch(&[key(CTRL, true, false), key(ALT, false, false), key(DELETE, false, false)])));
        // Ctrl let go first: no
        let mut e = engine(&d);
        assert!(!sas(&e.batch(&[
            key(CTRL, false, false),
            key(ALT, false, false),
            key(CTRL, false, true),
            key(DELETE, true, false)
        ])));
        // Only Ctrl+Del
        assert!(!sas(&engine(&d).batch(&[key(CTRL, false, false), key(DELETE, true, false)])));
    }

    #[test]
    fn records_name_each_input() {
        let mut r = RecordInjector::new(Vec::new());
        assert_eq!(r.inject(&[key(0x1E, false, false), key(0x1D, true, true), Inject::Wheel(-120)]), Ok(3));
        r.note("exit");
        assert_eq!(String::from_utf8(r.out).unwrap(), "key 0x1e down\nkey 0x1d ext up\nwheel -120\nexit\n");
    }
}
