//! The device state, kept up to date from what the earbuds send: responses
//! and notifications on characteristic 1002 ([`QcyState::apply`]) and the
//! direct reads of battery (0008), firmware (0007) and key functions (000D).
//!
//! Same rules as OpenQCY's `QcyDeviceClient.ApplyCommand`: every field is
//! `None` (unknown) until the device reported it, a response with too few
//! parameters is ignored, and the older in-ear opcode 0x06 never overrides
//! the advanced wearing-detection opcode 0x2C.

use std::collections::BTreeMap;

use crate::commands::{EqualizerState, NoiseControlState, WearingDetection, opcode, parse_key_functions};
use crate::packet::Command;

/// Which opcode family reports wear detection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WearDetectionProtocol {
    #[default]
    Unknown,
    /// Opcode 0x06
    Legacy,
    /// Opcode 0x2C
    WearingDetection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Battery {
    pub left: Option<u8>,
    pub right: Option<u8>,
    /// None while the case reports 0 (nothing known)
    pub case: Option<u8>,
    pub left_charging: bool,
    pub right_charging: bool,
    pub case_charging: bool,
}

impl Battery {
    /// Characteristic 0008 and the 0x2F response: left, right, case; bit 7
    /// of each is "charging", the low 7 bits the percentage (capped at 100)
    pub fn parse(parameters: &[u8]) -> Battery {
        let level = |v: u8| (v & 0x7F).min(100);
        Battery {
            left: parameters.first().map(|v| level(*v)),
            right: parameters.get(1).map(|v| level(*v)),
            case: parameters.get(2).filter(|v| **v & 0x7F > 0).map(|v| level(*v)),
            left_charging: parameters.first().is_some_and(|v| v & 0x80 != 0),
            right_charging: parameters.get(1).is_some_and(|v| v & 0x80 != 0),
            case_charging: parameters.get(2).is_some_and(|v| v & 0x80 != 0),
        }
    }
}

