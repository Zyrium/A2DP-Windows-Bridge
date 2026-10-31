//! Changing a setting of the earbuds: the ONLY place that frames a write.
//!
//! Phase F1 (plano_escrita_qcy.md): one setting, game mode (opcode 0x09),
//! is allowed. Everything else is refused here, whatever the caller asks:
//!
//! - [`ALLOWED`]: opcode 0x09 with exactly one parameter byte, `01` (on) or
//!   `02` (off). Any other opcode, length or value: [`Refused`].
//! - [`BLOCKED`]: opcodes that are never written, even if a later phase
//!   widens [`ALLOWED`] by mistake (restore defaults, clear pairings,
//!   factory reset, test modes, standby, rename, fit/EQ tests).
//! - [`check_target`]: a write goes only to service A001 / characteristic
//!   1001. The OTA services (FE2C and 7033 with 2001/2002) are refused.
//!
//! Frame on the wire (A001/1001, ATT Write Command):
//!
//! ```text
//! FF 03 09 01 01   game mode on
//! FF 03 09 01 02   game mode off
//! ```
//!
//! Sources: OpenQCY-Desktop `Protocol/QcyCommands.cs`
//! (`SetGameMode => Toggle(0x09, enabled)`, Toggle = 01 / 02, framed by
//! `QcyPacket.Pack`), hui1601/Quicky `docs/protocol.md` ("0x09 Low Latency,
//! Send: [0x09, 0x01, state], 0x01 enable"), HttpKiwi/OpenQCY (game mode
//! 0x09, not among its reboot opcodes). The read-back is the read the app
//! already does: request `FF 03 FE 01 09`, answer `09 01 <state>` on 1002.
//!
//! [`run`] is the whole per-click flow over a [`Transport`] (the core gives
//! it the resident BTstack link; the tests a mock): read the current value
//! and keep it, send exactly ONE write, read back and compare the bytes.
//! Success only when the read-back matches; no answer within the timeout
//! is a failure, and nothing is ever re-sent.

use std::fmt;
use std::time::{Duration, Instant};

use crate::commands::{self, opcode};
use crate::packet;

/// The opcodes this build may write
pub const ALLOWED: [u8; 1] = [opcode::GAME_MODE];

/// Never written, whatever [`ALLOWED`] says
pub const BLOCKED: [u8; 9] = [
    0x01,                   // restore defaults
    0x02,                   // clear pairings
    0x03,                   // factory reset
    0x0D,                   // test mode
    0x11,                   // ear tip fit test
    0x18,                   // rename (stored in the earbuds)
    0x1F,                   // standby (powers off)
    0x45,                   // EQ test
    packet::REQUEST_OPCODE, // 0xFE is a read, never framed as a write
];

/// GATT services / characteristics that are never written (OTA)
pub const BLOCKED_SERVICES: [u16; 2] = [0xFE2C, 0x7033];
pub const BLOCKED_CHARACTERISTICS: [u16; 2] = [0x2001, 0x2002];

/// The one setting this build changes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    GameMode,
}

impl Setting {
    pub fn opcode(self) -> u8 {
        match self {
            Setting::GameMode => opcode::GAME_MODE,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Setting::GameMode => "game_mode",
        }
    }
    /// The UI's change kind -> the setting; None for everything not allowed
    pub fn from_kind(kind: &str) -> Option<Setting> {
        match kind {
            "game_mode" => Some(Setting::GameMode),
            _ => None,
        }
    }
}

/// Why a write was not framed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Blocked(u8),
    NotAllowed(u8),
    BadParameters(u8, Vec<u8>),
    BlockedTarget(u16, u16),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Blocked(op) => write!(f, "opcode 0x{op:02X} is blocked"),
            Refused::NotAllowed(op) => write!(f, "opcode 0x{op:02X} is not allowed in this version"),
            Refused::BadParameters(op, p) => {
                write!(f, "opcode 0x{op:02X}: parameters {} not allowed", packet::to_hex(p))
            }
            Refused::BlockedTarget(s, c) => write!(f, "service {s:04X} / characteristic {c:04X} is never written"),
        }
    }
}

impl std::error::Error for Refused {}

/// The frame of a write, or why it is refused
pub fn build(op: u8, parameters: &[u8]) -> Result<Vec<u8>, Refused> {
    if BLOCKED.contains(&op) {
        return Err(Refused::Blocked(op));
    }
    if !ALLOWED.contains(&op) {
        return Err(Refused::NotAllowed(op));
    }
    match (op, parameters) {
        (opcode::GAME_MODE, [0x01] | [0x02]) => Ok(packet::frame(op, parameters)),
        _ => Err(Refused::BadParameters(op, parameters.to_vec())),
    }
}

