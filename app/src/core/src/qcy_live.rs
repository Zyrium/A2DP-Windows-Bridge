//! The Fones screen's QCY reads (MeloBuds N70): `qcy.state`, `qcy.refresh`,
//! `qcy.reconnect` and the `qcy` event, plus ONE write per click: `qcy.set`
//! for game mode, sleep mode or wind reduction (phase F2; the prompt
//! volume is read-only since fase 2b: the N70 did not take 0x1D), noise
//! control (F3), wear detection (F3b) and, "Em teste", the ten-band
//! equalizer (F4, opcode 0x22, [`write::run_eq`]). The reads only send the `FF 03 FE <op>` state requests
//! of [`commands::STATE_QUERIES`] (opcode 0xFE). The write goes through
//! [`qcy_protocol::write`] (allowlist: 0x09 / 0x10 / 0x2A with 01/02;
//! every other change is refused before any Bluetooth traffic) and only on an
//! explicit qcy.set: never when the app opens or the link comes up.
//! `le_connect_plain` never pairs.
//!
//! Audio: the reads use the resident core's own BTstack (the one that
//! streams A2DP), so no second process takes the adapter and the stack is
//! never restarted here. That needs LE in the running stack (LE Audio
//! (experimental) on): when it is off and audio streams, turning LE on
//! would restart BTstack and drop the audio, so the read is refused
//! (`le_off`) instead. Nothing polls: a read happens only on an explicit
//! qcy.refresh / qcy.reconnect (the "Atualizar" / "Tentar de novo" buttons).
//!
//! Airtime: on a single radio adapter every LE packet is time taken from
//! the A2DP stream (LDAC 990 kbps has little slack), so a read keeps the LE
//! side short: it connects straight to the earbuds' LE address of the last
//! read (no scan), reuses that link's GATT handles (no discovery), asks
//! for a long connection interval with short events, sends the 13 queries
//! back to back and disconnects as soon as they are answered. Only the
//! first read (or one after a failure) scans, with a 25 % window, and stops
//! when the earbuds are seen. Where the last read found them is kept in
//! qcy-cache.json ([`crate::qcy_cache`]), so this holds after a restart too.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use a2dpwb_core::Service;
use bt_host::{BtHost, GattChar, GattService, LeAddr, LeScanResult};
use qcy_protocol::advertisement::{self, Advertisement};
use qcy_protocol::commands::{self, NoiseMode, NoiseScene};
use qcy_protocol::packet;
use qcy_protocol::state::QcyState;
use qcy_protocol::uuids::{self, uuid16_matches};
use qcy_protocol::write::{self, Setting};
use serde_json::{Value, json};

/// Longest scan (it stops as soon as the earbuds are seen)
const SCAN: Duration = Duration::from_secs(4);
/// Scan window 15 ms of every 60 ms (0.625 ms units): 25 % of the radio
/// instead of the 100 % of a plain scan
const SCAN_INTERVAL: u16 = 0x0060;
const SCAN_WINDOW: u16 = 0x0018;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A direct connect to the cached address: the earbuds advertise often,
/// so a longer wait means they are gone or changed address (then scan)
const DIRECT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const GATT_TIMEOUT: Duration = Duration::from_secs(5);
/// After the last query is sent: how long the answers may take in total
const ANSWERS_TIMEOUT: Duration = Duration::from_millis(1500);
/// A write: the first read, then write -> read-back, each at most this
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// Touch actions in the protocol's order (qcy.ts TOUCH_ACTIONS)
const TOUCH_ACTIONS: [&str; 9] = [
    "none",
    "play_pause",
    "previous",
    "next",
    "voice_assistant",
    "volume_up",
    "volume_down",
    "game_mode",
    "noise_control",
];
/// (key id, side, gesture): double tap 0x03/0x04, long press 0x09/0x0A
const GESTURE_KEYS: [(u8, &str, &str); 4] = [
    (0x03, "left", "double_tap"),
    (0x04, "right", "double_tap"),
    (0x09, "left", "long_press"),
    (0x0A, "right", "long_press"),
];

/// Why no read happened (QcyUnavailableReason of qcy.ts)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// LE is not on in the running stack; turning it on would restart it
    LeOff,
    /// A device scan runs
    AdapterBusy,
    NotFound,
    GattFailed,
    Timeout,
    Other,
}

impl Unavailable {
    fn reason(self) -> &'static str {
        match self {
            Unavailable::LeOff => "le_off",
            Unavailable::AdapterBusy => "adapter_busy",
            Unavailable::NotFound => "not_found",
            Unavailable::GattFailed => "gatt_failed",
            Unavailable::Timeout => "timeout",
            Unavailable::Other => "other",
        }
    }
}

/// One successful read
#[derive(Debug, Clone)]
pub struct Reading {
    pub state: QcyState,
    pub name: String,
    pub address: String,
    pub answered: usize,
    pub at_ms: u64,
}

/// Where the last read found the earbuds: their LE address and that
/// link's GATT cache (a read connects straight there, no scan, no
/// discovery)
#[derive(Clone)]
struct Known {
    addr: LeAddr,
    name: String,
    address: String,
    services: Vec<GattService>,
}

/// The last write (the banner of the Fones screen)
#[derive(Debug, Clone)]
pub struct LastWrite {
    pub setting: &'static str,
    /// On/off setting (value 01/02) rather than a level
    pub toggle: bool,
    /// The write's parameter byte: 01/02 for a toggle, the level otherwise
    pub value: u8,
    /// Same encoding, read before the write
    pub previous: Option<u8>,
    /// The maximum the earbuds reported (prompt volume)
    pub max: Option<u8>,
    pub sent: Option<String>,
    pub read_back: Option<String>,
    /// None: confirmed by the read-back
    pub failure: Option<&'static str>,
    pub steps: Vec<(String, String)>,
    pub at_ms: u64,
    /// Phase F4: the equalizer write's curves
    pub eq: Option<EqWrite>,
}

/// An equalizer write (1/100 dB)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqWrite {
    pub curve: write::EqCurve,
    /// Preset byte and curve read before the write (None: not read)
    pub previous: Option<(u8, write::EqCurve)>,
}

/// The curve read before the FIRST equalizer write ("Restaurar curva
/// original"): preset byte, curve, Unix ms. Kept in qcy-cache.json too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqOriginal {
    pub preset: u8,
    pub curve: write::EqCurve,
    pub saved_at: u64,
}

/// The qcy-cache.json key of [`EqOriginal`]
const EQ_ORIGINAL_KEY: &str = "equalizer_original";

static EQ_ORIGINAL: Mutex<Option<EqOriginal>> = Mutex::new(None);

