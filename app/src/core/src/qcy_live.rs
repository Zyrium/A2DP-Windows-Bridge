//! The Fones screen's QCY reads (MeloBuds N70), READ ONLY: `qcy.state`,
//! `qcy.refresh`, `qcy.reconnect` and the `qcy` event. Nothing here changes
//! a setting of the earbuds: the only frames written to 1001 are the
//! `FF 03 FE <op>` state requests of [`commands::STATE_QUERIES`] (opcode
//! 0xFE), and `le_connect_plain` never pairs.
//!
//! Audio: the reads use the resident core's own BTstack (the one that
//! streams A2DP), so no second process takes the adapter and the stack is
//! never restarted here. That needs LE in the running stack (LE Audio
//! (experimental) on): when it is off and audio streams, turning LE on
//! would restart BTstack and drop the audio, so the read is refused
//! (`le_off`) instead. Nothing polls: a read happens only on an explicit
//! qcy.refresh / qcy.reconnect (the "Atualizar" / "Tentar de novo" buttons).

use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use a2dpwb_core::Service;
use bt_host::{BtHost, GattChar, GattService, LeScanResult};
use qcy_protocol::advertisement::{self, Advertisement};
use qcy_protocol::commands::{self, NoiseMode, NoiseScene};
use qcy_protocol::packet;
use qcy_protocol::state::QcyState;
use qcy_protocol::uuids::{self, uuid16_matches};
use serde_json::{Value, json};

const SCAN: Duration = Duration::from_secs(4);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const GATT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(2500);
const SETTLE: Duration = Duration::from_millis(500);

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
const GESTURE_KEYS: [(u8, &str, &str); 4] =
    [(0x03, "left", "double_tap"), (0x04, "right", "double_tap"), (0x09, "left", "long_press"), (0x0A, "right", "long_press")];

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

#[derive(Default)]
struct Cache {
    last: Option<Reading>,
    link: Option<Unavailable>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache { last: None, link: None });
/// One read at a time
static READING: Mutex<()> = Mutex::new(());

/// The last known state (no Bluetooth traffic)
pub fn current() -> Value {
    let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    view(c.last.as_ref(), c.link)
}

/// Reads the earbuds now (blocking: run off the async runtime)
pub fn refresh(service: &Service) -> Value {
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    let result = host_for_read(service).and_then(|h| read(&h));
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(r) => {
            tracing::info!("QCY: read {} of {} queries from {}", r.answered, commands::STATE_QUERIES.len(), r.address);
            c.last = Some(r);
            c.link = None;
        }
        Err(why) => {
            tracing::warn!("QCY: no read ({})", why.reason());
            c.link = Some(why);
        }
    }
    view(c.last.as_ref(), c.link)
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

fn read(host: &BtHost) -> Result<Reading, Unavailable> {
    let found: Vec<(LeScanResult, Advertisement)> = host
        .le_scan(SCAN)
        .into_iter()
        .filter_map(|r| {
            let a = advertisement::find_in_ad(&r.adv).or_else(|| advertisement::find_in_ad(&r.scan_rsp))?;
            Some((r, a))
        })
        .collect();
    let Some((target, adv)) = found.into_iter().find(|(_, a)| uuids::is_n70(a.vendor_id)) else {
        return Err(Unavailable::NotFound);
    };
    let name = if target.name.is_empty() { "QCY MeloBuds N70".to_string() } else { target.name.clone() };
    let address = adv.control_address.map_or_else(|| target.addr.to_string(), advertisement::format_address);
    let id = host.le_connect_plain(target.addr, &name, CONNECT_TIMEOUT).map_err(|e| {
        tracing::warn!("QCY: connection failed: {e}");
        if e.to_lowercase().contains("timeout") || e.to_lowercase().contains("timed out") {
            Unavailable::Timeout
        } else {
            Unavailable::GattFailed
        }
    })?;
    let r = query(host, id);
    host.le_disconnect(id);
    let (mut state, answered) = r?;
    if state.battery.left.is_none() {
        // The advertisement's battery when the characteristic said nothing
        state.battery.left = Some(adv.left_battery);
        state.battery.right = Some(adv.right_battery);
        state.battery.case = (adv.case_battery > 0).then_some(adv.case_battery);
    }
    Ok(Reading { state, name, address, answered, at_ms: now_ms() })
}

