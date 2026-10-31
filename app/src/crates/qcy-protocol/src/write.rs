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
//! - Phase F3: 0x17 noise control, ONLY the seven 3-byte states of
//!   [`NOISE_PRESETS`] (five ANC scenes, normal `02 00 00`, transparency
//!   `03 01 04`). The read-back must match the three bytes exactly. After a
//!   confirmed 0x17 write, 0x2A (wind reduction) is READ again (never
//!   written) so the screen shows its real state.
//! - Phase F3b: 0x2C wear detection ("Pausar ao tirar o fone"), on `01`
//!   or off `02`. The write copies the music and ANC action bytes (and the
//!   tone byte, when the firmware sends one) EXACTLY from the read made
//!   right before it: `FF 05 2C 03 <01|02> <music> <anc>` or
//!   `FF 06 2C 04 <01|02> <music> <anc> <tone>`. Confirmed when the
//!   read-back's state is the one written (off reads back as `00`) and the
//!   other bytes did not change. Experimental (not yet verified on the N70).
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
//! FF 05 17 03 m s v     noise control (F3)       read FE 17 -> 17 03 <m> <s> <v>
//! FF 05 2C 03 e m a     wear detection (F3b)     read FE 2C -> 2C 03 <01|00> <m> <a> [t]
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

/// The opcodes this build may write (exactly these five)
pub const ALLOWED: [u8; 5] =
    [opcode::GAME_MODE, opcode::SLEEP_MODE, opcode::WIND_DETECTION, opcode::NOISE_CONTROL, opcode::WEARING_DETECTION];

/// The highest music / ANC action byte copied into a 0x2C write (the N70
/// reports `01`; anything above is not a known action and is never
/// written back)
pub const WEAR_ACTION_LIMIT: u8 = 0x0F;

/// Whether `p` are the parameters of an acceptable 0x2C write: state 01/02,
/// music and ANC actions 00..=[`WEAR_ACTION_LIMIT`], optional tone 01/02
fn wear_parameters_ok(p: &[u8]) -> bool {
    let actions_ok = |m: u8, a: u8| m <= WEAR_ACTION_LIMIT && a <= WEAR_ACTION_LIMIT;
    match *p {
        [0x01 | 0x02, m, a] => actions_ok(m, a),
        [0x01 | 0x02, m, a, 0x01 | 0x02] => actions_ok(m, a),
        _ => false,
    }
}

/// The 0x2C write that turns wear detection on (`01`) or off (`02`),
/// copying the music / ANC (/ tone) bytes of `current` (the read's
/// parameters). Refused when the read is not a 3- or 4-byte 0x2C answer.
pub fn wear_frame(value: u8, current: &[u8]) -> Result<Vec<u8>, Refused> {
    let op = opcode::WEARING_DETECTION;
    if !matches!(value, 0x01 | 0x02) {
        return Err(Refused::BadParameters(op, vec![value]));
    }
    let w = commands::WearingDetection::parse(current).ok_or_else(|| Refused::BadParameters(op, current.to_vec()))?;
    let mut p = vec![value, w.music_action, w.anc_action];
    if current.len() == 4 {
        // The tone byte as the firmware sent it (00 is written back as 02)
        p.push(if current[3] == 0x01 { 0x01 } else { 0x02 });
    }
    build(op, &p)
}

/// The only 0x17 states ever written, by the code the UI sends (`value` of
/// `{ kind: "noise_control" }`). Sources: OpenQCY-Desktop
/// `QcyCommands.cs` and the N70 3.0.13 transcript (ANC scenes: high
/// confidence); normal and transparency: medium (transparency's last two
/// bytes not confirmed on the N70).
pub const NOISE_PRESETS: [(u8, &str, [u8; 3]); 7] = [
    (0, "off", [0x02, 0x00, 0x00]),
    (1, "anc_adaptive", [0x01, 0x05, 0x00]),
    (2, "anc_indoor", [0x01, 0x01, 0x02]),
    (3, "anc_commuting", [0x01, 0x02, 0x02]),
    (4, "anc_noisy", [0x01, 0x03, 0x02]),
    (5, "anc_anti_wind", [0x01, 0x04, 0x00]),
    (6, "transparency", [0x03, 0x01, 0x04]),
];