/// The frame that turns `setting` on or off
pub fn toggle_frame(setting: Setting, enabled: bool) -> Result<Vec<u8>, Refused> {
    build(setting.opcode(), &[on_off(enabled)])
}

pub fn on_off(enabled: bool) -> u8 {
    if enabled { 0x01 } else { 0x02 }
}

/// Only A001 / 1001 may be written
pub fn check_target(service: u16, characteristic: u16) -> Result<(), Refused> {
    if BLOCKED_SERVICES.contains(&service)
        || BLOCKED_CHARACTERISTICS.contains(&characteristic)
        || service != crate::uuids::MAIN_SERVICE
        || characteristic != crate::uuids::COMMAND
    {
        return Err(Refused::BlockedTarget(service, characteristic));
    }
    Ok(())
}

/// Whether the read-back `got` (the answer's parameters) confirms that
/// `sent` (the write's parameters) was applied: the first byte must be the
/// one written. A read reports "off" as `02` or `00`, so `00` confirms an
/// off write too; nothing else does (no answer, another value, empty).
pub fn confirms(sent: &[u8], got: &[u8]) -> bool {
    match (sent.first(), got.first()) {
        (Some(&s), Some(&g)) => g == s || (s == 0x02 && g == 0x00),
        _ => false,
    }
}

/// On / off of a read's parameters (01 = on; 02 or 00 = off)
pub fn read_on(got: &[u8]) -> Option<bool> {
    match got.first() {
        Some(0x01) => Some(true),
        Some(0x02 | 0x00) => Some(false),
        _ => None,
    }
}

/// The link the flow talks over: one frame out to 1001, notifications of
/// 1002 in
pub trait Transport {
    /// Writes one frame to A001/1001 (write without response)
    fn send(&mut self, frame: &[u8]) -> Result<(), String>;
    /// The next notification of 1002, or None when nothing came in `wait`
    fn recv(&mut self, wait: Duration) -> Option<Vec<u8>>;
}

/// One step of a write, for the Diagnóstico log and the UI
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub what: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The current value could not be read: nothing was written
    NoPrevious,
    /// The write itself failed to go out
    SendFailed,
    /// No read-back within the timeout
    Timeout,
    /// The read-back shows another value
    Mismatch,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::NoPrevious => "no_previous",
            Failure::SendFailed => "send_failed",
            Failure::Timeout => "timeout",
            Failure::Mismatch => "mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub setting: Setting,
    /// The value asked for
    pub enabled: bool,
    /// The value read before the write (None: not read, nothing written)
    pub previous: Option<bool>,
    /// The frame sent (None: nothing sent)
    pub sent: Option<Vec<u8>>,
    /// The read-back's parameters (None: no answer)
    pub read_back: Option<Vec<u8>>,
    /// None: confirmed by the read-back
    pub failure: Option<Failure>,
    pub steps: Vec<Step>,
}