impl EqOriginal {
    fn to_json(&self) -> Value {
        json!({ "preset": self.preset, "gains": db(&self.curve), "saved_at": self.saved_at })
    }
    fn from_json(v: &Value) -> Option<EqOriginal> {
        let gains: Vec<f64> = v.get("gains")?.as_array()?.iter().map(Value::as_f64).collect::<Option<_>>()?;
        Some(EqOriginal {
            preset: u8::try_from(v.get("preset")?.as_u64()?).ok()?,
            curve: write::eq_curve_from_db(&gains)?,
            saved_at: v.get("saved_at")?.as_u64()?,
        })
    }
}

/// 1/100 dB -> dB
fn db(c: &write::EqCurve) -> Vec<f64> {
    c.iter().map(|g| f64::from(*g) / 100.0).collect()
}

#[derive(Default)]
struct Cache {
    last: Option<Reading>,
    link: Option<Unavailable>,
    write: Option<LastWrite>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { last: None, link: None, write: None });
static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
/// qcy-cache.json was read into KNOWN (once per process)
static LOADED: AtomicBool = AtomicBool::new(false);
/// One read at a time
static READING: Mutex<()> = Mutex::new(());

/// The last known state (no Bluetooth traffic)
pub fn current() -> Value {
    let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    view_all(c.last.as_ref(), c.link, c.write.as_ref())
}

/// Reads the earbuds now (blocking: run off the async runtime)
pub fn refresh(service: &Service) -> Value {
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    let result = host_for_read(service).and_then(|h| read(&h, service.config_dir()));
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(r) => {
            tracing::info!("QCY: read {} of {} queries from {}", r.answered, commands::STATE_QUERIES.len(), r.address);
            // The 0x17 parameter bytes as read (mode, sub-scene, value), so
            // a "Desligado" on screen can be told apart from a decoding bug
            match r.state.noise_control {
                Some(n) => tracing::info!("QCY: 0x17 = {n} ({:?})", n.mode),
                None => tracing::info!("QCY: 0x17 not read (noise mode unknown)"),
            }
            c.last = Some(keep_known(c.last.as_ref(), r));
            c.link = None;
        }
        Err(why) => {
            tracing::warn!("QCY: no read ({})", why.reason());
            c.link = Some(why);
        }
    }
    view_all(c.last.as_ref(), c.link, c.write.as_ref())
}

/// A read that missed 0x17 keeps the last good noise control of the same
/// earbuds (the screen showed "Desligado" when ANC was simply unanswered)
fn keep_known(previous: Option<&Reading>, mut r: Reading) -> Reading {
    if r.state.noise_control.is_none()
        && let Some(p) = previous.filter(|p| p.address == r.address)
    {
        r.state.noise_control = p.state.noise_control;
    }
    r
}

/// A change the UI asked for (`qcy.set`) that this version accepts, as the
/// setting and its write parameter byte; None (nothing sent) for anything
/// else. Only `{ "kind": "game_mode" | "sleep_mode" | "wind_reduction",
/// "enabled": bool }` and, since phase F3, `{ "kind": "noise_control",
/// "value": 0..=6 }` (a code of `write::NOISE_PRESETS`) pass (the prompt
/// volume is read-only). Since phase F3b, `{ "kind": "wear_detection",
/// "enabled": bool }` too (the music / ANC bytes are copied from the read
/// made right before the write, never from the UI).
pub fn allowed_change(change: &Value) -> Option<(Setting, u8)> {
    let setting = Setting::from_kind(change.get("kind")?.as_str()?)?;
    // Exactly kind + one value: no extra parameters ride along
    if change.as_object()?.len() != 2 {
        return None;
    }
    let value = match setting {
        s if s.is_toggle() => write::on_off(change.get("enabled")?.as_bool()?),
        Setting::NoiseControl => u8::try_from(change.get("value")?.as_u64()?).ok()?,
        _ => return None,
    };
    // The same check the frame builder makes
    write::check_value(setting, value).ok()?;
    Some((setting, value))
}

/// Changes one allowed setting (blocking: run off the async runtime). Err
/// only when the change is refused (nothing sent); a write that is not
/// confirmed is Ok with `write.status = "failed"` in the state.
pub fn set(service: &Service, change: &Value) -> Result<Value, ()> {
    if change.get("kind").and_then(Value::as_str) == Some("equalizer") {
        let Some(curve) = allowed_eq(change) else {
            tracing::warn!("QCY write: refused (equalizer curve not allowed): {change}");
            return Err(());
        };
        return Ok(set_eq(service, &curve));
    }
    let Some((setting, value)) = allowed_change(change) else {
        tracing::warn!("QCY write: refused (not allowed in this version): {change}");
        return Err(());
    };
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    tracing::info!(
        "QCY write: {} -> {} (one command, confirmed by a read-back)",
        setting.key(),
        match setting {
            Setting::NoiseControl => write::noise_name(value).to_string(),
            s if s.is_toggle() => (if value == 0x01 { "on" } else { "off" }).to_string(),
            _ => value.to_string(),
        }
    );
    let outcome = host_for_read(service).and_then(|h| write_at(&h, service.config_dir(), setting, value));
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let w = match outcome {
        Ok(o) => {
            if let (Some(r), Some(p)) = (c.last.as_mut(), o.read_back.as_deref()) {
                let on = write::read_on(p);
                match setting {
                    Setting::GameMode => r.state.game_mode = on.or(r.state.game_mode),
                    Setting::SleepMode => r.state.sleep_mode = on.or(r.state.sleep_mode),
                    Setting::WindReduction => r.state.wind_detection = on.or(r.state.wind_detection),
                    Setting::NoiseControl => {
                        r.state.noise_control =
                            qcy_protocol::commands::NoiseControlState::parse(p).or(r.state.noise_control);
                    }
                    Setting::WearDetection => {
                        if let Some(w) = qcy_protocol::commands::WearingDetection::parse(p) {
                            r.state.wearing_detection = Some(w);
                            r.state.wear_detection_enabled = Some(w.enabled);
                        }
                    }
                    // Written only by set_eq
                    Setting::Equalizer => {}
                    Setting::PromptVolume => {
                        if let Some(v) = p.first() {
                            r.state.prompt_volume = Some(*v);
                        }
                        if let Some(m) = write::read_max(p) {
                            r.state.prompt_volume_max = m;
                        }
                    }
                }
                // 0x2A read again after the noise change (never written)
                if let Some(w) = o.wind_after.as_deref() {
                    r.state.wind_detection = write::read_on(w).or(r.state.wind_detection);
                }
                r.at_ms = now_ms();
            }
            c.link = None;
            last_write(&o)
        }
        Err(why) => {
            tracing::warn!("QCY write: no link ({}), nothing sent", why.reason());
            LastWrite {
                setting: setting.key(),
                toggle: setting.is_toggle(),
                value,
                previous: None,
                max: None,
                sent: None,
                read_back: None,
                failure: Some(why.reason()),
                steps: vec![("no_link".into(), why.reason().into())],
                at_ms: now_ms(),
                eq: None,
            }
        }
    };
    match w.failure {
        None => tracing::info!("QCY write: confirmed by the earbuds ({})", w.read_back.as_deref().unwrap_or("")),
        Some(f) => tracing::warn!("QCY write: NOT confirmed ({f}); the control goes back, nothing is re-sent"),
    }
    c.write = Some(w);
    Ok(view_all(c.last.as_ref(), c.link, c.write.as_ref()))
}