/// "L 3.0.13 · R 3.0.13" for six bytes, "3.0.13" for three to five; None
/// below three
pub fn format_version(value: &[u8]) -> Option<String> {
    match *value {
        [a, b, c, d, e, f, ..] => Some(format!("L {a}.{b}.{c} · R {d}.{e}.{f}")),
        [a, b, c, ..] => Some(format!("{a}.{b}.{c}")),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QcyState {
    pub battery: Battery,
    pub firmware_version: Option<String>,
    pub wear_detection_enabled: Option<bool>,
    pub wear_detection_protocol: WearDetectionProtocol,
    pub wearing_detection: Option<WearingDetection>,
    pub noise_control: Option<NoiseControlState>,
    pub game_mode: Option<bool>,
    pub sleep_mode: Option<bool>,
    pub ldac: Option<bool>,
    pub multipoint: Option<bool>,
    pub wind_detection: Option<bool>,
    pub prompt_volume: Option<u8>,
    /// 100 until the device reports its own maximum (the N70 reports 15)
    pub prompt_volume_max: u8,
    /// 0xFFFF: never
    pub auto_power_off_minutes: Option<u16>,
    pub key_functions: BTreeMap<u8, u8>,
    pub equalizer_preset: Option<u8>,
    /// dB per band
    pub equalizer_gains: Vec<f64>,
}

impl Default for QcyState {
    fn default() -> QcyState {
        QcyState {
            battery: Battery::default(),
            firmware_version: None,
            wear_detection_enabled: None,
            wear_detection_protocol: WearDetectionProtocol::Unknown,
            wearing_detection: None,
            noise_control: None,
            game_mode: None,
            sleep_mode: None,
            ldac: None,
            multipoint: None,
            wind_detection: None,
            prompt_volume: None,
            prompt_volume_max: 100,
            auto_power_off_minutes: None,
            key_functions: BTreeMap::new(),
            equalizer_preset: None,
            equalizer_gains: Vec::new(),
        }
    }
}

impl QcyState {
    /// Applies one block of a response / notification; true when the state
    /// changed
    pub fn apply(&mut self, command: &Command) -> bool {
        let before = self.clone();
        let p = command.parameters.as_slice();
        let on = |p: &[u8]| p[0] == 0x01;
        match command.opcode {
            opcode::IN_EAR_DETECTION if !p.is_empty() => {
                if self.wear_detection_protocol != WearDetectionProtocol::WearingDetection {
                    self.wear_detection_enabled = Some(on(p));
                    self.wear_detection_protocol = WearDetectionProtocol::Legacy;
                }
            }
            opcode::WEARING_DETECTION => {
                if let Some(w) = WearingDetection::parse(p) {
                    self.wear_detection_enabled = Some(w.enabled);
                    self.wear_detection_protocol = WearDetectionProtocol::WearingDetection;
                    self.wearing_detection = Some(w);
                }
            }
            opcode::NOISE_CONTROL if p.len() >= 3 => {
                if let Some(n) = NoiseControlState::parse(p) {
                    self.noise_control = Some(n);
                }
            }
            opcode::GAME_MODE if !p.is_empty() => self.game_mode = Some(on(p)),
            opcode::SLEEP_MODE if !p.is_empty() => self.sleep_mode = Some(on(p)),
            opcode::LDAC if !p.is_empty() => self.ldac = Some(on(p)),
            opcode::MULTIPOINT if !p.is_empty() => self.multipoint = Some(on(p)),
            opcode::WIND_DETECTION if !p.is_empty() => self.wind_detection = Some(on(p)),
            opcode::PROMPT_VOLUME if !p.is_empty() => {
                self.prompt_volume = Some(p[0]);
                if let Some(max) = p.get(1).filter(|m| **m > 0) {
                    self.prompt_volume_max = *max;
                }
            }
            opcode::AUTO_POWER_OFF if p.len() >= 2 => {
                self.auto_power_off_minutes = Some(u16::from_le_bytes([p[0], p[1]]));
            }
            opcode::BATTERY if p.len() >= 2 => self.battery = Battery::parse(p),
            opcode::FIRMWARE if p.len() >= 3 => self.firmware_version = format_version(p),
            opcode::EQUALIZER => {
                if let Some(EqualizerState { preset_index, bands, .. }) = EqualizerState::parse_v2(p) {
                    self.equalizer_preset = Some(preset_index);
                    self.equalizer_gains = bands.iter().map(|b| b.gain).collect();
                }
            }
            opcode::KEY_FUNCTIONS if p.len() >= 2 => self.key_functions = parse_key_functions(p),
            _ => {}
        }
        *self != before
    }

    /// A value of characteristic 0008 (read or notification); too short
    /// values are ignored
    pub fn apply_battery_characteristic(&mut self, value: &[u8]) -> bool {
        if value.len() < 2 {
            return false;
        }
        let battery = Battery::parse(value);
        let changed = self.battery != battery;
        self.battery = battery;
        changed
    }

    /// A read of characteristic 0007
    pub fn apply_firmware_characteristic(&mut self, value: &[u8]) -> bool {
        match format_version(value) {
            Some(v) if self.firmware_version.as_ref() != Some(&v) => {
                self.firmware_version = Some(v);
                true
            }
            _ => false,
        }
    }

    /// A read of characteristic 000D
    pub fn apply_key_functions_characteristic(&mut self, value: &[u8]) -> bool {
        let map = parse_key_functions(value);
        let changed = self.key_functions != map;
        self.key_functions = map;
        changed
    }

    /// A read of the legacy equalizer characteristic 000B
    pub fn apply_equalizer_characteristic(&mut self, value: &[u8]) -> bool {
        match value.first() {
            Some(preset) if self.equalizer_preset != Some(*preset) => {
                self.equalizer_preset = Some(*preset);
                true
            }
            _ => false,
        }
    }

    /// The state as printable lines (the probe command and the log)
    pub fn summary(&self) -> Vec<String> {
        let flag = |v: Option<bool>| match v {
            Some(true) => "on",
            Some(false) => "off",
            None => "?",
        };
        let level = |v: Option<u8>, charging: bool| {
            let text = v.map_or_else(|| "?".to_string(), |v| format!("{v}%"));
            if charging { format!("{text} (charging)") } else { text }
        };
        let b = &self.battery;
        vec![
            format!(
                "battery: left {}, right {}, case {}",
                level(b.left, b.left_charging),
                level(b.right, b.right_charging),
                level(b.case, b.case_charging)
            ),
            format!("firmware: {}", self.firmware_version.as_deref().unwrap_or("?")),
            format!(
                "noise control: {}",
                self.noise_control.map_or_else(
                    || "?".to_string(),
                    |n| match n.scene {
                        Some(scene) => format!("{:?} / {scene:?} ({n})", n.mode),
                        None => format!("{:?} ({n})", n.mode),
                    }
                )
            ),
            format!("wear detection: {} ({:?})", flag(self.wear_detection_enabled), self.wear_detection_protocol),
            format!(
                "game mode: {}, sleep mode: {}, LDAC: {}, multipoint: {}, wind detection: {}",
                flag(self.game_mode),
                flag(self.sleep_mode),
                flag(self.ldac),
                flag(self.multipoint),
                flag(self.wind_detection)
            ),
            format!(
                "prompt volume: {} of {}",
                self.prompt_volume.map_or_else(|| "?".to_string(), |v| v.to_string()),
                self.prompt_volume_max
            ),
            format!(
                "auto power-off: {}",
                match self.auto_power_off_minutes {
                    Some(0xFFFF) => "never".to_string(),
                    Some(m) => format!("{m} min"),
                    None => "?".to_string(),
                }
            ),
            format!("key functions: {:?}", self.key_functions),
            format!("equalizer: preset {:?}, gains {:?}", self.equalizer_preset, self.equalizer_gains),
        ]
    }

    /// Prompt volume as a 0-100 percentage of the device's maximum
    pub fn prompt_volume_percent(&self) -> Option<u8> {
        let max = u32::from(self.prompt_volume_max.max(1));
        self.prompt_volume.map(|v| ((u32::from(v) * 100 + max / 2) / max).min(100) as u8)
    }
}

/// The raw value of a prompt-volume write for `percent` (0-100) of the
/// device's maximum, as OpenQCY computes it (rounded, clamped)
pub fn prompt_volume_value(percent: f64, maximum: u8) -> u8 {
    (percent / 100.0 * f64::from(maximum)).round_ties_even().clamp(0.0, f64::from(maximum)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{NoiseMode, NoiseScene};
    use crate::packet::parse;
    use crate::test_util::hex;

    /// Feeds a whole received packet
    fn feed(state: &mut QcyState, packet: &str) -> bool {
        let mut changed = false;
        for command in parse(&hex(packet)) {
            changed |= state.apply(&command);
        }
        changed
    }

    #[test]
    fn summary_of_the_transcript() {
        let mut s = QcyState::default();
        assert!(s.summary().iter().any(|l| l == "firmware: ?"));
        feed(&mut s, "FF051703010302");
        feed(&mut s, "FF03230101");
        feed(&mut s, "FF061404FFFF0000");
        s.apply_battery_characteristic(&[0x80 | 50, 40, 0]);
        let text = s.summary().join("\n");
        assert!(text.contains("battery: left 50% (charging), right 40%, case ?"), "{text}");
        assert!(text.contains("noise control: NoiseCancellation / Noisy (010302)"), "{text}");
        assert!(text.contains("LDAC: on"), "{text}");
        assert!(text.contains("auto power-off: never"), "{text}");
    }

    #[test]
    fn the_whole_transcript() {
        let mut s = QcyState::default();
        assert!(feed(&mut s, "FF052C03000101")); // wearing detection off
        assert!(feed(&mut s, "FF051703010302")); // ANC: noisy
        assert!(feed(&mut s, "FF03090102")); // game mode off
        assert!(feed(&mut s, "FF03100100")); // sleep mode off
        assert!(feed(&mut s, "FF03230101")); // LDAC on
        assert!(feed(&mut s, "FF03240101")); // multipoint on
        assert!(feed(&mut s, "FF032A0101")); // wind detection on
        assert!(feed(&mut s, "FF041D02040F")); // prompt volume 4 / 15
        assert!(feed(&mut s, "FF061404FFFF0000")); // auto power-off: never

        assert_eq!(s.wear_detection_enabled, Some(false));
        assert_eq!(s.wear_detection_protocol, WearDetectionProtocol::WearingDetection);
        assert_eq!(s.noise_control, Some(NoiseControlState::create(NoiseMode::NoiseCancellation, NoiseScene::Noisy)));
        assert_eq!(s.game_mode, Some(false));
        // 0x00 is neither "on" (01) nor the usual "off" (02): not on
        assert_eq!(s.sleep_mode, Some(false));
        assert_eq!(s.ldac, Some(true));
        assert_eq!(s.multipoint, Some(true));
        assert_eq!(s.wind_detection, Some(true));
        assert_eq!(s.prompt_volume, Some(4));
        assert_eq!(s.prompt_volume_max, 15);
        assert_eq!(s.auto_power_off_minutes, Some(0xFFFF));
    }

    #[test]
    fn applying_the_same_value_again_changes_nothing() {
        let mut s = QcyState::default();
        assert!(feed(&mut s, "FF03230101"));
        assert!(!feed(&mut s, "FF03230101"));
    }

    #[test]
    fn short_responses_are_ignored() {
        let mut s = QcyState::default();
        let none = |opcode| Command { opcode, parameters: vec![] };
        for op in [opcode::GAME_MODE, opcode::LDAC, opcode::PROMPT_VOLUME, opcode::IN_EAR_DETECTION] {
            assert!(!s.apply(&none(op)));
        }
        assert!(!s.apply(&Command { opcode: opcode::AUTO_POWER_OFF, parameters: vec![1] }));
        assert!(!s.apply(&Command { opcode: opcode::NOISE_CONTROL, parameters: vec![1, 2] }));
        assert!(!s.apply(&Command { opcode: opcode::FIRMWARE, parameters: vec![3, 0] }));
        assert!(!s.apply(&Command { opcode: 0x7E, parameters: vec![1, 2, 3] }));
        assert_eq!(s, QcyState::default());
    }

    #[test]
    fn legacy_wear_detection_never_overrides_the_advanced_one() {
        let mut s = QcyState::default();
        assert!(feed(&mut s, "FF03060101"));
        assert_eq!(s.wear_detection_protocol, WearDetectionProtocol::Legacy);
        assert_eq!(s.wear_detection_enabled, Some(true));
        // The advanced opcode takes over
        assert!(feed(&mut s, "FF052C03000101"));
        assert_eq!(s.wear_detection_protocol, WearDetectionProtocol::WearingDetection);
        // And the legacy one is ignored from then on
        assert!(!feed(&mut s, "FF03060101"));
        assert_eq!(s.wear_detection_enabled, Some(false));
    }

    #[test]
    fn prompt_volume_keeps_the_known_maximum() {
        let mut s = QcyState::default();
        assert_eq!(s.prompt_volume_max, 100);
        feed(&mut s, "FF041D02040F");
        // A response without a maximum (or with 0) keeps 15
        feed(&mut s, "FF031D0107");
        assert_eq!((s.prompt_volume, s.prompt_volume_max), (Some(7), 15));
        feed(&mut s, "FF041D020800");
        assert_eq!((s.prompt_volume, s.prompt_volume_max), (Some(8), 15));
    }

    #[test]
    fn prompt_volume_percentages() {
        let mut s = QcyState { prompt_volume: Some(15), prompt_volume_max: 15, ..QcyState::default() };
        assert_eq!(s.prompt_volume_percent(), Some(100));
        s.prompt_volume = Some(4);
        assert_eq!(s.prompt_volume_percent(), Some(27));
        s.prompt_volume = None;
        assert_eq!(s.prompt_volume_percent(), None);
        assert_eq!(prompt_volume_value(100.0, 15), 15);
        assert_eq!(prompt_volume_value(0.0, 15), 0);
        assert_eq!(prompt_volume_value(50.0, 15), 8); // 7.5 rounds to even: 8
        assert_eq!(prompt_volume_value(250.0, 15), 15);
        assert_eq!(prompt_volume_value(-5.0, 15), 0);
    }

    #[test]
    fn battery_values() {
        let b = Battery::parse(&[0x80 | 50, 40, 0x80 | 10]);
        assert_eq!((b.left, b.right, b.case), (Some(50), Some(40), Some(10)));
        assert_eq!((b.left_charging, b.right_charging, b.case_charging), (true, false, true));
        // A case reporting 0 is unknown; levels are capped
        let b = Battery::parse(&[0x7F, 0x7F, 0x00]);
        assert_eq!((b.left, b.right, b.case), (Some(100), Some(100), None));
        // Two bytes only: no case
        let b = Battery::parse(&[20, 30]);
        assert_eq!((b.left, b.right, b.case), (Some(20), Some(30), None));
        assert_eq!(Battery::parse(&[]), Battery::default());
    }

    #[test]
    fn battery_from_the_characteristic_and_from_opcode_2f() {
        let mut s = QcyState::default();
        assert!(!s.apply_battery_characteristic(&[5]));
        assert!(s.apply_battery_characteristic(&[60, 70, 80]));
        assert!(!s.apply_battery_characteristic(&[60, 70, 80]));
        assert!(feed(&mut s, "FF05 2F 03 5A 5B 5C"));
        assert_eq!((s.battery.left, s.battery.right, s.battery.case), (Some(90), Some(91), Some(92)));
    }

    #[test]
    fn firmware_versions() {
        assert_eq!(format_version(&[3, 0, 13, 3, 0, 13]).as_deref(), Some("L 3.0.13 · R 3.0.13"));
        assert_eq!(format_version(&[3, 0, 13]).as_deref(), Some("3.0.13"));
        assert_eq!(format_version(&[3, 0]), None);
        let mut s = QcyState::default();
        assert!(s.apply_firmware_characteristic(&[3, 0, 13, 3, 0, 13]));
        assert!(!s.apply_firmware_characteristic(&[3, 0, 13, 3, 0, 13]));
        assert!(!s.apply_firmware_characteristic(&[1]));
        assert!(feed(&mut s, "FF05 30 03 04 00 01"));
        assert_eq!(s.firmware_version.as_deref(), Some("4.0.1"));
    }

    #[test]
    fn equalizer_response_sets_preset_and_gains() {
        let mut s = QcyState::default();
        let packet = crate::commands::build_custom_equalizer(&[1.0; 10], 8).unwrap();
        for c in parse(&packet) {
            assert!(s.apply(&c));
        }
        assert_eq!(s.equalizer_preset, Some(8));
        assert_eq!(s.equalizer_gains, vec![1.0; 10]);
        // The legacy characteristic only knows the preset
        assert!(s.apply_equalizer_characteristic(&[3]));
        assert!(!s.apply_equalizer_characteristic(&[3]));
        assert!(!s.apply_equalizer_characteristic(&[]));
        assert_eq!(s.equalizer_preset, Some(3));
    }

    #[test]
    fn key_functions_from_both_sources() {
        let mut s = QcyState::default();
        assert!(s.apply_key_functions_characteristic(&[1, 5, 2, 7]));
        assert!(!s.apply_key_functions_characteristic(&[1, 5, 2, 7]));
        assert_eq!(s.key_functions.get(&2), Some(&7));
        // The 0x2B response replaces them
        assert!(feed(&mut s, "FF06 2B 04 01 06 03 08"));
        assert_eq!(s.key_functions.len(), 2);
        assert_eq!(s.key_functions.get(&3), Some(&8));
    }
}