fn query(host: &BtHost, id: bt_host::LeDeviceId) -> Result<(QcyState, usize), Unavailable> {
    let services = host.le_gatt_services(id);
    let command = find_char(&services, uuids::MAIN_SERVICE, uuids::COMMAND).ok_or(Unavailable::GattFailed)?;
    let notification =
        find_char(&services, uuids::MAIN_SERVICE, uuids::NOTIFICATION).ok_or(Unavailable::GattFailed)?;
    let mut state = QcyState::default();
    let (tx, responses) = mpsc::channel::<Vec<u8>>();
    host.le_gatt_subscribe(id, notification, tx, GATT_TIMEOUT).map_err(|_| Unavailable::GattFailed)?;

    for uuid in [uuids::FIRMWARE_VERSION, uuids::BATTERY, uuids::KEY_FUNCTIONS] {
        let Some(chr) = find_char(&services, uuids::MAIN_SERVICE, uuid) else { continue };
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

    let mut answered = 0;
    for op in commands::STATE_QUERIES {
        // Read-only requests only (FF 03 FE <op>)
        let frame = commands::request(op);
        debug_assert_eq!(frame[2], packet::REQUEST_OPCODE);
        while responses.try_recv().is_ok() {}
        let written = if command.properties & 0x04 != 0 {
            host.le_gatt_write_without_response(id, command.value_handle, frame, GATT_TIMEOUT)
        } else {
            host.le_gatt_write(id, command.value_handle, frame, GATT_TIMEOUT)
        };
        if written.is_err() {
            continue;
        }
        if wait_for(&responses, op, &mut state) {
            answered += 1;
        }
    }
    let until = Instant::now() + SETTLE;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match responses.recv_timeout(left) {
            Ok(v) => {
                apply(&v, &mut state);
            }
            Err(_) => break,
        }
    }
    if answered == 0 && state.battery.left.is_none() && state.firmware_version.is_none() {
        return Err(Unavailable::Timeout);
    }
    Ok((state, answered))
}

fn wait_for(responses: &mpsc::Receiver<Vec<u8>>, opcode: u8, state: &mut QcyState) -> bool {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
        match responses.recv_timeout(left) {
            Ok(v) if apply(&v, state).contains(&opcode) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
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

/// The QcyState of qcy.ts (app/src/ui/src/ipc/qcy.ts), read_only: true
pub fn view(last: Option<&Reading>, link: Option<Unavailable>) -> Value {
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
            match n.mode {
                NoiseMode::NoiseCancellation => "anc",
                NoiseMode::Transparency => "transparency",
                NoiseMode::Normal => "off",
            },
            match n.scene {
                Some(NoiseScene::Indoor) => "indoor",
                Some(NoiseScene::Commuting) => "commuting",
                Some(NoiseScene::Noisy) => "noisy",
                Some(NoiseScene::AntiWind) => "anti_wind",
                Some(NoiseScene::Adaptive) | None => "adaptive",
            },
        ),
        None => ("off", "adaptive"),
    };
    let mut left = serde_json::Map::new();
    let mut right = serde_json::Map::new();
    for (key, side, gesture) in GESTURE_KEYS {
        if let Some(action) = s.key_functions.get(&key).and_then(|a| TOUCH_ACTIONS.get(usize::from(*a))) {
            let m = if side == "left" { &mut left } else { &mut right };
            m.insert(gesture.into(), json!(action));
        }
    }
    let gains: Vec<f64> = if s.equalizer_gains.len() == 10 { s.equalizer_gains.clone() } else { vec![0.0; 10] };
    let wear = s.wearing_detection.map(|w| w.enabled).or(s.wear_detection_enabled);
    json!({
        "link": link,
        "read_only": true,
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
        "ldac": s.ldac.unwrap_or(false),
        "multipoint": s.multipoint.unwrap_or(false),
        "prompt_volume": s.prompt_volume.unwrap_or(0),
        "prompt_volume_max": if last.is_some() { s.prompt_volume_max } else { 15 },
        "auto_off": match s.auto_power_off_minutes { None | Some(0xFFFF) => Value::Null, Some(m) => json!(m) },
        "eq": { "preset": "custom", "gains": gains, "master_gain": 0 },
        "eq_preset_index": s.equalizer_preset,
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
        assert_eq!(v["read_only"], true);
        assert_eq!(v["synced_at"], Value::Null);
        assert_eq!(v["battery"]["left"]["level"], Value::Null);
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
    fn only_read_requests_are_ever_framed() {
        for op in commands::STATE_QUERIES {
            let f = commands::request(op);
            assert_eq!(f[2], 0xFE, "0x{op:02X}");
            assert!(!(0x01..=0x03).contains(&f[2]));
        }
    }
}