fn last_write(o: &write::Outcome) -> LastWrite {
    LastWrite {
        setting: o.setting.key(),
        toggle: o.setting.is_toggle(),
        value: o.value,
        previous: o.previous,
        max: o.max,
        sent: o.sent.as_deref().map(packet::to_hex),
        read_back: o.read_back.as_deref().map(packet::to_hex),
        failure: o.failure.map(write::Failure::code),
        steps: o.steps.iter().map(|s| (s.what.to_string(), s.detail.clone())).collect(),
        at_ms: now_ms(),
        eq: None,
    }
}

/// Phase F4: the equalizer change the UI may ask for, exactly
/// `{ "kind": "equalizer", "gains": [10 numbers] }` in dB, each a whole
/// number of hundredths within ±8 dB (refused, never clamped). Preset,
/// master gain, Q and type are not the UI's: always 8 / 0 / 1.00 / 0.
pub fn allowed_eq(change: &Value) -> Option<write::EqCurve> {
    let c = change.as_object()?;
    if c.len() != 2 || c.get("kind")?.as_str()? != "equalizer" {
        return None;
    }
    let gains: Vec<f64> = c.get("gains")?.as_array()?.iter().map(Value::as_f64).collect::<Option<_>>()?;
    let curve = write::eq_curve_from_db(&gains)?;
    // The same check the frame builder makes
    write::eq_frame(&curve).ok()?;
    Some(curve)
}

/// The equalizer write (blocking): read + keep the curve, ONE 0x22 frame,
/// read back the ten bands. Never re-sent; a failure shows the curve read.
fn set_eq(service: &Service, curve: &write::EqCurve) -> Value {
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    tracing::info!("QCY write: equalizer -> [{}] (one command, confirmed by a read-back)", write::eq_text(curve));
    let dir = service.config_dir();
    let outcome = host_for_read(service).and_then(|h| link_at(&h, dir, |l| write::run_eq(l, curve, WRITE_TIMEOUT)));
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let w = match outcome {
        Ok((o, classic)) => {
            for s in &o.steps {
                tracing::info!("QCY write step: {} {}", s.what, s.detail);
            }
            if let Some((preset, prev)) = o.previous {
                let saved = json!({ "preset": preset, "gains": db(&prev) });
                crate::qcy_cache::save_previous(dir, &classic, Setting::Equalizer.key(), saved);
                keep_original(dir, &classic, preset, &prev);
            }
            if let Some(r) = c.last.as_mut() {
                // The screen shows what the earbuds hold: the read-back when
                // it parses, else the curve read before the write
                let now = o.read_back.as_deref().and_then(write::eq_read).or(o.previous);
                if let Some((preset, gains)) = now {
                    set_gains(&mut r.state, preset, &gains);
                    r.at_ms = now_ms();
                }
            }
            c.link = None;
            LastWrite {
                setting: Setting::Equalizer.key(),
                toggle: false,
                value: 0,
                previous: None,
                max: None,
                sent: o.sent.as_deref().map(packet::to_hex),
                read_back: o.read_back.as_deref().map(packet::to_hex),
                failure: o.failure.map(write::Failure::code),
                steps: o.steps.iter().map(|s| (s.what.to_string(), s.detail.clone())).collect(),
                at_ms: now_ms(),
                eq: Some(EqWrite { curve: o.curve, previous: o.previous }),
            }
        }
        Err(why) => {
            tracing::warn!("QCY write: no link ({}), nothing sent", why.reason());
            LastWrite {
                setting: Setting::Equalizer.key(),
                toggle: false,
                value: 0,
                previous: None,
                max: None,
                sent: None,
                read_back: None,
                failure: Some(why.reason()),
                steps: vec![("no_link".into(), why.reason().into())],
                at_ms: now_ms(),
                eq: Some(EqWrite { curve: *curve, previous: None }),
            }
        }
    };
    match w.failure {
        None => tracing::info!("QCY write: equalizer confirmed by the earbuds"),
        Some(f) => tracing::warn!("QCY write: equalizer NOT confirmed ({f}); the screen shows the curve read, nothing is re-sent"),
    }
    c.write = Some(w);
    view_all(c.last.as_ref(), c.link, c.write.as_ref())
}

/// The first ten gains of a state become `gains` (the slots after the tenth
/// stay as read)
fn set_gains(s: &mut QcyState, preset: u8, gains: &write::EqCurve) {
    s.equalizer_preset = Some(preset);
    if s.equalizer_gains.len() < 10 {
        s.equalizer_gains = vec![0.0; 10];
    }
    for (slot, g) in s.equalizer_gains.iter_mut().zip(db(gains)) {
        *slot = g;
    }
}

/// Keeps the curve read before the first equalizer write, once
fn keep_original(dir: &Path, classic: &str, preset: u8, curve: &write::EqCurve) {
    let mut o = EQ_ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
    if o.is_some() {
        return;
    }
    let orig = EqOriginal { preset, curve: *curve, saved_at: now_ms() };
    crate::qcy_cache::save_previous(dir, classic, EQ_ORIGINAL_KEY, orig.to_json());
    tracing::info!("QCY: original equalizer kept (preset {preset} [{}])", write::eq_text(curve));
    *o = Some(orig);
}

/// The BTstack link as the write flow's transport: frames to 1001,
/// notifications of 1002
struct GattLink<'a> {
    host: &'a BtHost,
    id: bt_host::LeDeviceId,
    command: GattChar,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl write::Transport for GattLink<'_> {
    fn send(&mut self, frame: &[u8]) -> Result<(), String> {
        let f = frame.to_vec();
        if self.command.properties & 0x04 != 0 {
            self.host.le_gatt_write_without_response(self.id, self.command.value_handle, f, GATT_TIMEOUT)
        } else {
            self.host.le_gatt_write(self.id, self.command.value_handle, f, GATT_TIMEOUT)
        }
    }
    fn recv(&mut self, wait: Duration) -> Option<Vec<u8>> {
        if wait.is_zero() { self.rx.try_recv().ok() } else { self.rx.recv_timeout(wait).ok() }
    }
}

/// Connects straight to where the last read found the earbuds (no scan:
/// a write needs a read first), runs the write flow, disconnects
fn write_at(host: &BtHost, dir: &Path, setting: Setting, value: u8) -> Result<write::Outcome, Unavailable> {
    let (o, classic) = link_at(host, dir, |l| write::run(l, setting, value, WRITE_TIMEOUT))?;
    for s in &o.steps {
        tracing::info!("QCY write step: {} {}", s.what, s.detail);
    }
    if let Some(p) = o.previous {
        let saved = if setting.is_toggle() { json!(p == 0x01) } else { json!(p) };
        crate::qcy_cache::save_previous(dir, &classic, setting.key(), saved);
    }
    Ok(o)
}