/// The three bytes of a noise-control code (None: not one of the seven)
pub fn noise_bytes(code: u8) -> Option<[u8; 3]> {
    NOISE_PRESETS.iter().find(|p| p.0 == code).map(|p| p.2)
}

/// The name of a noise-control code (for the log)
pub fn noise_name(code: u8) -> &'static str {
    NOISE_PRESETS.iter().find(|p| p.0 == code).map_or("?", |p| p.1)
}

/// The code of a 0x17 read: the preset with exactly these three bytes,
/// else the preset of the same mode and scene (parsed), else None (then
/// nothing is written: there is no known value to go back to)
pub fn noise_code(got: &[u8]) -> Option<u8> {
    let three = got.get(..3)?;
    if let Some(p) = NOISE_PRESETS.iter().find(|p| p.2 == three) {
        return Some(p.0);
    }
    let parsed = commands::NoiseControlState::parse(three)?;
    NOISE_PRESETS
        .iter()
        .find(|p| {
            commands::NoiseControlState::parse(&p.2).is_some_and(|q| q.mode == parsed.mode && q.scene == parsed.scene)
        })
        .map(|p| p.0)
}

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
    /// 0x17 (phase F3): the value is a code of [`NOISE_PRESETS`]
    NoiseControl,
    /// 0x2C (phase F3b): on/off; the other bytes come from the read
    WearDetection,
}

impl Setting {
    /// The writable settings (prompt volume is read-only)
    pub const ALL: [Setting; 5] =
        [Setting::GameMode, Setting::SleepMode, Setting::WindReduction, Setting::NoiseControl, Setting::WearDetection];

