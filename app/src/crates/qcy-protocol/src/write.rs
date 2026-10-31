//! Changing a setting of the earbuds: the ONLY place that frames a write.
//!
//! Phase F2 (plano_escrita_qcy.md): three settings may change. Everything
//! else is refused here, whatever the caller asks:
//!
//! - [`ALLOWED`]: exactly 0x09 game mode, 0x10 sleep mode and 0x2A wind
//!   reduction (on/off: one parameter byte, `01` on or `02` off). Any other
//!   opcode, length or value: [`Refused`].
//! - 0x1D prompt volume is NOT written (fase 2b): on the N70 every
//!   `FF 03 1D 01 <v>` read back the old level (04/0F, 2026-10-31), so it
//!   stays read-only until the earbuds' answer is understood.
//! - [`BLOCKED`]: opcodes that are never written, even if a later phase
//!   widens [`ALLOWED`] by mistake (restore defaults, clear pairings,
//!   factory reset, test modes, standby, rename, fit/EQ tests).
//! - [`check_target`]: a write goes only to service A001 / characteristic
//!   1001. The OTA services (FE2C and 7033 with 2001/2002) are refused.
//!
//! Frames on the wire (A001/1001, ATT Write Command), `FF <len+2> <op> <len> <params>`:
//!
//! ```text
//! FF 03 09 01 01 / 02   game mode on / off       read FE 09 -> 09 01 <01|02>
//! FF 03 10 01 01 / 02   sleep mode on / off      read FE 10 -> 10 01 <01|00|02>
//! FF 03 2A 01 01 / 02   wind reduction on / off  read FE 2A -> 2A 01 <01|00|02>
//! FF 03 1D 01 vv        prompt volume vv         read FE 1D -> 1D 02 <vv> <max>
//! ```
//!
//! Sources (confidence in plano_escrita_qcy.md):
//! - 0x09: OpenQCY-Desktop `Protocol/QcyCommands.cs` (`SetGameMode =>
//!   Toggle(0x09, enabled)`), hui1601/Quicky `docs/protocol.md` (0x09
//!   Low Latency, `[0x09, 0x01, state]`), HttpKiwi/OpenQCY. Verified on
//!   the N70 (Gabriel, 2026-10-31).
//! - 0x10: `SetSleepMode => Toggle(0x10, enabled)` (OpenQCY-Desktop),
//!   Quicky "0x10 Sleep Mode, Send: [0x10, 0x01, state], 01 enable / 02
//!   disable", HttpKiwi/OpenQCY `buildSleepModeCommand = toggle(0x10)`.
//!   The N70 reads `FF 03 10 01 00` (probe2): off reads as 00. High.
//! - 0x2A: OpenQCY-Desktop only (`SetWindDetection => Toggle(0x2A,
//!   enabled)`); Quicky and HttpKiwi/OpenQCY do not list it. The N70 reads
//!   `FF 03 2A 01 01` (probe2). Medium.
//! - 0x1D: OpenQCY-Desktop `SetPromptVolume(byte) => Pack(0x1D, [value])`,
//!   Quicky "0x1D Tone Volume, Send: [0x1D, 0x01, volume], response
//!   [0x1D, 0x02, toneVoice, maxVolume]". The N70 reads `FF 04 1D 02 04
//!   0F` (probe2): 4 of 15. The write sends one byte, the read returns two;
//!   the first must equal the value written. Medium-high.
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

/// The opcodes this build may write (exactly these four)
pub const ALLOWED: [u8; 3] = [opcode::GAME_MODE, opcode::SLEEP_MODE, opcode::WIND_DETECTION];

/// How long the raw notifications of 1002 are kept after a write (logged as
/// `notify` steps; at most half the timeout)
pub const NOTIFY_WINDOW: Duration = Duration::from_millis(500);

/// The largest prompt volume ever framed (Quicky: the default maximum when
/// the earbuds report none); [`run`] also caps it at the reported maximum
pub const PROMPT_VOLUME_LIMIT: u8 = 100;

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

/// The settings this build changes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    GameMode,
    SleepMode,
    WindReduction,
    /// Read-only in this build: not in [`Setting::ALL`] nor [`ALLOWED`], so
    /// [`run`] refuses it before any traffic
    PromptVolume,
}

impl Setting {
    /// The writable settings (prompt volume is read-only)
    pub const ALL: [Setting; 3] = [Setting::GameMode, Setting::SleepMode, Setting::WindReduction];