/// Connects straight to the known earbuds, runs `f` on the command link,
/// disconnects; returns its result and their classic address
fn link_at<R>(host: &BtHost, dir: &Path, f: impl FnOnce(&mut GattLink) -> R) -> Result<(R, String), Unavailable> {
    load_known(dir);
    let Some(k) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        // Never scan for a write: "Atualizar" finds them first
        return Err(Unavailable::NotFound);
    };
    let started = Instant::now();
    let id =
        host.le_connect_plain_cached(k.addr, &k.name, DIRECT_CONNECT_TIMEOUT, k.services.clone()).map_err(|e| {
            tracing::warn!("QCY write: connection failed: {e}");
            Unavailable::Timeout
        })?;
    let services = host.le_gatt_services(id);
    let r = write_on(host, id, &services, f);
    host.le_disconnect(id);
    tracing::info!("QCY write: link up {} ms in total", started.elapsed().as_millis());
    Ok((r?, k.address))
}

fn write_on<R>(
    host: &BtHost,
    id: bt_host::LeDeviceId,
    services: &[GattService],
    f: impl FnOnce(&mut GattLink) -> R,
) -> Result<R, Unavailable> {
    let command = find_char(services, uuids::MAIN_SERVICE, uuids::COMMAND).ok_or(Unavailable::GattFailed)?;
    let notification = find_char(services, uuids::MAIN_SERVICE, uuids::NOTIFICATION).ok_or(Unavailable::GattFailed)?;
    // Only A001 / 1001, never the OTA services
    write::check_target(uuids::MAIN_SERVICE, uuids::COMMAND).map_err(|_| Unavailable::Other)?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    host.le_gatt_subscribe(id, notification, tx, GATT_TIMEOUT).map_err(|_| Unavailable::GattFailed)?;
    let mut link = GattLink { host, id, command, rx };
    Ok(f(&mut link))
}

/// The stack to read with, never restarting one that streams
fn host_for_read(service: &Service) -> Result<std::sync::Arc<BtHost>, Unavailable> {
    if service.is_scanning() {
        return Err(Unavailable::AdapterBusy);
    }
    let host = match service.host() {
        Some(h) => h,
        // Nothing runs, so nothing can be cut: start the stack
        None if !service.is_running() => service.ensure_host().map_err(|_| Unavailable::Other)?,
        None => return Err(Unavailable::Other),
    };
    let until = Instant::now() + Duration::from_secs(2);
    while !host.le_ready() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    if host.le_ready() { Ok(host) } else { Err(Unavailable::LeOff) }
}

/// Fills KNOWN from qcy-cache.json the first time (no Bluetooth traffic)
fn load_known(dir: &Path) {
    if LOADED.swap(true, Ordering::SeqCst) {
        return;
    }
    let file = crate::qcy_cache::load(dir);
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if known.is_none()
        && let Some((classic, addr, services, e)) = crate::qcy_cache::latest(&file)
    {
        tracing::info!("QCY: {} from {} (LE {}, {} services)", classic, crate::qcy_cache::FILE, addr, services.len());
        if let Some(o) = e.previous.get(EQ_ORIGINAL_KEY).and_then(EqOriginal::from_json) {
            let mut slot = EQ_ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
            slot.get_or_insert(o);
        }
        *known = Some(Known { addr, name: e.name.clone(), address: classic, services });
    }
}

fn read(host: &BtHost, dir: &Path) -> Result<Reading, Unavailable> {
    load_known(dir);
    let known = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(k) = known {
        match read_at(host, &k, None) {
            Ok(r) => {
                remember(dir, &k.address, &r);
                return Ok(r);
            }
            Err(why) => {
                // Moved, changed address or firmware: forget it (memory and
                // qcy-cache.json), find it again
                tracing::info!("QCY: the known LE address failed ({}), scanning", why.reason());
                *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
                crate::qcy_cache::invalidate(dir, &k.address);
            }
        }
    }
    let found: Vec<(LeScanResult, Advertisement)> = host
        .le_scan_until(SCAN, SCAN_INTERVAL, SCAN_WINDOW, |seen| seen.iter().any(|r| n70_in(r).is_some()))
        .into_iter()
        .filter_map(|r| n70_in(&r).map(|a| (r, a)))
        .collect();
    let Some((target, adv)) = found.into_iter().next() else {
        return Err(Unavailable::NotFound);
    };
    let name = if target.name.is_empty() { "QCY MeloBuds N70".to_string() } else { target.name.clone() };
    let address = adv.control_address.map_or_else(|| target.addr.to_string(), advertisement::format_address);
    let k = Known { addr: target.addr, name, address, services: Vec::new() };
    let r = read_at(host, &k, Some(&adv))?;
    remember(dir, &k.address, &r);
    Ok(r)
}

/// Writes where `r` was read (KNOWN, set by read_at) to qcy-cache.json
fn remember(dir: &Path, classic: &str, r: &Reading) {
    let Some(k) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    if k.services.is_empty() {
        return;
    }
    let e = crate::qcy_cache::entry(&k.name, k.addr, &k.services, r.state.firmware_version.clone(), now_ms());
    crate::qcy_cache::remember(dir, classic, e);
}

/// The N70 advertisement of a scan result
fn n70_in(r: &LeScanResult) -> Option<Advertisement> {
    advertisement::find_in_ad(&r.adv)
        .or_else(|| advertisement::find_in_ad(&r.scan_rsp))
        .filter(|a| uuids::is_n70(a.vendor_id))
}

/// Connects to `k` (with its cached GATT handles when it has them), reads,
/// disconnects at once; remembers where it worked
fn read_at(host: &BtHost, k: &Known, adv: Option<&Advertisement>) -> Result<Reading, Unavailable> {
    let timeout = if adv.is_none() { DIRECT_CONNECT_TIMEOUT } else { CONNECT_TIMEOUT };
    let started = Instant::now();
    let id = host.le_connect_plain_cached(k.addr, &k.name, timeout, k.services.clone()).map_err(|e| {
        tracing::warn!("QCY: connection failed: {e}");
        if e.to_lowercase().contains("timeout") || e.to_lowercase().contains("timed out") {
            Unavailable::Timeout
        } else {
            Unavailable::GattFailed
        }
    })?;
    let connected = started.elapsed();
    let services = host.le_gatt_services(id);
    let r = query(host, id, &services);
    host.le_disconnect(id);
    tracing::info!(
        "QCY: connected in {} ms, link up {} ms in total{}",
        connected.as_millis(),
        started.elapsed().as_millis(),
        if k.services.is_empty() { "" } else { " (cached GATT handles)" }
    );
    let (mut state, answered) = r?;
    if state.battery.left.is_none()
        && let Some(adv) = adv
    {
        // The advertisement's battery when the characteristic said nothing
        state.battery.left = Some(adv.left_battery);
        state.battery.right = Some(adv.right_battery);
        state.battery.case = (adv.case_battery > 0).then_some(adv.case_battery);
    }
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Known { services, ..k.clone() });
    Ok(Reading { state, name: k.name.clone(), address: k.address.clone(), answered, at_ms: now_ms() })
}