    pub fn opcode(self) -> u8 {
        match self {
            Setting::GameMode => opcode::GAME_MODE,
            Setting::SleepMode => opcode::SLEEP_MODE,
            Setting::WindReduction => opcode::WIND_DETECTION,
            Setting::PromptVolume => opcode::PROMPT_VOLUME,
            Setting::NoiseControl => opcode::NOISE_CONTROL,
            Setting::WearDetection => opcode::WEARING_DETECTION,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Setting::GameMode => "game_mode",
            Setting::SleepMode => "sleep_mode",
            Setting::WindReduction => "wind_reduction",
            Setting::PromptVolume => "prompt_volume",
            Setting::NoiseControl => "noise_control",
            Setting::WearDetection => "wear_detection",
        }
    }
    /// On/off (01/02) rather than a level
    pub fn is_toggle(self) -> bool {
        !matches!(self, Setting::PromptVolume | Setting::NoiseControl)
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
        (opcode::NOISE_CONTROL, p) if NOISE_PRESETS.iter().any(|n| n.2 == p) => Ok(packet::frame(op, p)),
        (opcode::WEARING_DETECTION, p) if wear_parameters_ok(p) => Ok(packet::frame(op, p)),
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

/// Whether `value` may be asked for `setting` (the check before any
/// traffic; for wear detection, `01` or `02` with 0x2C allowed)
pub fn check_value(setting: Setting, value: u8) -> Result<(), Refused> {
    if setting == Setting::WearDetection {
        let op = opcode::WEARING_DETECTION;
        if BLOCKED.contains(&op) {
            return Err(Refused::Blocked(op));
        }
        if !ALLOWED.contains(&op) {
            return Err(Refused::NotAllowed(op));
        }
        return if matches!(value, 0x01 | 0x02) { Ok(()) } else { Err(Refused::BadParameters(op, vec![value])) };
    }
    frame_for(setting, value).map(|_| ())
}

/// The frame of `setting` with its raw parameter byte (01/02 for a
/// toggle, the level for the prompt volume)
///
/// Wear detection has no frame without the current read: use
/// [`check_value`] to validate and [`wear_frame`] to frame it.
pub fn frame_for(setting: Setting, value: u8) -> Result<Vec<u8>, Refused> {
    if setting == Setting::WearDetection {
        return Err(Refused::BadParameters(opcode::WEARING_DETECTION, vec![value]));
    }
    if setting == Setting::NoiseControl {
        let bytes = noise_bytes(value).ok_or_else(|| Refused::BadParameters(opcode::NOISE_CONTROL, vec![value]))?;
        return build(opcode::NOISE_CONTROL, &bytes);
    }
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
    if setting == Setting::WearDetection {
        wear_confirms(sent, got)
    } else if setting.is_toggle() {
        confirms(sent, got)
    } else if setting == Setting::NoiseControl {
        // All three bytes, exactly
        sent.len() == 3 && got.get(..3) == Some(sent)
    } else {
        matches!((sent.first(), got.first()), (Some(s), Some(g)) if s == g)
    }
}

/// A 0x2C read-back confirms a write when its state is the one written
/// (off reads back as `00` or `02`), the music and ANC bytes are exactly
/// the ones written (copied from the earlier read), and, when a tone byte
/// was written, the read reports the same tone on/off
pub fn wear_confirms(sent: &[u8], got: &[u8]) -> bool {
    if !(3..=4).contains(&sent.len()) || got.len() < 3 || !confirms(sent, got) || got[1..3] != sent[1..3] {
        return false;
    }
    match (sent.get(3), got.get(3)) {
        (None, _) => true,
        (Some(&t), Some(&g)) => read_on(&[t]) == read_on(&[g]),
        (Some(_), None) => false,
    }
}

/// The value of a read as the raw parameter of a write: a toggle's 01 /
/// 02 (00 reads as 02), a level's first byte
pub fn read_value(setting: Setting, got: &[u8]) -> Option<u8> {
    match setting {
        Setting::NoiseControl => noise_code(got),
        // Only a well-formed 0x2C answer (3 or 4 bytes) is a known value
        Setting::WearDetection => {
            commands::WearingDetection::parse(got)?;
            read_on(got).map(on_off)
        }
        s if s.is_toggle() => read_on(got).map(on_off),
        _ => got.first().copied(),
    }
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
    /// 0x2A read again after a confirmed 0x17 write (its parameters; None:
    /// not read or no answer). Never written.
    pub wind_after: Option<Vec<u8>>,
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
        wind_after: None,
        steps: Vec::new(),
    };
    let checked = if setting == Setting::WearDetection {
        check_value(setting, value).map(|()| Vec::new())
    } else {
        frame_for(setting, value)
    };
    let mut frame = match checked {
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
            out.max = if setting == Setting::PromptVolume { read_max(raw) } else { None };
            out.steps.push(Step {
                what: "read_previous",
                detail: format!(
                    "{} -> {} ({})",
                    packet::to_hex(&request),
                    value_text(setting, p, out.max),
                    packet::to_hex(raw)
                ),
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

    // Wear detection: the frame copies the bytes just read
    if setting == Setting::WearDetection {
        match read.as_deref().map(|raw| wear_frame(value, raw)) {
            Some(Ok(f)) => frame = f,
            Some(Err(e)) => {
                out.steps.push(Step { what: "refused", detail: e.to_string() });
                out.failure = Some(Failure::NoPrevious);
                return out;
            }
            None => {
                out.failure = Some(Failure::NoPrevious);
                return out;
            }
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

    // 5. Noise control confirmed: read 0x2A again (wind reduction only
    //    works with ANC; the earbuds may have turned it off). A read, never
    //    a write.
    if setting == Setting::NoiseControl && out.failure.is_none() {
        let wind = commands::request(opcode::WIND_DETECTION);
        let got = t.send(&wind).ok().and_then(|()| wait_for(t, opcode::WIND_DETECTION, Instant::now() + timeout));
        out.steps.push(Step {
            what: if got.is_some() { "read_wind" } else { "read_wind_failed" },
            detail: got.as_deref().map_or_else(|| packet::to_hex(&wind), packet::to_hex),
        });
        out.wind_after = got;
    }
    out
}

fn value_text(setting: Setting, v: u8, max: Option<u8>) -> String {
    if setting == Setting::NoiseControl {
        noise_name(v).to_string()
    } else if setting.is_toggle() {
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
    fn exactly_the_five_settings_are_allowed() {
        assert_eq!(ALLOWED, [0x09, 0x10, 0x2A, 0x17, 0x2C]);
        for op in 0..=u8::MAX {
            for p in [&[0x01u8][..], &[0x02], &[0x00], &[0x0F], &[0x64], &[0x65], &[0xFF], &[0x01, 0x02], &[]] {
                let r = build(op, p);
                let ok = match op {
                    0x09 | 0x10 | 0x2A => p == [0x01] || p == [0x02],
                    // 0x17 and 0x2C take three or four bytes: none of these
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
        assert_eq!(build(0x2C, &[0x01]), Err(Refused::BadParameters(0x2C, vec![1])));
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

    #[test]
    fn noise_control_takes_only_the_seven_states() {
        let want = [
            (0, "FF05170302 0000"),
            (1, "FF05170301 0500"),
            (2, "FF05170301 0102"),
            (3, "FF05170301 0202"),
            (4, "FF05170301 0302"),
            (5, "FF05170301 0400"),
            (6, "FF05170303 0104"),
        ];
        for (code, frame) in want {
            assert_eq!(frame_for(Setting::NoiseControl, code).unwrap(), hex(frame), "code {code}");
        }
        // Same bytes as the crate's builders for the ANC scenes and normal
        use commands::{NoiseMode, NoiseScene};
        assert_eq!(
            frame_for(Setting::NoiseControl, 1).unwrap(),
            commands::set_noise_mode(NoiseMode::NoiseCancellation, NoiseScene::Adaptive)
        );
        assert_eq!(
            frame_for(Setting::NoiseControl, 4).unwrap(),
            commands::set_noise_mode(NoiseMode::NoiseCancellation, NoiseScene::Noisy)
        );
        assert_eq!(
            frame_for(Setting::NoiseControl, 0).unwrap(),
            commands::set_noise_mode(NoiseMode::Normal, NoiseScene::Adaptive)
        );
        for code in [7, 8, 0x17, 0xFF] {
            assert!(frame_for(Setting::NoiseControl, code).is_err(), "code {code}");
        }
        // Every other 3-byte combination is refused
        let mut ok = 0;
        for a in 0..=4u8 {
            for b in 0..=6u8 {
                for c in 0..=5u8 {
                    if build(0x17, &[a, b, c]).is_ok() {
                        ok += 1;
                        assert!(NOISE_PRESETS.iter().any(|n| n.2 == [a, b, c]));
                    }
                }
            }
        }
        assert_eq!(ok, 7);
        assert!(build(0x17, &[0x01]).is_err());
        assert!(build(0x17, &[0x01, 0x05, 0x00, 0x00]).is_err());
        assert!(build(0x17, &[]).is_err());
        assert_eq!(Setting::from_kind("noise_control"), Some(Setting::NoiseControl));
        assert!(Setting::from_kind("noise").is_none());
    }

    #[test]
    fn noise_reads_and_exact_confirmation() {
        assert_eq!(noise_code(&[0x01, 0x05, 0x00]), Some(1));
        assert_eq!(noise_code(&[0x03, 0x01, 0x04]), Some(6));
        assert_eq!(noise_code(&[0x02, 0x00, 0x00]), Some(0));
        // Same mode and scene, other firmware parameter: the canonical code
        assert_eq!(noise_code(&[0x01, 0x03, 0x01]), Some(4));
        assert_eq!(noise_code(&[0x03, 0x02, 0x07]), Some(6));
        assert_eq!(noise_code(&[0x09, 0x00, 0x00]), None);
        assert_eq!(noise_code(&[0x01, 0x05]), None);
        assert!(confirms_for(Setting::NoiseControl, &[0x01, 0x03, 0x02], &[0x01, 0x03, 0x02]));
        assert!(!confirms_for(Setting::NoiseControl, &[0x01, 0x03, 0x02], &[0x01, 0x03, 0x01]));
        assert!(!confirms_for(Setting::NoiseControl, &[0x03, 0x01, 0x04], &[0x03, 0x01]));
        assert!(!confirms_for(Setting::NoiseControl, &[0x02, 0x00, 0x00], &[0x00]));
    }

    /// Earbuds with a 0x17 state and a 0x2A state
    struct NoiseMock {
        noise: [u8; 3],
        wind: u8,
        applies: bool,
        /// Answers reads after the write
        answers_after: bool,
        /// Wind reduction turns off when ANC goes off (as on the N70)
        drops_wind: bool,
        written: bool,
        sent: Vec<Vec<u8>>,
        inbox: VecDeque<Vec<u8>>,
    }

    impl NoiseMock {
        fn new(noise: [u8; 3]) -> NoiseMock {
            NoiseMock {
                noise,
                wind: 0x01,
                applies: true,
                answers_after: true,
                drops_wind: true,
                written: false,
                sent: Vec::new(),
                inbox: VecDeque::new(),
            }
        }
        fn writes(&self) -> Vec<&Vec<u8>> {
            self.sent.iter().filter(|f| f[2] != 0xFE).collect()
        }
    }

    impl Transport for NoiseMock {
        fn send(&mut self, frame: &[u8]) -> Result<(), String> {
            self.sent.push(frame.to_vec());
            if frame[2] == 0xFE {
                if self.written && !self.answers_after {
                    return Ok(());
                }
                match frame[4] {
                    0x17 => self.inbox.push_back(packet::frame(0x17, &self.noise)),
                    0x2A => self.inbox.push_back(packet::frame(0x2A, &[self.wind])),
                    _ => {}
                }
            } else {
                self.written = true;
                if self.applies && frame[2] == 0x17 {
                    self.noise = [frame[4], frame[5], frame[6]];
                    if self.drops_wind && frame[4] != 0x01 {
                        self.wind = 0x00;
                    }
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

    #[test]
    fn transparency_is_confirmed_and_wind_is_read_not_written() {
        let mut m = NoiseMock::new([0x01, 0x05, 0x00]);
        let o = run(&mut m, Setting::NoiseControl, 6, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(1));
        assert_eq!(o.sent, Some(hex("FF05170303 0104")));
        assert_eq!(m.writes(), [&hex("FF05170303 0104")], "one write, no 0x2A write");
        assert_eq!(m.sent[0], hex("FF03FE0117"));
        assert_eq!(m.sent.last().unwrap(), &hex("FF03FE012A"), "0x2A is only read");
        assert_eq!(o.wind_after, Some(vec![0x00]));
        assert_eq!(
            o.steps.iter().map(|s| s.what).collect::<Vec<_>>(),
            ["read_previous", "sent", "read_back", "read_wind"]
        );
        // Desfazer: the saved code, the same way
        let undo = run(&mut m, Setting::NoiseControl, o.previous.unwrap(), T);
        assert!(undo.confirmed());
        assert_eq!(m.writes(), [&hex("FF05170303 0104"), &hex("FF05170301 0500")]);
    }

    #[test]
    fn a_silent_noise_write_times_out_once() {
        let mut m = NoiseMock::new([0x01, 0x05, 0x00]);
        m.answers_after = false;
        let started = Instant::now();
        let o = run(&mut m, Setting::NoiseControl, 4, T);
        assert_eq!(o.failure, Some(Failure::Timeout));
        assert!(started.elapsed() >= T);
        assert_eq!(m.writes(), [&hex("FF05170301 0302")], "never re-sent");
        assert_eq!(o.previous, Some(1));
        assert_eq!(o.wind_after, None, "no 0x2A read after a failure");
        assert!(!m.sent.iter().any(|f| f == &hex("FF03FE012A")));
    }

    #[test]
    fn a_noise_read_back_with_other_bytes_fails() {
        let mut m = NoiseMock::new([0x01, 0x05, 0x00]);
        m.applies = false;
        let o = run(&mut m, Setting::NoiseControl, 0, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
        assert_eq!(o.read_back, Some(vec![0x01, 0x05, 0x00]));
        assert_eq!(m.writes().len(), 1);
    }

    #[test]
    fn an_unknown_noise_state_is_never_overwritten() {
        let mut m = NoiseMock::new([0x07, 0x00, 0x00]);
        let o = run(&mut m, Setting::NoiseControl, 1, T);
        assert_eq!(o.failure, Some(Failure::NoPrevious));
        assert!(m.writes().is_empty());
        let mut m = NoiseMock::new([0x01, 0x05, 0x00]);
        let o = run(&mut m, Setting::NoiseControl, 9, T);
        assert_eq!(o.failure, Some(Failure::OutOfRange));
        assert!(m.sent.is_empty(), "a bad code sends nothing, not even a read");
    }

    // ---- Phase F3b: 0x2C wear detection ----

    #[test]
    fn wear_frames_copy_the_read_bytes() {
        // The N70 transcript: off reads 00 01 01
        assert_eq!(wear_frame(0x01, &[0x00, 0x01, 0x01]).unwrap(), hex("FF052C03010101"));
        assert_eq!(wear_frame(0x02, &[0x01, 0x01, 0x01]).unwrap(), hex("FF052C03020101"));
        // Other actions are copied as they are
        assert_eq!(wear_frame(0x01, &[0x02, 0x03, 0x00]).unwrap(), hex("FF052C03010300"));
        // With a tone byte: FF 06 2C 04
        assert_eq!(wear_frame(0x01, &[0x00, 0x02, 0x03, 0x01]).unwrap(), hex("FF062C0401020301"));
        assert_eq!(wear_frame(0x02, &[0x01, 0x02, 0x03, 0x02]).unwrap(), hex("FF062C0402020302"));
        assert_eq!(wear_frame(0x02, &[0x01, 0x02, 0x03, 0x00]).unwrap(), hex("FF062C0402020302"));
        // Same bytes as the crate's builder
        let w = commands::WearingDetection::parse(&[0x00, 0x01, 0x01]).unwrap();
        assert_eq!(wear_frame(0x01, &[0x00, 0x01, 0x01]).unwrap(), commands::set_wearing_detection(true, &w));
        // Bad value or bad read: refused
        for v in [0x00, 0x03, 0xFF] {
            assert!(wear_frame(v, &[0x01, 0x01, 0x01]).is_err(), "{v}");
        }
        assert!(wear_frame(0x01, &[]).is_err());
        assert!(wear_frame(0x01, &[0x01, 0x01]).is_err());
        assert!(wear_frame(0x01, &[0x01, 0x01, 0x01, 0x01, 0x01]).is_err());
        assert!(wear_frame(0x01, &[0x01, 0x10, 0x01]).is_err(), "unknown action byte");
        assert!(wear_frame(0x01, &[0x01, 0x01, 0xFF]).is_err(), "unknown action byte");
        // No frame without the read
        assert!(frame_for(Setting::WearDetection, 0x01).is_err());
        assert_eq!(check_value(Setting::WearDetection, 0x01), Ok(()));
        assert_eq!(check_value(Setting::WearDetection, 0x02), Ok(()));
        assert!(check_value(Setting::WearDetection, 0x00).is_err());
        assert_eq!(Setting::from_kind("wear_detection"), Some(Setting::WearDetection));
        assert!(Setting::WearDetection.is_toggle());
    }

    #[test]
    fn wear_build_takes_only_well_formed_parameters() {
        assert!(build(0x2C, &[0x01, 0x01, 0x01]).is_ok());
        assert!(build(0x2C, &[0x02, 0x01, 0x01, 0x02]).is_ok());
        assert!(build(0x2C, &[0x00, 0x01, 0x01]).is_err(), "00 is never written");
        assert!(build(0x2C, &[0x03, 0x01, 0x01]).is_err());
        assert!(build(0x2C, &[0x01, 0x01, 0x01, 0x00]).is_err());
        assert!(build(0x2C, &[0x01, 0x01]).is_err());
        assert!(build(0x2C, &[0x01, 0x01, 0x01, 0x01, 0x01]).is_err());
        assert!(build(0x2C, &[]).is_err());
        // The blocklist still wins for every opcode, whatever the parameters
        for op in [0x01u8, 0x02, 0x03, 0x0D, 0x11, 0x18, 0x1F, 0x45, 0xFE] {
            assert_eq!(build(op, &[0x01, 0x01, 0x01]), Err(Refused::Blocked(op)));
        }
        assert!(check_target(0xFE2C, 0x1001).is_err());
        assert!(check_target(0x7033, 0x2001).is_err());
        assert!(check_target(0x7033, 0x2002).is_err());
    }

    #[test]
    fn wear_confirmation() {
        // Off reads back as 00 (or 02): confirmed
        assert!(wear_confirms(&[0x02, 0x01, 0x01], &[0x00, 0x01, 0x01]));
        assert!(wear_confirms(&[0x02, 0x01, 0x01], &[0x02, 0x01, 0x01]));
        assert!(wear_confirms(&[0x01, 0x01, 0x01], &[0x01, 0x01, 0x01]));
        // Wrong state or changed actions: not confirmed
        assert!(!wear_confirms(&[0x01, 0x01, 0x01], &[0x00, 0x01, 0x01]));
        assert!(!wear_confirms(&[0x02, 0x01, 0x01], &[0x01, 0x01, 0x01]));
        assert!(!wear_confirms(&[0x01, 0x01, 0x01], &[0x01, 0x02, 0x01]));
        assert!(!wear_confirms(&[0x01, 0x01, 0x01], &[0x01, 0x01]));
        assert!(!wear_confirms(&[0x01, 0x01, 0x01], &[]));
        // Tone byte
        assert!(wear_confirms(&[0x01, 0x02, 0x03, 0x02], &[0x01, 0x02, 0x03, 0x00]));
        assert!(!wear_confirms(&[0x01, 0x02, 0x03, 0x01], &[0x01, 0x02, 0x03, 0x00]));
        assert!(!wear_confirms(&[0x01, 0x02, 0x03, 0x01], &[0x01, 0x02, 0x03]));
        assert!(confirms_for(Setting::WearDetection, &[0x02, 0x01, 0x01], &[0x00, 0x01, 0x01]));
        assert_eq!(read_value(Setting::WearDetection, &[0x00, 0x01, 0x01]), Some(0x02));
        assert_eq!(read_value(Setting::WearDetection, &[0x01, 0x01, 0x01, 0x01]), Some(0x01));
        assert_eq!(read_value(Setting::WearDetection, &[0x01]), None, "a short read is unknown");
        assert_eq!(read_value(Setting::WearDetection, &[0x07, 0x01, 0x01]), None);
    }

    /// Earbuds with a 0x2C state
    struct WearMock {
        state: Vec<u8>,
        applies: bool,
        answers_after: bool,
        /// Changes the music action on a write (a firmware that rewrites it)
        mangles: bool,
        written: bool,
        sent: Vec<Vec<u8>>,
        inbox: VecDeque<Vec<u8>>,
    }

    impl WearMock {
        fn new(state: &[u8]) -> WearMock {
            WearMock {
                state: state.to_vec(),
                applies: true,
                answers_after: true,
                mangles: false,
                written: false,
                sent: Vec::new(),
                inbox: VecDeque::new(),
            }
        }
        fn writes(&self) -> Vec<&Vec<u8>> {
            self.sent.iter().filter(|f| f[2] != 0xFE).collect()
        }
    }

    impl Transport for WearMock {
        fn send(&mut self, frame: &[u8]) -> Result<(), String> {
            self.sent.push(frame.to_vec());
            if frame[2] == 0xFE {
                if self.written && !self.answers_after {
                    return Ok(());
                }
                if frame[4] == 0x2C {
                    self.inbox.push_back(packet::frame(0x2C, &self.state));
                }
            } else {
                self.written = true;
                if self.applies && frame[2] == 0x2C {
                    let mut p = frame[4..].to_vec();
                    // The N70 reads "off" back as 00
                    if p[0] == 0x02 {
                        p[0] = 0x00;
                    }
                    if self.mangles {
                        p[1] = p[1].wrapping_add(1);
                    }
                    self.state = p;
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

    #[test]
    fn wear_off_is_confirmed_by_a_00_read_back_and_undo_restores() {
        let mut m = WearMock::new(&[0x01, 0x01, 0x01]);
        let o = run_toggle(&mut m, Setting::WearDetection, false, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(o.previous, Some(0x01));
        assert_eq!(m.sent[0], hex("FF03FE012C"), "read first");
        assert_eq!(m.writes(), [&hex("FF052C03020101")], "exactly one write");
        assert_eq!(o.read_back, Some(vec![0x00, 0x01, 0x01]));
        assert_eq!(o.steps.iter().map(|s| s.what).collect::<Vec<_>>(), ["read_previous", "sent", "read_back"]);
        // Desfazer: the saved value, the same flow
        let undo = run(&mut m, Setting::WearDetection, o.previous.unwrap(), T);
        assert!(undo.confirmed());
        assert_eq!(m.writes(), [&hex("FF052C03020101"), &hex("FF052C03010101")]);
    }

    #[test]
    fn wear_with_a_tone_byte_keeps_it() {
        let mut m = WearMock::new(&[0x00, 0x02, 0x03, 0x01]);
        let o = run_toggle(&mut m, Setting::WearDetection, true, T);
        assert!(o.confirmed(), "{o:?}");
        assert_eq!(m.writes(), [&hex("FF062C0401020301")]);
    }

    #[test]
    fn a_silent_wear_write_times_out_once() {
        let mut m = WearMock::new(&[0x01, 0x01, 0x01]);
        m.answers_after = false;
        let started = Instant::now();
        let o = run_toggle(&mut m, Setting::WearDetection, false, T);
        assert_eq!(o.failure, Some(Failure::Timeout));
        assert!(started.elapsed() >= T);
        assert_eq!(m.writes().len(), 1, "never re-sent");
        assert_eq!(o.previous, Some(0x01));
    }

    #[test]
    fn a_wear_read_back_with_other_bytes_fails() {
        let mut m = WearMock::new(&[0x01, 0x01, 0x01]);
        m.applies = false;
        let o = run_toggle(&mut m, Setting::WearDetection, false, T);
        assert_eq!(o.failure, Some(Failure::Mismatch));
        let mut m = WearMock::new(&[0x00, 0x01, 0x01]);
        m.mangles = true;
        let o = run_toggle(&mut m, Setting::WearDetection, true, T);
        assert_eq!(o.failure, Some(Failure::Mismatch), "music action changed");
        assert_eq!(m.writes().len(), 1);
    }

    #[test]
    fn an_unknown_wear_state_is_never_overwritten() {
        for state in [&[0x01u8][..], &[0x07, 0x01, 0x01], &[0x01, 0x20, 0x01], &[0x01, 0x01, 0x01, 0x01, 0x01]] {
            let mut m = WearMock::new(state);
            let o = run_toggle(&mut m, Setting::WearDetection, true, T);
            assert_eq!(o.failure, Some(Failure::NoPrevious), "{state:?}");
            assert!(m.writes().is_empty(), "{state:?}");
        }
        let mut m = WearMock::new(&[0x01, 0x01, 0x01]);
        let o = run(&mut m, Setting::WearDetection, 0x00, T);
        assert_eq!(o.failure, Some(Failure::OutOfRange));
        assert!(m.sent.is_empty(), "a bad value sends nothing, not even a read");
    }
}