    pub fn opcode(self) -> u8 {
        match self {
            Setting::GameMode => opcode::GAME_MODE,
            Setting::SleepMode => opcode::SLEEP_MODE,
            Setting::WindReduction => opcode::WIND_DETECTION,
            Setting::PromptVolume => opcode::PROMPT_VOLUME,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Setting::GameMode => "game_mode",
            Setting::SleepMode => "sleep_mode",
            Setting::WindReduction => "wind_reduction",
            Setting::PromptVolume => "prompt_volume",
        }
    }
    /// On/off (01/02) rather than a level
    pub fn is_toggle(self) -> bool {
        !matches!(self, Setting::PromptVolume)
    }
    /// The UI's change kind -> the setting; None for everything not allowed
    pub fn from_kind(kind: &str) -> Option<Setting> {
        Setting::ALL.into_iter().find(|s| s.key() == kind)
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
        (opcode::GAME_MODE | opcode::SLEEP_MODE | opcode::WIND_DETECTION, [0x01] | [0x02]) => {
            Ok(packet::frame(op, parameters))
        }
        _ => Err(Refused::BadParameters(op, parameters.to_vec())),
    }
}

/// The frame that turns `setting` on or off
pub fn toggle_frame(setting: Setting, enabled: bool) -> Result<Vec<u8>, Refused> {
    if !setting.is_toggle() {
        return Err(Refused::BadParameters(setting.opcode(), vec![on_off(enabled)]));
    }
    build(setting.opcode(), &[on_off(enabled)])
}