fn query(host: &BtHost, id: bt_host::LeDeviceId, services: &[GattService]) -> Result<(QcyState, usize), Unavailable> {
    let command = find_char(services, uuids::MAIN_SERVICE, uuids::COMMAND).ok_or(Unavailable::GattFailed)?;
    let notification = find_char(services, uuids::MAIN_SERVICE, uuids::NOTIFICATION).ok_or(Unavailable::GattFailed)?;
    let mut state = QcyState::default();
    let (tx, responses) = mpsc::channel::<Vec<u8>>();
    host.le_gatt_subscribe(id, notification, tx, GATT_TIMEOUT).map_err(|_| Unavailable::GattFailed)?;

    for uuid in [uuids::FIRMWARE_VERSION, uuids::BATTERY, uuids::KEY_FUNCTIONS] {
        let Some(chr) = find_char(services, uuids::MAIN_SERVICE, uuid) else { continue };
        if chr.properties & 0x02 == 0 {
            continue;
        }
        if let Ok(value) = host.le_gatt_read(id, chr.value_handle, GATT_TIMEOUT) {
            match uuid {
                uuids::FIRMWARE_VERSION => state.apply_firmware_characteristic(&value),
                uuids::BATTERY => state.apply_battery_characteristic(&value),
                _ => state.apply_key_functions_characteristic(&value),
            };
        }
    }

    // All queries back to back, then the answers: the link is busy for a
    // few connection events instead of one wait per query
    let mut pending: Vec<u8> = Vec::new();
    for op in commands::STATE_QUERIES {
        // Read-only requests only (FF 03 FE <op>)
        let frame = commands::request(op);
        debug_assert_eq!(frame[2], packet::REQUEST_OPCODE);
        let written = if command.properties & 0x04 != 0 {
            host.le_gatt_write_without_response(id, command.value_handle, frame, GATT_TIMEOUT)
        } else {
            host.le_gatt_write(id, command.value_handle, frame, GATT_TIMEOUT)
        };
        if written.is_ok() {
            pending.push(op);
        }
        while let Ok(v) = responses.try_recv() {
            answer(&v, &mut state, &mut pending);
        }
    }
    let sent = commands::STATE_QUERIES.len();
    let mut answered = collect(&responses, &mut state, &mut pending, ANSWERS_TIMEOUT);
    answered = answered.min(sent);
    if !pending.is_empty() {
        let ops: Vec<String> = pending.iter().map(|op| format!("0x{op:02X}")).collect();
        tracing::info!("QCY: no answer to {}", ops.join(" "));
    }
    if answered == 0 && state.battery.left.is_none() && state.firmware_version.is_none() {
        return Err(Unavailable::Timeout);
    }
    Ok((state, answered))
}

/// Waits for the answers to `pending` (until all came or `timeout`); the
/// number of queries answered in total
fn collect(
    responses: &mpsc::Receiver<Vec<u8>>,
    state: &mut QcyState,
    pending: &mut Vec<u8>,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    while !pending.is_empty() {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match responses.recv_timeout(left) {
            Ok(v) => answer(&v, state, pending),
            Err(_) => break,
        }
    }
    commands::STATE_QUERIES.len() - pending.len()
}

/// Applies a notification; the queries it answers leave `pending`
fn answer(value: &[u8], state: &mut QcyState, pending: &mut Vec<u8>) {
    for op in apply(value, state) {
        pending.retain(|p| *p != op);
    }
}

fn apply(value: &[u8], state: &mut QcyState) -> Vec<u8> {
    let cmds = packet::parse(value);
    for c in &cmds {
        state.apply(c);
    }
    cmds.iter().map(|c| c.opcode).collect()
}

