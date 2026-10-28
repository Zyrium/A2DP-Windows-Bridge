//! Typed commands and the values they carry. Opcode numbers and layouts are
//! those OpenQCY validated on N70 firmware 3.0.13 (`docs/protocol-research.md`).

use std::collections::BTreeMap;
use std::fmt;

use crate::packet::{self, REQUEST_OPCODE};

/// Opcodes of the command channel
pub mod opcode {
    /// Legacy in-ear detection toggle (older firmware)
    pub const IN_EAR_DETECTION: u8 = 0x06;
    pub const GAME_MODE: u8 = 0x09;
    pub const SLEEP_MODE: u8 = 0x10;
    pub const AUTO_POWER_OFF: u8 = 0x14;
    pub const NOISE_CONTROL: u8 = 0x17;
    pub const PROMPT_VOLUME: u8 = 0x1D;
    /// Ten-band parametric equalizer
    pub const EQUALIZER: u8 = 0x22;
    pub const LDAC: u8 = 0x23;
    pub const MULTIPOINT: u8 = 0x24;
    pub const WIND_DETECTION: u8 = 0x2A;
    pub const KEY_FUNCTIONS: u8 = 0x2B;
    /// Advanced wearing detection (N70 firmware 3.0.13)
    pub const WEARING_DETECTION: u8 = 0x2C;
    pub const BATTERY: u8 = 0x2F;
    pub const FIRMWARE: u8 = 0x30;
}

/// The queries a refresh sends after the direct reads, in OpenQCY's order
pub const REFRESH_QUERIES: [u8; 8] = [
    opcode::NOISE_CONTROL,
    opcode::GAME_MODE,
    opcode::SLEEP_MODE,
    opcode::LDAC,
    opcode::MULTIPOINT,
    opcode::WIND_DETECTION,
    opcode::PROMPT_VOLUME,
    opcode::AUTO_POWER_OFF,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseMode {
    NoiseCancellation,
    Transparency,
    Normal,
}

/// The five N70 ANC scenes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseScene {
    Adaptive,
    Indoor,
    Commuting,
    Noisy,
    AntiWind,
}

/// Opcode 0x17: three bytes. The first selects ANC / normal / transparency,
/// the others the ANC scene and its firmware parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoiseControlState {
    pub mode: NoiseMode,
    pub scene: Option<NoiseScene>,
    pub sub_scene: u8,
    pub noise_value: u8,
}

impl NoiseControlState {
    /// The state a request for `mode` (and `scene`, only for ANC) asks for
    pub fn create(mode: NoiseMode, scene: NoiseScene) -> NoiseControlState {
        let (scene, sub_scene, noise_value) = match mode {
            NoiseMode::NoiseCancellation => {
                let (sub, noise) = match scene {
                    NoiseScene::Adaptive => (0x05, 0x00),
                    NoiseScene::Indoor => (0x01, 0x02),
                    NoiseScene::Commuting => (0x02, 0x02),
                    NoiseScene::Noisy => (0x03, 0x02),
                    NoiseScene::AntiWind => (0x04, 0x00),
                };
                (Some(scene), sub, noise)
            }
            NoiseMode::Transparency => (None, 0x01, 0x04),
            NoiseMode::Normal => (None, 0x00, 0x00),
        };
        NoiseControlState { mode, scene, sub_scene, noise_value }
    }

    pub fn parse(parameters: &[u8]) -> Option<NoiseControlState> {
        let [mode, sub_scene, noise_value, ..] = *parameters else { return None };
        let (mode, scene) = match mode {
            0x01 => (
                NoiseMode::NoiseCancellation,
                match sub_scene {
                    0x01 => Some(NoiseScene::Indoor),
                    0x02 => Some(NoiseScene::Commuting),
                    0x03 => Some(NoiseScene::Noisy),
                    0x04 => Some(NoiseScene::AntiWind),
                    0x05 => Some(NoiseScene::Adaptive),
                    _ => None,
                },
            ),
            0x02 if sub_scene == 0x00 => (NoiseMode::Normal, None),
            0x03 => (NoiseMode::Transparency, None),
            _ => return None,
        };
        Some(NoiseControlState { mode, scene, sub_scene, noise_value })
    }