impl Outcome {
    pub fn confirmed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Waits (until `deadline`) for an answer carrying `op`; its parameters
fn wait_for<T: Transport>(t: &mut T, op: u8, deadline: Instant) -> Option<Vec<u8>> {
    loop {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        let Some(v) = t.recv(left) else { continue };
        if let Some(c) = packet::parse(&v).into_iter().find(|c| c.opcode == op) {
            return Some(c.parameters);
        }
    }
}

/// Discards what already came in (late answers of earlier requests)
fn drain<T: Transport>(t: &mut T) {
    while t.recv(Duration::ZERO).is_some() {}
}

/// The whole write: read and keep the current value, ONE write, read back
/// and compare. `timeout` bounds the first read and, separately, the time
/// from the write to the read-back (3 s in the app). Never re-sends.
pub fn run<T: Transport>(t: &mut T, setting: Setting, enabled: bool, timeout: Duration) -> Outcome {
    let op = setting.opcode();
    let mut out =
        Outcome { setting, enabled, previous: None, sent: None, read_back: None, failure: None, steps: Vec::new() };
    let frame = match toggle_frame(setting, enabled) {
        Ok(f) => f,
        Err(e) => {
            // Unreachable for the Setting enum, kept as a guard
            out.steps.push(Step { what: "refused", detail: e.to_string() });
            out.failure = Some(Failure::SendFailed);
            return out;
        }
    };

    // 1. Read the current value and keep it
    drain(t);
    let request = commands::request(op);
    let previous =
        t.send(&request).ok().and_then(|()| wait_for(t, op, Instant::now() + timeout)).and_then(|p| read_on(&p));
    match previous {
        Some(p) => {
            out.previous = Some(p);
            out.steps.push(Step {
                what: "read_previous",
                detail: format!("{} -> {}", packet::to_hex(&request), on_text(p)),
            });
        }
        None => {
            out.steps.push(Step { what: "read_previous_failed", detail: packet::to_hex(&request) });
            out.failure = Some(Failure::NoPrevious);
            return out;
        }
    }

    // 2. Exactly one write
    if let Err(e) = t.send(&frame) {
        out.steps.push(Step { what: "send_failed", detail: format!("{}: {e}", packet::to_hex(&frame)) });
        out.failure = Some(Failure::SendFailed);
        return out;
    }
    let written = Instant::now();
    out.sent = Some(frame.clone());
    out.steps.push(Step { what: "sent", detail: packet::to_hex(&frame) });

    // 3. Read back (an echo of the write is not taken as the read-back:
    //    what came before the request is discarded)
    std::thread::sleep(Duration::from_millis(150).min(timeout / 4));
    drain(t);
    let deadline = written + timeout;
    let got = t.send(&request).ok().and_then(|()| wait_for(t, op, deadline));
    let sent_params = &frame[4..];
    match got {
        None => {
            out.steps.push(Step { what: "no_read_back", detail: format!("{} ms", timeout.as_millis()) });
            out.failure = Some(Failure::Timeout);
        }
        Some(p) => {
            out.steps.push(Step { what: "read_back", detail: packet::to_hex(&p) });
            if !confirms(sent_params, &p) {
                out.failure = Some(Failure::Mismatch);
            }
            out.read_back = Some(p);
        }
    }
    out
}

fn on_text(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::hex;
    use std::collections::VecDeque;

    #[test]
    fn game_mode_frames() {
        assert_eq!(toggle_frame(Setting::GameMode, true).unwrap(), hex("FF03 09 01 01"));
        assert_eq!(toggle_frame(Setting::GameMode, false).unwrap(), hex("FF03 09 01 02"));
        // The same bytes as the crate's builder (port of OpenQCY's Toggle)
        assert_eq!(toggle_frame(Setting::GameMode, true).unwrap(), commands::set_game_mode(true));
        assert_eq!(toggle_frame(Setting::GameMode, false).unwrap(), commands::set_game_mode(false));
    }

    #[test]
    fn only_game_mode_is_allowed() {
        for op in 0..=u8::MAX {
            for p in [&[0x01u8][..], &[0x02], &[0x00], &[0x01, 0x02], &[]] {
                let r = build(op, p);
                let ok = op == 0x09 && (p == [0x01] || p == [0x02]);
                assert_eq!(r.is_ok(), ok, "0x{op:02X} {p:?}");
            }
        }
        assert_eq!(build(0x09, &[0x03]), Err(Refused::BadParameters(0x09, vec![3])));
        assert_eq!(build(0x09, &[]), Err(Refused::BadParameters(0x09, vec![])));
        assert_eq!(build(0x23, &[0x01]), Err(Refused::NotAllowed(0x23)));
        assert_eq!(build(0x24, &[0x01]), Err(Refused::NotAllowed(0x24)));
        assert_eq!(build(0x14, &[0xFF, 0xFF, 0, 0]), Err(Refused::NotAllowed(0x14)));
        assert!(Setting::from_kind("ldac").is_none());
        assert!(Setting::from_kind("sleep_mode").is_none());
        assert_eq!(Setting::from_kind("game_mode"), Some(Setting::GameMode));
    }

    #[test]
    fn dangerous_opcodes_are_blocked() {
        for op in [0x01, 0x02, 0x03, 0x0D, 0x11, 0x18, 0x1F, 0x45, 0xFE] {
            assert!(BLOCKED.contains(&op));
            assert_eq!(build(op, &[0x01]), Err(Refused::Blocked(op)), "0x{op:02X}");
            assert!(!ALLOWED.contains(&op));
        }
    }

    #[test]
    fn only_the_command_characteristic_is_written() {
        assert!(check_target(0xA001, 0x1001).is_ok());
        assert!(check_target(0xFE2C, 0x1001).is_err());
        for c in [0x1233, 0x1234, 0x1235, 0x1236, 0x1237] {
            assert!(check_target(0xFE2C, c).is_err());
        }
        assert!(check_target(0x7033, 0x2001).is_err());
        assert!(check_target(0x7033, 0x2002).is_err());
        assert!(check_target(0xA001, 0x2001).is_err());
        assert!(check_target(0xA001, 0x1002).is_err());
        assert!(check_target(0xA001, 0x000D).is_err());
    }

    #[test]
    fn read_back_comparison() {
        assert!(confirms(&[0x01], &[0x01]));
        assert!(confirms(&[0x01], &[0x01, 0x00]));
        assert!(confirms(&[0x02], &[0x02]));
        assert!(confirms(&[0x02], &[0x00]));
        assert!(!confirms(&[0x01], &[0x02]));
        assert!(!confirms(&[0x01], &[0x00]));
        assert!(!confirms(&[0x02], &[0x01]));
        assert!(!confirms(&[0x01], &[]));
        assert!(!confirms(&[], &[0x01]));
        assert_eq!(read_on(&[0x01]), Some(true));
        assert_eq!(read_on(&[0x00]), Some(false));
        assert_eq!(read_on(&[0x05]), None);
    }

    /// Answers requests from `state`; a write changes it when `applies`
    struct Mock {
        state: u8,
        applies: bool,
        answers: bool,
        echo: bool,
        sent: Vec<Vec<u8>>,
        inbox: VecDeque<Vec<u8>>,
    }

    impl Mock {
        fn new(state: u8) -> Mock {
            Mock { state, applies: true, answers: true, echo: false, sent: Vec::new(), inbox: VecDeque::new() }
        }
        fn writes(&self) -> Vec<&Vec<u8>> {
            self.sent.iter().filter(|f| f[2] != 0xFE).collect()
        }
    }

    impl Transport for Mock {
        fn send(&mut self, frame: &[u8]) -> Result<(), String> {
            self.sent.push(frame.to_vec());
            if frame[2] == 0xFE {
                if self.answers {
                    self.inbox.push_back(packet::frame(frame[4], &[self.state, 0x00]));
                }
            } else {
                if self.applies {
                    self.state = frame[4];
                }
                if self.echo {
                    // A notification that looks like a confirmation of the
                    // asked value even though it was not applied
                    self.inbox.push_back(packet::frame(frame[2], &[frame[4]]));
                }
            }
            Ok(())
        }
        fn recv(&mut self, wait: Duration) -> Option<Vec<u8>> {
            let v = self.inbox.pop_front();
            if v.is_none() && !wait.is_zero() {
                std::thread::sleep(wait.min(Duration::from_millis(5)));
            }
            v
        }
    }

    const T: Duration = Duration::from_millis(200);

    #[test]
    fn a_confirmed_write() {
        let mut m = Mock::new(0x02);
        let o = run(&mut m, Setting::GameMode, true, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(false));
        assert_eq!(o.sent, Some(hex("FF03090101")));
        assert_eq!(o.read_back.as_deref().map(|p| p[0]), Some(0x01));
        assert_eq!(m.writes(), [&hex("FF03090101")]);
        assert_eq!(m.sent[0], hex("FF03FE0109"));
        assert_eq!(m.sent.last().unwrap(), &hex("FF03FE0109"));
        assert_eq!(o.steps.iter().map(|s| s.what).collect::<Vec<_>>(), ["read_previous", "sent", "read_back"]);
    }

    #[test]
    fn undo_writes_the_saved_value_the_same_way() {
        let mut m = Mock::new(0x02);
        let o = run(&mut m, Setting::GameMode, true, T);
        let undo = run(&mut m, Setting::GameMode, o.previous.unwrap(), T);
        assert!(undo.confirmed());
        assert_eq!(undo.previous, Some(true));
        assert_eq!(m.writes(), [&hex("FF03090101"), &hex("FF03090102")]);
        assert_eq!(m.state, 0x02);
    }

    #[test]
    fn no_read_back_is_a_timeout_and_nothing_is_resent() {
        let mut m = Mock::new(0x02);
        // Answers the first read, then goes silent
        struct Silent(Mock, usize);
        impl Transport for Silent {
            fn send(&mut self, f: &[u8]) -> Result<(), String> {
                self.1 += 1;
                if self.1 > 1 {
                    self.0.answers = false;
                }
                self.0.send(f)
            }
            fn recv(&mut self, w: Duration) -> Option<Vec<u8>> {
                self.0.recv(w)
            }
        }
        let mut s = Silent(std::mem::replace(&mut m, Mock::new(0)), 0);
        let started = Instant::now();
        let o = run(&mut s, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Timeout));
        assert!(started.elapsed() >= T, "waited the timeout");
        assert!(started.elapsed() < T * 4);
        assert_eq!(s.0.writes().len(), 1, "exactly one write, never re-sent");
        assert_eq!(o.previous, Some(false));
    }

    #[test]
    fn a_read_back_that_differs_fails() {
        let mut m = Mock::new(0x02);
        m.applies = false;
        let o = run(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
        assert_eq!(o.read_back.as_deref().map(|p| p[0]), Some(0x02));
        assert_eq!(m.writes().len(), 1);
    }

    #[test]
    fn an_echo_of_the_write_is_not_a_confirmation() {
        let mut m = Mock::new(0x02);
        m.applies = false;
        m.echo = true;
        let o = run(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
    }

    #[test]
    fn nothing_is_written_without_the_previous_value() {
        let mut m = Mock::new(0x02);
        m.answers = false;
        let o = run(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::NoPrevious));
        assert!(m.writes().is_empty());
        assert_eq!(o.sent, None);
    }
}