fn find_char(services: &[GattService], service: u16, characteristic: u16) -> Option<GattChar> {
    services
        .iter()
        .filter(|s| uuid16_matches(s.uuid16, &s.uuid128, service))
        .flat_map(|s| s.chars.iter())
        .find(|c| uuid16_matches(c.uuid16, &c.uuid128, characteristic))
        .cloned()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

#[cfg(test)]
/// [`view_all`] without a write
pub fn view(last: Option<&Reading>, link: Option<Unavailable>) -> Value {
    view_all(last, link, None)
}

fn write_view(w: &LastWrite) -> Value {
    // Toggles: enabled / previous as booleans; the level: value / previous
    // as numbers, with the reported maximum
    let (enabled, value, previous) = if w.setting == "noise_control" {
        // The code and its name: the screen maps it to mode + scene
        (Value::Null, json!(w.value), w.previous.map_or(Value::Null, |p| json!(p)))
    } else if w.toggle {
        (json!(w.value == 0x01), Value::Null, w.previous.map_or(Value::Null, |p| json!(p == 0x01)))
    } else {
        (Value::Null, json!(w.value), w.previous.map_or(Value::Null, |p| json!(p)))
    };
    json!({
        "setting": w.setting,
        "enabled": enabled,
        "value": value,
        "previous": previous,
        "max": w.max,
        "preset": if w.setting == "noise_control" { json!(write::noise_name(w.value)) } else { Value::Null },
        "previous_preset": match (w.setting, w.previous) {
            ("noise_control", Some(p)) => json!(write::noise_name(p)),
            _ => Value::Null,
        },
        "status": if w.failure.is_none() { "confirmed" } else { "failed" },
        "failure": w.failure,
        "sent": w.sent,
        "read_back": w.read_back,
        "steps": w.steps.iter().map(|(what, detail)| json!({ "what": what, "detail": detail })).collect::<Vec<_>>(),
        "at_ms": w.at_ms,
        // Phase F4: the curve asked for and the one read before (dB)
        "gains": w.eq.as_ref().map_or(Value::Null, |e| json!(db(&e.curve))),
        "previous_gains": w.eq.as_ref().and_then(|e| e.previous).map_or(Value::Null, |(_, c)| json!(db(&c))),
        "previous_preset_index": w.eq.as_ref().and_then(|e| e.previous).map_or(Value::Null, |(p, _)| json!(p)),
    })
}

/// The QcyState of qcy.ts (app/src/ui/src/ipc/qcy.ts): read_only false,
/// only the `writable` settings can change (game mode, sleep mode, wind
/// reduction, noise control, wear detection); `verified` lists those
/// confirmed on the N70 (all five: Gabriel, 2026-10-31); any other writable
/// one would show "Em teste" (the equalizer, phase F4)
pub fn view_all(last: Option<&Reading>, link: Option<Unavailable>, write: Option<&LastWrite>) -> Value {
    let link = match (link, last) {
        (Some(why), _) => json!({ "state": "unavailable", "reason": why.reason() }),
        (None, Some(_)) => json!({ "state": "connected" }),
        (None, None) => json!({ "state": "none" }),
    };
    let default = QcyState::default();
    let s = last.map_or(&default, |r| &r.state);
    let battery = |level: Option<u8>, charging: bool| json!({ "level": level, "charging": charging });
    let (noise_mode, noise_scene) = match s.noise_control {
        Some(n) => (
            Some(match n.mode {
                NoiseMode::NoiseCancellation => "anc",
                NoiseMode::Transparency => "transparency",
                NoiseMode::Normal => "off",
            }),
            match n.scene {
                Some(NoiseScene::Indoor) => "indoor",
                Some(NoiseScene::Commuting) => "commuting",
                Some(NoiseScene::Noisy) => "noisy",
                Some(NoiseScene::AntiWind) => "anti_wind",
                Some(NoiseScene::Adaptive) | None => "adaptive",
            },
        ),
        // 0x17 not answered in this read: unknown, never "off" (the N70
        // reported ANC adaptive while the screen showed Desligado)
        None => (None, "adaptive"),
    };
    let mut left = serde_json::Map::new();
    let mut right = serde_json::Map::new();
    for (key, side, gesture) in GESTURE_KEYS {
        if let Some(action) = s.key_functions.get(&key).and_then(|a| TOUCH_ACTIONS.get(usize::from(*a))) {
            let m = if side == "left" { &mut left } else { &mut right };
            m.insert(gesture.into(), json!(action));
        }
    }
    // The first ten slots (the N70 answers 20); unknown: zeros, eq_known false
    let eq_known = s.equalizer_gains.len() >= 10;
    let gains: Vec<f64> = if eq_known { s.equalizer_gains[..10].to_vec() } else { vec![0.0; 10] };
    let original = EQ_ORIGINAL.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(Value::Null, EqOriginal::to_json);
    let wear = s.wearing_detection.map(|w| w.enabled).or(s.wear_detection_enabled);
    // The 0x2C bytes as read (state, music action, ANC action, tone): the
    // screen shows them; a write copies them from a fresh read, not these
    let wear_read = s.wearing_detection.map_or(Value::Null, |w| {
        json!({
            "enabled": w.enabled,
            "music": w.music_action,
            "anc": w.anc_action,
            "tone": w.tone_enabled,
        })
    });
    json!({
        "link": link,
        "read_only": false,
        "writable": Setting::ALL.iter().filter(|s| write::ALLOWED.contains(&s.opcode())).map(|s| s.key()).collect::<Vec<_>>(),
        "verified": ["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"],
        "experimental": ["equalizer"],
        "write": write.map_or(Value::Null, write_view),
        "device_name": last.map_or("", |r| r.name.as_str()),
        "address": last.map_or("", |r| r.address.as_str()),
        "firmware": s.firmware_version,
        "battery": {
            "left": battery(s.battery.left, s.battery.left_charging),
            "right": battery(s.battery.right, s.battery.right_charging),
            "case": battery(s.battery.case, s.battery.case_charging),
        },
        "noise_mode": noise_mode,
        "noise_scene": noise_scene,
        "game_mode": s.game_mode.unwrap_or(false),
        "sleep_mode": s.sleep_mode.unwrap_or(false),
        "wind_reduction": s.wind_detection.unwrap_or(false),
        "wear_detection": wear.unwrap_or(false),
        "wear_read": wear_read,
        "ldac": s.ldac.unwrap_or(false),
        "multipoint": s.multipoint.unwrap_or(false),
        "prompt_volume": s.prompt_volume.unwrap_or(0),
        "prompt_volume_max": if last.is_some() { s.prompt_volume_max } else { 15 },
        "auto_off": match s.auto_power_off_minutes { None | Some(0xFFFF) => Value::Null, Some(m) => json!(m) },
        "eq": { "preset": "custom", "gains": gains, "master_gain": 0 },
        "eq_preset_index": s.equalizer_preset,
        "eq_known": eq_known,
        "eq_original": original,
        "gestures": { "left": left, "right": right },
        "supported_gestures": [],
        "reapply_on_connect": false,
        "profile_saved_at": Value::Null,
        "synced_at": last.map(|r| r.at_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qcy_protocol::packet::Command;

    fn reading() -> Reading {
        let mut state = QcyState::default();
        state.apply_battery_characteristic(&[96, 97, 57]);
        state.firmware_version = Some("3.0.13".into());
        state.apply(&Command { opcode: commands::opcode::LDAC, parameters: vec![0x01] });
        state.apply(&Command { opcode: commands::opcode::GAME_MODE, parameters: vec![0x02] });
        state.auto_power_off_minutes = Some(0xFFFF);
        state.key_functions.insert(0x03, 1);
        state.key_functions.insert(0x0A, 8);
        Reading { state, name: "Melon Buds N70".into(), address: "C4:33:96:11:0C:82".into(), answered: 11, at_ms: 42 }
    }

    #[test]
    fn nothing_read_yet_is_no_headset() {
        let v = current_of(None, None);
        assert_eq!(v["link"]["state"], "none");
        assert_eq!(v["read_only"], false);
        assert_eq!(
            v["writable"],
            json!(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"])
        );
        assert_eq!(
            v["verified"],
            json!(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"])
        );
        assert_eq!(v["write"], Value::Null);
        assert_eq!(v["synced_at"], Value::Null);
        assert_eq!(v["battery"]["left"]["level"], Value::Null);
        assert_eq!(v["noise_mode"], Value::Null, "0x17 unread is unknown, not off");
    }

    fn current_of(r: Option<&Reading>, l: Option<Unavailable>) -> Value {
        view(r, l)
    }

    #[test]
    fn a_reading_maps_to_the_ui_state() {
        let r = reading();
        let v = view(Some(&r), None);
        assert_eq!(v["link"]["state"], "connected");
        assert_eq!(v["battery"]["left"]["level"], 96);
        assert_eq!(v["battery"]["case"]["level"], 57);
        assert_eq!(v["firmware"], "3.0.13");
        assert_eq!(v["ldac"], true);
        assert_eq!(v["game_mode"], false);
        assert_eq!(v["auto_off"], Value::Null);
        assert_eq!(v["gestures"]["left"]["double_tap"], "play_pause");
        assert_eq!(v["gestures"]["right"]["long_press"], "noise_control");
        assert_eq!(v["synced_at"], 42);
        assert_eq!(v["eq"]["gains"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn a_failed_read_keeps_the_last_values() {
        let r = reading();
        let v = view(Some(&r), Some(Unavailable::LeOff));
        assert_eq!(v["link"]["state"], "unavailable");
        assert_eq!(v["link"]["reason"], "le_off");
        assert_eq!(v["battery"]["right"]["level"], 97);
    }

    #[test]
    fn answers_leave_the_pending_list() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mut pending: Vec<u8> = commands::STATE_QUERIES.to_vec();
        let mut state = QcyState::default();
        // No answer at all: nothing answered, the wait ends at the timeout
        assert_eq!(collect(&rx, &mut state, &mut pending.clone(), Duration::from_millis(10)), 0);
        drop(tx);
        // A closed channel ends the wait too
        assert_eq!(collect(&rx, &mut state, &mut pending, Duration::from_secs(5)), 0);
    }

    #[test]
    fn only_the_f2_changes_pass() {
        assert_eq!(allowed_change(&json!({ "kind": "game_mode", "enabled": true })), Some((Setting::GameMode, 0x01)));
        assert_eq!(allowed_change(&json!({ "kind": "game_mode", "enabled": false })), Some((Setting::GameMode, 0x02)));
        assert_eq!(allowed_change(&json!({ "kind": "sleep_mode", "enabled": true })), Some((Setting::SleepMode, 0x01)));
        assert_eq!(
            allowed_change(&json!({ "kind": "wind_reduction", "enabled": false })),
            Some((Setting::WindReduction, 0x02))
        );
        for c in [
            json!({ "kind": "ldac", "enabled": false }),
            json!({ "kind": "multipoint", "enabled": true }),
            json!({ "kind": "wear_detection", "enabled": 1 }),
            json!({ "kind": "wear_detection", "value": 1 }),
            json!({ "kind": "wear_detection" }),
            json!({ "kind": "wear_detection", "enabled": true, "music": 1 }),
            json!({ "kind": "wear_detection", "enabled": true, "bytes": [1, 1, 1] }),
            json!({ "kind": "reapply_on_connect", "enabled": true }),
            json!({ "kind": "noise", "mode": "anc" }),
            json!({ "kind": "noise_control", "value": 7 }),
            json!({ "kind": "noise_control", "value": 255 }),
            json!({ "kind": "noise_control", "value": 256 }),
            json!({ "kind": "noise_control", "value": -1 }),
            json!({ "kind": "noise_control", "value": 1.5 }),
            json!({ "kind": "noise_control", "value": "6" }),
            json!({ "kind": "noise_control", "enabled": true }),
            json!({ "kind": "noise_control" }),
            json!({ "kind": "noise_control", "value": 6, "bytes": [3, 1, 4] }),
            json!({ "kind": "noise_control", "mode": "transparency" }),
            json!({ "kind": "auto_off", "minutes": 5 }),
            json!({ "kind": "game_mode" }),
            json!({ "kind": "game_mode", "enabled": 1 }),
            json!({ "kind": "game_mode", "enabled": true, "opcode": 3 }),
            json!({ "kind": "sleep_mode", "value": 1 }),
            json!({ "kind": "prompt_volume", "enabled": true }),
            json!({ "kind": "prompt_volume", "value": 12 }),
            json!({ "kind": "prompt_volume", "value": 0 }),
            json!({ "kind": "prompt_volume", "value": 101 }),
            json!({ "kind": "prompt_volume", "value": 256 }),
            json!({ "kind": "prompt_volume", "value": -1 }),
            json!({ "kind": "prompt_volume", "value": 1.5 }),
            json!({ "kind": "prompt_volume", "value": "9" }),
            json!({ "kind": "prompt_volume", "value": 9, "max": 15 }),
            json!(null),
        ] {
            assert_eq!(allowed_change(&c), None, "{c}");
        }
    }

    #[test]
    fn a_read_without_0x17_keeps_the_last_noise_control() {
        let mut good = reading();
        good.state.noise_control =
            Some(commands::NoiseControlState::create(NoiseMode::NoiseCancellation, NoiseScene::Adaptive));
        let mut miss = reading();
        miss.state.noise_control = None;
        let kept = keep_known(Some(&good), miss.clone());
        assert_eq!(kept.state.noise_control, good.state.noise_control);
        assert_eq!(view(Some(&kept), None)["noise_mode"], "anc");
        // Another headset, or nothing before: unknown (null), never "off"
        let mut other = miss.clone();
        other.address = "AA:BB:CC:DD:EE:FF".into();
        assert_eq!(keep_known(Some(&good), other).state.noise_control, None);
        let none = keep_known(None, miss);
        assert_eq!(view(Some(&none), None)["noise_mode"], Value::Null);
        // A fresh answer wins over the kept one
        let mut fresh = reading();
        fresh.state.noise_control =
            Some(commands::NoiseControlState::create(NoiseMode::Transparency, NoiseScene::Adaptive));
        assert_eq!(keep_known(Some(&good), fresh.clone()).state.noise_control, fresh.state.noise_control);
    }

    #[test]
    fn wear_detection_passes_as_a_toggle_only() {
        assert_eq!(
            allowed_change(&json!({ "kind": "wear_detection", "enabled": true })),
            Some((Setting::WearDetection, 0x01))
        );
        assert_eq!(
            allowed_change(&json!({ "kind": "wear_detection", "enabled": false })),
            Some((Setting::WearDetection, 0x02))
        );
        let v = view_all(Some(&reading()), None, None);
        assert_eq!(v["experimental"], json!(["equalizer"]));
        assert!(v["verified"].as_array().unwrap().contains(&json!("wear_detection")));
        assert_eq!(v["reapply_on_connect"], false);
    }

    #[test]
    fn the_wear_bytes_are_shown_as_read() {
        let mut r = reading();
        assert_eq!(view_all(Some(&r), None, None)["wear_read"], Value::Null);
        r.state.apply(&Command { opcode: commands::opcode::WEARING_DETECTION, parameters: vec![0x00, 0x01, 0x01] });
        let v = view_all(Some(&r), None, None);
        assert_eq!(v["wear_detection"], false);
        assert_eq!(v["wear_read"], json!({ "enabled": false, "music": 1, "anc": 1, "tone": null }));
    }

    #[test]
    fn the_seven_noise_codes_pass() {
        for code in 0..=6u8 {
            assert_eq!(
                allowed_change(&json!({ "kind": "noise_control", "value": code })),
                Some((Setting::NoiseControl, code))
            );
        }
    }

    #[test]
    fn a_noise_write_shows_the_codes() {
        let w = LastWrite {
            setting: "noise_control",
            toggle: false,
            value: 6,
            previous: Some(1),
            max: None,
            sent: Some("FF0517030301 04".replace(' ', "")),
            read_back: Some("030104".into()),
            failure: None,
            steps: vec![],
            at_ms: 7,
            eq: None,
        };
        let v = view_all(Some(&reading()), None, Some(&w));
        assert_eq!(v["write"]["value"], 6);
        assert_eq!(v["write"]["previous"], 1);
        assert_eq!(v["write"]["preset"], "transparency");
        assert_eq!(v["write"]["previous_preset"], "anc_adaptive");
        assert_eq!(v["write"]["enabled"], Value::Null);
        assert_eq!(v["write"]["max"], Value::Null);
    }

    #[test]
    fn writable_and_verified() {
        let v = view_all(Some(&reading()), None, None);
        assert_eq!(
            v["writable"],
            json!(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"])
        );
        assert_eq!(
            v["verified"],
            json!(["game_mode", "sleep_mode", "wind_reduction", "noise_control", "wear_detection"])
        );
    }

    #[test]
    fn a_volume_write_shows_numbers() {
        let w = LastWrite {
            setting: "prompt_volume",
            toggle: false,
            value: 12,
            previous: Some(9),
            max: Some(15),
            sent: Some("FF031D010C".into()),
            read_back: Some("0C0F".into()),
            failure: None,
            steps: vec![],
            at_ms: 7,
            eq: None,
        };
        let v = view_all(Some(&reading()), None, Some(&w));
        assert_eq!(v["write"]["value"], 12);
        assert_eq!(v["write"]["previous"], 9);
        assert_eq!(v["write"]["max"], 15);
        assert_eq!(v["write"]["enabled"], Value::Null);
    }

    #[test]
    fn a_write_shows_in_the_state() {
        let r = reading();
        let w = LastWrite {
            setting: "game_mode",
            toggle: true,
            value: 0x01,
            previous: Some(0x02),
            max: None,
            sent: Some("FF03090101".into()),
            read_back: Some("0100".into()),
            failure: None,
            steps: vec![("sent".into(), "FF03090101".into())],
            at_ms: 7,
            eq: None,
        };
        let v = view_all(Some(&r), None, Some(&w));
        assert_eq!(v["write"]["status"], "confirmed");
        assert_eq!(v["write"]["previous"], false);
        assert_eq!(v["write"]["sent"], "FF03090101");
        let failed = LastWrite { failure: Some("timeout"), read_back: None, ..w };
        let v = view_all(Some(&r), None, Some(&failed));
        assert_eq!(v["write"]["status"], "failed");
        assert_eq!(v["write"]["failure"], "timeout");
    }

    #[test]
    fn only_read_requests_are_ever_framed() {
        for op in commands::STATE_QUERIES {
            let f = commands::request(op);
            assert_eq!(f[2], 0xFE, "0x{op:02X}");
            assert!(!(0x01..=0x03).contains(&f[2]));
        }
    }

    // ---- Phase F4: equalizer ----

    #[test]
    fn only_a_ten_band_curve_in_range_passes() {
        let rock = json!([4, 3, 1.5, -0.5, -1.5, -1, 1, 2.5, 3.5, 3.5]);
        assert_eq!(
            allowed_eq(&json!({ "kind": "equalizer", "gains": rock })),
            Some([400, 300, 150, -50, -150, -100, 100, 250, 350, 350])
        );
        assert!(allowed_eq(&json!({ "kind": "equalizer", "gains": [8, -8, 0, 0, 0, 0, 0, 0, 0, 0] })).is_some());
        for c in [
            json!({ "kind": "equalizer", "gains": [8.5, 0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": [0, 0, 0, 0, 0, 0, 0, 0, 0, -9] }),
            json!({ "kind": "equalizer", "gains": [0.001, 0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": [0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": ["1", 0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": [null, 0, 0, 0, 0, 0, 0, 0, 0, 0] }),
            json!({ "kind": "equalizer", "gains": rock, "preset": 1 }),
            json!({ "kind": "equalizer", "gains": rock, "master_gain": 0 }),
            json!({ "kind": "equalizer" }),
            json!({ "kind": "eq", "gains": rock }),
            json!({ "kind": "eq", "eq": { "preset": "rock", "gains": rock, "master_gain": 0 } }),
        ] {
            assert_eq!(allowed_eq(&c), None, "{c}");
            // Never as a one-byte setting either
            assert_eq!(allowed_change(&c), None, "{c}");
        }
        assert_eq!(allowed_change(&json!({ "kind": "equalizer", "value": 1 })), None);
    }

    #[test]
    fn the_eq_view_uses_the_first_ten_slots() {
        let mut r = reading();
        assert_eq!(view_all(Some(&r), None, None)["eq_known"], false);
        // The N70 answers 20 slots
        let mut p = vec![1u8, 0, 0];
        for i in 0..20u16 {
            let f = commands::EQUALIZER_FREQUENCIES.get(usize::from(i)).copied().unwrap_or(16000);
            p.extend(f.to_le_bytes());
            p.extend((if i < 10 { 50 * i16::try_from(i).unwrap() } else { -700 }).to_le_bytes());
            p.extend([100, 0, 0]);
        }
        r.state.apply(&Command { opcode: commands::opcode::EQUALIZER, parameters: p });
        let v = view_all(Some(&r), None, None);
        assert_eq!(v["eq_known"], true);
        assert_eq!(v["eq"]["gains"], json!([0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5]));
        assert_eq!(v["eq_preset_index"], 1);
        assert!(v["writable"].as_array().unwrap().contains(&json!("equalizer")));
        assert!(!v["verified"].as_array().unwrap().contains(&json!("equalizer")));
        assert_eq!(v["reapply_on_connect"], false);
        // set_gains keeps slots 11..20 as read
        set_gains(&mut r.state, 8, &[100; 10]);
        assert_eq!(r.state.equalizer_gains.len(), 20);
        assert_eq!(r.state.equalizer_gains[..10], [1.0; 10]);
        assert_eq!(r.state.equalizer_gains[10], -7.0);
        assert_eq!(r.state.equalizer_preset, Some(8));
    }

    #[test]
    fn an_eq_write_shows_both_curves() {
        let w = LastWrite {
            setting: "equalizer",
            toggle: false,
            value: 0,
            previous: None,
            max: None,
            sent: Some("FF4B2249".into()),
            read_back: None,
            failure: Some("timeout"),
            steps: vec![],
            at_ms: 9,
            eq: Some(EqWrite { curve: [250, 0, 0, 0, 0, 0, 0, 0, 0, -50], previous: Some((1, [0; 10])) }),
        };
        let v = view_all(Some(&reading()), None, Some(&w));
        assert_eq!(v["write"]["setting"], "equalizer");
        assert_eq!(v["write"]["status"], "failed");
        assert_eq!(v["write"]["gains"], json!([2.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.5]));
        assert_eq!(v["write"]["previous_gains"], json!([0.0; 10]));
        assert_eq!(v["write"]["previous_preset_index"], 1);
        let o = EqOriginal { preset: 1, curve: [0, 0, 0, 0, 0, 0, 0, 0, 0, 25], saved_at: 5 };
        assert_eq!(EqOriginal::from_json(&o.to_json()), Some(o));
        assert_eq!(EqOriginal::from_json(&json!({ "preset": 1, "gains": [9, 0, 0, 0, 0, 0, 0, 0, 0, 0], "saved_at": 5 })), None);
    }
}