    pub fn to_parameters(&self) -> [u8; 3] {
        let mode = match self.mode {
            NoiseMode::NoiseCancellation => 0x01,
            NoiseMode::Normal => 0x02,
            NoiseMode::Transparency => 0x03,
        };
        [mode, self.sub_scene, self.noise_value]
    }
}

impl fmt::Display for NoiseControlState {
    /// The parameter bytes in hex, as OpenQCY words a mismatch
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.to_parameters();
        write!(f, "{a:02X}{b:02X}{c:02X}")
    }
}

/// Opcode 0x2C. The write keeps the music and ANC action bytes the device
/// reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WearingDetection {
    pub enabled: bool,
    pub music_action: u8,
    pub anc_action: u8,
    /// Present only when the firmware sends a fourth byte
    pub tone_enabled: Option<bool>,
}

impl WearingDetection {
    pub fn parse(parameters: &[u8]) -> Option<WearingDetection> {
        match *parameters {
            [state, music, anc] => Some(WearingDetection {
                enabled: state == 0x01,
                music_action: music,
                anc_action: anc,
                tone_enabled: None,
            }),
            [state, music, anc, tone] => Some(WearingDetection {
                enabled: state == 0x01,
                music_action: music,
                anc_action: anc,
                tone_enabled: Some(tone == 0x01),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqualizerBand {
    pub frequency: u16,
    /// dB
    pub gain: f64,
    pub q: f64,
    pub kind: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EqualizerState {
    pub preset_index: u8,
    /// dB
    pub master_gain: f64,
    pub bands: Vec<EqualizerBand>,
}

impl EqualizerState {
    /// Opcode 0x22 response: preset index, master gain (i16 LE, 1/100 dB),
    /// then 7 bytes per band (frequency u16, gain i16, Q u16 in 1/100, type)
    pub fn parse_v2(parameters: &[u8]) -> Option<EqualizerState> {
        if parameters.len() < 3 || !(parameters.len() - 3).is_multiple_of(7) {
            return None;
        }
        let bands = parameters[3..]
            .chunks_exact(7)
            .map(|b| EqualizerBand {
                frequency: u16::from_le_bytes([b[0], b[1]]),
                gain: f64::from(i16::from_le_bytes([b[2], b[3]])) / 100.0,
                q: f64::from(u16::from_le_bytes([b[4], b[5]])) / 100.0,
                kind: b[6],
            })
            .collect();
        Some(EqualizerState {
            preset_index: parameters[0],
            master_gain: f64::from(i16::from_le_bytes([parameters[1], parameters[2]])) / 100.0,
            bands,
        })
    }
}

/// Center frequencies of the N70's ten bands
pub const EQUALIZER_FREQUENCIES: [u16; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
/// The preset index OpenQCY writes with a custom curve
pub const CUSTOM_EQUALIZER_PRESET: u8 = 8;
/// Gain limit of a band, dB
pub const EQUALIZER_MAX_GAIN: f64 = 8.0;

/// A request for `opcode`'s current state
pub fn request(opcode: u8) -> Vec<u8> {
    packet::frame(REQUEST_OPCODE, &[opcode])
}

fn toggle(opcode: u8, enabled: bool) -> Vec<u8> {
    packet::frame(opcode, &[if enabled { 0x01 } else { 0x02 }])
}

pub fn set_in_ear_detection(enabled: bool) -> Vec<u8> {
    toggle(opcode::IN_EAR_DETECTION, enabled)
}

pub fn set_wearing_detection(enabled: bool, current: &WearingDetection) -> Vec<u8> {
    let state = if enabled { 0x01 } else { 0x02 };
    match current.tone_enabled {
        Some(tone) => packet::frame(
            opcode::WEARING_DETECTION,
            &[state, current.music_action, current.anc_action, if tone { 0x01 } else { 0x02 }],
        ),
        None => packet::frame(opcode::WEARING_DETECTION, &[state, current.music_action, current.anc_action]),
    }
}

pub fn set_noise_mode(mode: NoiseMode, scene: NoiseScene) -> Vec<u8> {
    packet::frame(opcode::NOISE_CONTROL, &NoiseControlState::create(mode, scene).to_parameters())
}

pub fn set_game_mode(enabled: bool) -> Vec<u8> {
    toggle(opcode::GAME_MODE, enabled)
}

pub fn set_sleep_mode(enabled: bool) -> Vec<u8> {
    toggle(opcode::SLEEP_MODE, enabled)
}

pub fn set_ldac(enabled: bool) -> Vec<u8> {
    toggle(opcode::LDAC, enabled)
}

pub fn set_multipoint(enabled: bool) -> Vec<u8> {
    toggle(opcode::MULTIPOINT, enabled)
}

pub fn set_wind_detection(enabled: bool) -> Vec<u8> {
    toggle(opcode::WIND_DETECTION, enabled)
}

pub fn set_prompt_volume(value: u8) -> Vec<u8> {
    packet::frame(opcode::PROMPT_VOLUME, &[value])
}

/// 0xFFFF minutes means never. Four parameter bytes: the minutes (u16 LE)
/// and two zero bytes, the shape the device reports.
pub fn set_auto_power_off(minutes: u16) -> Vec<u8> {
    let m = minutes.to_le_bytes();
    packet::frame(opcode::AUTO_POWER_OFF, &[m[0], m[1], 0, 0])
}

/// Value to write to characteristic 000B (firmware that has it); N70
/// firmware 3.0.13 does not
pub fn set_equalizer_preset(preset_index: u8) -> Vec<u8> {
    vec![preset_index]
}

/// Value to write to characteristic 000D: (key, function) pairs ordered by
/// key
pub fn set_key_functions(mappings: &BTreeMap<u8, u8>) -> Vec<u8> {
    mappings.iter().flat_map(|(key, function)| [*key, *function]).collect()
}

pub fn parse_key_functions(value: &[u8]) -> BTreeMap<u8, u8> {
    if !value.len().is_multiple_of(2) {
        return BTreeMap::new();
    }
    value.chunks_exact(2).map(|pair| (pair[0], pair[1])).collect()
}

/// The N70 equalizer needs exactly ten gains
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrongBandCount {
    pub got: usize,
}

impl fmt::Display for WrongBandCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the N70 equalizer requires exactly {} bands, got {}", EQUALIZER_FREQUENCIES.len(), self.got)
    }
}

impl std::error::Error for WrongBandCount {}

/// Opcode 0x22 with a custom ten-band curve: gains in dB, each limited to
/// +-8 dB and rounded to 1/100 dB (halves to even, as OpenQCY's
/// `Math.Round`), master gain 0, Q 1.00, band type 0
pub fn build_custom_equalizer(gains: &[f64], preset_index: u8) -> Result<Vec<u8>, WrongBandCount> {
    if gains.len() != EQUALIZER_FREQUENCIES.len() {
        return Err(WrongBandCount { got: gains.len() });
    }
    let mut parameters = Vec::with_capacity(3 + EQUALIZER_FREQUENCIES.len() * 7);
    parameters.push(preset_index);
    // Master gain, 0.00 dB
    parameters.extend([0, 0]);
    for (frequency, gain) in EQUALIZER_FREQUENCIES.iter().zip(gains) {
        let hundredths = (gain * 100.0).round_ties_even().clamp(-800.0, 800.0) as i16;
        parameters.extend(frequency.to_le_bytes());
        parameters.extend(hundredths.to_le_bytes());
        // Q = 1.00
        parameters.extend(100u16.to_le_bytes());
        parameters.push(0);
    }
    Ok(packet::frame(opcode::EQUALIZER, &parameters))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::parse;
    use crate::test_util::hex;

    #[test]
    fn requests_of_the_transcript() {
        assert_eq!(request(opcode::WEARING_DETECTION), hex("FF03FE012C"));
        assert_eq!(request(opcode::NOISE_CONTROL), hex("FF03FE0117"));
        assert_eq!(request(opcode::GAME_MODE), hex("FF03FE0109"));
        assert_eq!(request(opcode::SLEEP_MODE), hex("FF03FE0110"));
        assert_eq!(request(opcode::LDAC), hex("FF03FE0123"));
        assert_eq!(request(opcode::MULTIPOINT), hex("FF03FE0124"));
        assert_eq!(request(opcode::WIND_DETECTION), hex("FF03FE012A"));
        assert_eq!(request(opcode::PROMPT_VOLUME), hex("FF03FE011D"));
        assert_eq!(request(opcode::AUTO_POWER_OFF), hex("FF03FE0114"));
    }

    #[test]
    fn anc_scenes_of_the_transcript() {
        let write = |scene| set_noise_mode(NoiseMode::NoiseCancellation, scene);
        assert_eq!(write(NoiseScene::Adaptive), hex("FF051703010500"));
        assert_eq!(write(NoiseScene::Indoor), hex("FF051703010102"));
        assert_eq!(write(NoiseScene::Commuting), hex("FF051703010202"));
        assert_eq!(write(NoiseScene::Noisy), hex("FF051703010302"));
        assert_eq!(write(NoiseScene::AntiWind), hex("FF051703010400"));
    }

    #[test]
    fn noise_state_round_trips_and_confirms() {
        for scene in
            [NoiseScene::Adaptive, NoiseScene::Indoor, NoiseScene::Commuting, NoiseScene::Noisy, NoiseScene::AntiWind]
        {
            let want = NoiseControlState::create(NoiseMode::NoiseCancellation, scene);
            assert_eq!(NoiseControlState::parse(&want.to_parameters()), Some(want));
        }
        // The ANC response of the transcript is the "noisy" scene
        let got = NoiseControlState::parse(&[1, 3, 2]).unwrap();
        assert_eq!(got, NoiseControlState::create(NoiseMode::NoiseCancellation, NoiseScene::Noisy));
        // Normal and transparency
        let normal = NoiseControlState::create(NoiseMode::Normal, NoiseScene::Adaptive);
        assert_eq!(normal.to_parameters(), [0x02, 0x00, 0x00]);
        assert_eq!(NoiseControlState::parse(&[2, 0, 0]), Some(normal));
        let transparency = NoiseControlState::create(NoiseMode::Transparency, NoiseScene::Adaptive);
        assert_eq!(transparency.to_parameters(), [0x03, 0x01, 0x04]);
        assert_eq!(NoiseControlState::parse(&[3, 1, 4]), Some(transparency));
        // A different scene byte is a mismatch
        assert_ne!(NoiseControlState::parse(&[1, 1, 2]), Some(got));
    }

    #[test]
    fn noise_state_rejects_garbage() {
        assert_eq!(NoiseControlState::parse(&[1, 3]), None);
        assert_eq!(NoiseControlState::parse(&[9, 0, 0]), None);
        // Normal mode only with sub-scene 0
        assert_eq!(NoiseControlState::parse(&[2, 1, 0]), None);
        // An unknown ANC scene keeps the mode and loses the scene
        let s = NoiseControlState::parse(&[1, 9, 0]).unwrap();
        assert_eq!((s.mode, s.scene), (NoiseMode::NoiseCancellation, None));
    }

    #[test]
    fn wearing_detection_of_the_transcript() {
        // Off: FF052C03000101
        let off = WearingDetection::parse(&parse(&hex("FF052C03000101"))[0].parameters).unwrap();
        assert_eq!(off, WearingDetection { enabled: false, music_action: 1, anc_action: 1, tone_enabled: None });
        assert_eq!(set_wearing_detection(true, &off), hex("FF052C03010101"));
        assert_eq!(set_wearing_detection(false, &off), hex("FF052C03020101"));
    }

    #[test]
    fn wearing_detection_with_a_tone_byte() {
        let w = WearingDetection::parse(&[1, 2, 3, 1]).unwrap();
        assert_eq!(w.tone_enabled, Some(true));
        assert_eq!(set_wearing_detection(true, &w), hex("FF062C0401020301"));
        let w = WearingDetection { tone_enabled: Some(false), ..w };
        assert_eq!(set_wearing_detection(false, &w), hex("FF062C0402020302"));
        assert_eq!(WearingDetection::parse(&[1, 2]), None);
        assert_eq!(WearingDetection::parse(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn toggles() {
        assert_eq!(set_ldac(true), hex("FF03230101"));
        assert_eq!(set_ldac(false), hex("FF03230102"));
        assert_eq!(set_game_mode(false), hex("FF03090102"));
        assert_eq!(set_sleep_mode(true), hex("FF03100101"));
        assert_eq!(set_multipoint(true), hex("FF03240101"));
        assert_eq!(set_wind_detection(true), hex("FF032A0101"));
        assert_eq!(set_in_ear_detection(true), hex("FF03060101"));
    }

    #[test]
    fn prompt_volume_and_auto_power_off() {
        assert_eq!(set_prompt_volume(4), hex("FF03 1D 01 04"));
        // "Never" has the shape of the transcript's response
        assert_eq!(set_auto_power_off(0xFFFF), hex("FF061404FFFF0000"));
        assert_eq!(set_auto_power_off(30), hex("FF0614041E000000"));
    }

    #[test]
    fn custom_equalizer_layout() {
        let gains = [0.0, 1.5, -2.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 8.0];
        let packet = build_custom_equalizer(&gains, CUSTOM_EQUALIZER_PRESET).unwrap();
        let cmds = parse(&packet);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].opcode, opcode::EQUALIZER);
        let p = &cmds[0].parameters;
        assert_eq!(p.len(), 3 + 10 * 7);
        assert_eq!(&p[..3], &[8, 0, 0]);
        let eq = EqualizerState::parse_v2(p).unwrap();
        assert_eq!(eq.preset_index, 8);
        assert_eq!(eq.master_gain, 0.0);
        assert_eq!(eq.bands.len(), 10);
        let freqs: Vec<u16> = eq.bands.iter().map(|b| b.frequency).collect();
        assert_eq!(freqs, EQUALIZER_FREQUENCIES);
        let got: Vec<f64> = eq.bands.iter().map(|b| b.gain).collect();
        assert_eq!(got, gains);
        assert!(eq.bands.iter().all(|b| b.q == 1.0 && b.kind == 0));
    }

    #[test]
    fn custom_equalizer_clamps_and_rounds() {
        let mut gains = [0.0; 10];
        gains[0] = 9.0;
        gains[1] = -9.0;
        gains[2] = 1.234;
        let p = parse(&build_custom_equalizer(&gains, 8).unwrap())[0].parameters.clone();
        let eq = EqualizerState::parse_v2(&p).unwrap();
        assert_eq!(eq.bands[0].gain, 8.0);
        assert_eq!(eq.bands[1].gain, -8.0);
        assert_eq!(eq.bands[2].gain, 1.23);
    }

    #[test]
    fn custom_equalizer_needs_ten_bands() {
        assert_eq!(build_custom_equalizer(&[0.0; 9], 8), Err(WrongBandCount { got: 9 }));
        assert_eq!(build_custom_equalizer(&[0.0; 11], 8), Err(WrongBandCount { got: 11 }));
    }

    #[test]
    fn equalizer_response_must_fit_the_band_size() {
        assert_eq!(EqualizerState::parse_v2(&[8, 0]), None);
        assert_eq!(EqualizerState::parse_v2(&[8, 0, 0, 1, 2, 3]), None);
        assert_eq!(EqualizerState::parse_v2(&[8, 0, 0]).unwrap().bands.len(), 0);
    }

    #[test]
    fn key_functions() {
        let map = parse_key_functions(&[2, 7, 1, 5, 3, 9]);
        assert_eq!(map.get(&1), Some(&5));
        // Written ordered by key
        assert_eq!(set_key_functions(&map), [1, 5, 2, 7, 3, 9]);
        // An odd length is not a mapping
        assert!(parse_key_functions(&[1, 2, 3]).is_empty());
        assert!(parse_key_functions(&[]).is_empty());
    }

    #[test]
    fn equalizer_preset_value() {
        assert_eq!(set_equalizer_preset(3), [3]);
    }
}