/// The frame of `setting` with its raw parameter byte (01/02 for a
/// toggle, the level for the prompt volume)
pub fn frame_for(setting: Setting, value: u8) -> Result<Vec<u8>, Refused> {
    build(setting.opcode(), &[value])
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

/// [`confirms`] for `setting`: a level must read back exactly (00 is a
/// level, not "off")
pub fn confirms_for(setting: Setting, sent: &[u8], got: &[u8]) -> bool {
    if setting.is_toggle() {
        confirms(sent, got)
    } else {
        matches!((sent.first(), got.first()), (Some(s), Some(g)) if s == g)
    }
}

/// The value of a read as the raw parameter of a write: a toggle's 01 /
/// 02 (00 reads as 02), a level's first byte
pub fn read_value(setting: Setting, got: &[u8]) -> Option<u8> {
    if setting.is_toggle() { read_on(got).map(on_off) } else { got.first().copied() }
}

/// The maximum prompt volume a read reports (second byte), if any
pub fn read_max(got: &[u8]) -> Option<u8> {
    got.get(1).copied().filter(|m| *m > 0)
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
    /// The level is above the maximum the earbuds reported: nothing written
    OutOfRange,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::NoPrevious => "no_previous",
            Failure::SendFailed => "send_failed",
            Failure::Timeout => "timeout",
            Failure::Mismatch => "mismatch",
            Failure::OutOfRange => "out_of_range",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub setting: Setting,
    /// The value asked for, as the write's parameter byte (01/02 for a
    /// toggle, the level for the prompt volume)
    pub value: u8,
    /// The value read before the write, same encoding (None: not read,
    /// nothing written)
    pub previous: Option<u8>,
    /// The maximum the earbuds reported (prompt volume only)
    pub max: Option<u8>,
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
    /// The asked value as on/off (toggles)
    pub fn enabled(&self) -> bool {
        self.value == 0x01
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

/// [`run`] for a toggle: on or off
pub fn run_toggle<T: Transport>(t: &mut T, setting: Setting, enabled: bool, timeout: Duration) -> Outcome {
    run(t, setting, on_off(enabled), timeout)
}

/// The whole write: read and keep the current value, ONE write, read back
/// and compare. `value` is the write's parameter byte. `timeout` bounds the
/// first read and, separately, the time from the write to the read-back
/// (3 s in the app). Never re-sends.
pub fn run<T: Transport>(t: &mut T, setting: Setting, value: u8, timeout: Duration) -> Outcome {
    let op = setting.opcode();
    let mut out = Outcome {
        setting,
        value,
        previous: None,
        max: None,
        sent: None,
        read_back: None,
        failure: None,
        steps: Vec::new(),
    };
    let frame = match frame_for(setting, value) {
        Ok(f) => f,
        Err(e) => {
            out.steps.push(Step { what: "refused", detail: e.to_string() });
            out.failure = Some(Failure::OutOfRange);
            return out;
        }
    };

    // 1. Read the current value and keep it
    drain(t);
    let request = commands::request(op);
    let read = t.send(&request).ok().and_then(|()| wait_for(t, op, Instant::now() + timeout));
    match read.as_deref().and_then(|p| read_value(setting, p).map(|v| (v, p))) {
        Some((p, raw)) => {
            out.previous = Some(p);
            out.max = if setting.is_toggle() { None } else { read_max(raw) };
            out.steps.push(Step {
                what: "read_previous",
                detail: format!("{} -> {}", packet::to_hex(&request), value_text(setting, p, out.max)),
            });
        }
        None => {
            out.steps.push(Step { what: "read_previous_failed", detail: packet::to_hex(&request) });
            out.failure = Some(Failure::NoPrevious);
            return out;
        }
    }
    // A level above what the earbuds report as their maximum is never sent
    if let Some(max) = out.max
        && value > max
    {
        out.steps.push(Step { what: "refused", detail: format!("{value} > {max}") });
        out.failure = Some(Failure::OutOfRange);
        return out;
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

    // 3. The raw notifications of 1002 right after the write (an ack, an
    //    echo or a state push), kept as steps for the log; none of them is
    //    taken as the read-back
    let listen_until = written + NOTIFY_WINDOW.min(timeout / 2);
    while let Some(left) = listen_until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        if let Some(v) = t.recv(left) {
            out.steps.push(Step { what: "notify", detail: packet::to_hex(&v) });
        }
    }
    while let Some(v) = t.recv(Duration::ZERO) {
        out.steps.push(Step { what: "notify", detail: packet::to_hex(&v) });
    }

    // 4. Read back
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
            if !confirms_for(setting, sent_params, &p) {
                out.failure = Some(Failure::Mismatch);
            }
            out.read_back = Some(p);
        }
    }
    out
}

fn value_text(setting: Setting, v: u8, max: Option<u8>) -> String {
    if setting.is_toggle() {
        on_text(v == 0x01).to_string()
    } else {
        format!("{v}/{}", max.map_or_else(|| "?".to_string(), |m| m.to_string()))
    }
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
    fn f2_frames() {
        assert_eq!(toggle_frame(Setting::SleepMode, true).unwrap(), hex("FF03 10 01 01"));
        assert_eq!(toggle_frame(Setting::SleepMode, false).unwrap(), hex("FF03 10 01 02"));
        assert_eq!(toggle_frame(Setting::WindReduction, true).unwrap(), hex("FF03 2A 01 01"));
        assert_eq!(toggle_frame(Setting::WindReduction, false).unwrap(), hex("FF03 2A 01 02"));
        // Prompt volume is read-only in this build
        assert_eq!(frame_for(Setting::PromptVolume, 12), Err(Refused::NotAllowed(0x1D)));
        // The same bytes as the crate's builders (ports of OpenQCY-Desktop)
        assert_eq!(toggle_frame(Setting::SleepMode, true).unwrap(), commands::set_sleep_mode(true));
        assert_eq!(toggle_frame(Setting::WindReduction, false).unwrap(), commands::set_wind_detection(false));
        // A level is not a toggle
        assert!(toggle_frame(Setting::PromptVolume, true).is_err());
    }

    #[test]
    fn exactly_the_three_settings_are_allowed() {
        assert_eq!(ALLOWED, [0x09, 0x10, 0x2A]);
        for op in 0..=u8::MAX {
            for p in [&[0x01u8][..], &[0x02], &[0x00], &[0x0F], &[0x64], &[0x65], &[0xFF], &[0x01, 0x02], &[]] {
                let r = build(op, p);
                let ok = match op {
                    0x09 | 0x10 | 0x2A => p == [0x01] || p == [0x02],
                    _ => false,
                };
                assert_eq!(r.is_ok(), ok, "0x{op:02X} {p:?}");
            }
        }
        assert_eq!(build(0x09, &[0x03]), Err(Refused::BadParameters(0x09, vec![3])));
        assert_eq!(build(0x10, &[0x00]), Err(Refused::BadParameters(0x10, vec![0])));
        assert_eq!(build(0x2A, &[]), Err(Refused::BadParameters(0x2A, vec![])));
        assert_eq!(build(0x1D, &[0x04]), Err(Refused::NotAllowed(0x1D)));
        assert!(Setting::from_kind("prompt_volume").is_none());
        assert_eq!(build(0x23, &[0x01]), Err(Refused::NotAllowed(0x23)));
        assert_eq!(build(0x24, &[0x01]), Err(Refused::NotAllowed(0x24)));
        assert_eq!(build(0x2C, &[0x01]), Err(Refused::NotAllowed(0x2C)));
        assert_eq!(build(0x14, &[0xFF, 0xFF, 0, 0]), Err(Refused::NotAllowed(0x14)));
        assert!(Setting::from_kind("ldac").is_none());
        assert!(Setting::from_kind("multipoint").is_none());
        assert!(Setting::from_kind("reapply_on_connect").is_none());
        for s in Setting::ALL {
            assert_eq!(Setting::from_kind(s.key()), Some(s));
            assert!(ALLOWED.contains(&s.opcode()));
            assert!(!BLOCKED.contains(&s.opcode()));
        }
    }

    #[test]
    fn level_read_back_is_exact() {
        assert!(confirms_for(Setting::PromptVolume, &[0x0C], &[0x0C, 0x0F]));
        assert!(!confirms_for(Setting::PromptVolume, &[0x02], &[0x00, 0x0F]));
        assert!(!confirms_for(Setting::PromptVolume, &[0x0C], &[0x0B, 0x0F]));
        assert!(!confirms_for(Setting::PromptVolume, &[0x0C], &[]));
        assert!(confirms_for(Setting::SleepMode, &[0x02], &[0x00]));
        assert_eq!(read_value(Setting::SleepMode, &[0x00]), Some(0x02));
        assert_eq!(read_value(Setting::WindReduction, &[0x01]), Some(0x01));
        assert_eq!(read_value(Setting::PromptVolume, &[0x04, 0x0F]), Some(4));
        assert_eq!(read_max(&[0x04, 0x0F]), Some(15));
        assert_eq!(read_max(&[0x04]), None);
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
        /// Second byte of a read (the prompt volume's maximum)
        max: u8,
    }

    impl Mock {
        fn new(state: u8) -> Mock {
            Mock { state, applies: true, answers: true, echo: false, sent: Vec::new(), inbox: VecDeque::new(), max: 0 }
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
                    self.inbox.push_back(packet::frame(frame[4], &[self.state, self.max]));
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
        let o = run_toggle(&mut m, Setting::GameMode, true, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(0x02));
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
        let o = run_toggle(&mut m, Setting::GameMode, true, T);
        let undo = run(&mut m, Setting::GameMode, o.previous.unwrap(), T);
        assert!(undo.confirmed());
        assert_eq!(undo.previous, Some(0x01));
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
        let o = run_toggle(&mut s, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Timeout));
        assert!(started.elapsed() >= T, "waited the timeout");
        assert!(started.elapsed() < T * 4);
        assert_eq!(s.0.writes().len(), 1, "exactly one write, never re-sent");
        assert_eq!(o.previous, Some(0x02));
    }

    #[test]
    fn a_read_back_that_differs_fails() {
        let mut m = Mock::new(0x02);
        m.applies = false;
        let o = run_toggle(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
        assert_eq!(o.read_back.as_deref().map(|p| p[0]), Some(0x02));
        assert_eq!(m.writes().len(), 1);
    }

    #[test]
    fn an_echo_of_the_write_is_not_a_confirmation() {
        let mut m = Mock::new(0x02);
        m.applies = false;
        m.echo = true;
        let o = run_toggle(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
    }

    #[test]
    fn nothing_is_written_without_the_previous_value() {
        let mut m = Mock::new(0x02);
        m.answers = false;
        let o = run_toggle(&mut m, Setting::GameMode, true, T);
        assert_eq!(o.failure, Some(Failure::NoPrevious));
        assert!(m.writes().is_empty());
        assert_eq!(o.sent, None);
    }

    #[test]
    fn sleep_and_wind_go_through_the_same_flow() {
        let mut m = Mock::new(0x00); // the N70 reads off as 00
        let o = run_toggle(&mut m, Setting::SleepMode, true, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(0x02));
        assert_eq!(m.sent[0], hex("FF03FE0110"));
        assert_eq!(m.writes(), [&hex("FF03100101")]);
        let mut m = Mock::new(0x01);
        let o = run_toggle(&mut m, Setting::WindReduction, false, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(0x01));
        assert_eq!(m.writes(), [&hex("FF032A0102")]);
        let undo = run(&mut m, Setting::WindReduction, o.previous.unwrap(), T);
        assert!(undo.confirmed());
        assert_eq!(m.writes(), [&hex("FF032A0102"), &hex("FF032A0101")]);
    }

    #[test]
    fn prompt_volume_is_refused_before_any_traffic() {
        for v in [0, 5, 10, 15, 0xFF] {
            let mut m = Mock::new(4);
            m.max = 15;
            let o = run(&mut m, Setting::PromptVolume, v, T);
            assert_eq!(o.failure, Some(Failure::OutOfRange), "{o:?}");
            assert!(m.sent.is_empty(), "nothing sent for {v}");
            assert_eq!(o.sent, None);
        }
    }

    #[test]
    fn notifications_after_the_write_are_logged_not_confirmations() {
        let mut m = Mock::new(0x02);
        m.applies = false;
        m.echo = true;
        let o = run_toggle(&mut m, Setting::SleepMode, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
        let notes: Vec<_> = o.steps.iter().filter(|s| s.what == "notify").map(|s| s.detail.as_str()).collect();
        assert_eq!(notes, ["FF03100101"]);
        assert_eq!(
            o.steps.iter().map(|s| s.what).collect::<Vec<_>>(),
            ["read_previous", "sent", "notify", "read_back"]
        );
    }
}
