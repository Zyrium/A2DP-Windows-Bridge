//! profiles.json in v1's format: {"profiles": [ ... ]}. Unknown keys of a
//! profile are kept.
//!
//! Since the Codecs screen (issue #86) the UI keeps one profile per device
//! entry (address, and whether it is the LE Audio entry): choosing a codec
//! card changes that profile. The settings of the codecs it is not using
//! are kept in its `codec_settings`; [`consolidate`] folds the profiles of
//! earlier versions into one per device.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use bt_host::HeadsetMic;
use bt_host::media::clamp_media_payload_limit;
use codec::{CodecKind, Quality};

use crate::media_eq::MediaEq;
use crate::stream::{Bitpools, SourceRequest, StreamRequest, VolumeSync};

fn auto() -> String {
    "auto".into()
}
fn hq() -> String {
    "hq".into()
}
fn loopback() -> String {
    "loopback".into()
}
fn yes() -> bool {
    true
}
fn headset_off() -> String {
    "off".into()
}
fn is_headset_off(mode: &str) -> bool {
    HeadsetMic::parse(mode) == HeadsetMic::Off
}
/// `headset_mic` as saved: "off" | "on" | "auto", or the bool of the first
/// development builds (true: "on"); anything else is "off"
fn headset_mode<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Flag(bool),
        Mode(String),
        Other(serde::de::IgnoredAny),
    }
    let mode = match Saved::deserialize(d)? {
        Saved::Flag(on) => {
            if on {
                HeadsetMic::On
            } else {
                HeadsetMic::Off
            }
        }
        Saved::Mode(m) => HeadsetMic::parse(&m),
        Saved::Other(_) => HeadsetMic::Off,
    };
    Ok(mode.as_str().into())
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u8) -> bool {
    *n == 0
}
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// XX:XX:XX:XX:XX:XX
    pub device_address: String,
    pub device_name: String,
    /// auto, ldac, aptxhd, aptxll, aptx, aptxad, sbcxq, sbc, aac, lhdcv5, opus, opus05, faststream
    #[serde(default = "auto")]
    pub codec: String,
    /// hq, sq, mq; mhq for LHDC V5 only; ll for aptX Adaptive only
    #[serde(default = "hq")]
    pub quality: String,
    pub abr: bool,
    /// LHDC V5: low latency asked for (issue #543)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lhdcv5_low_latency: bool,
    /// Legacy (v1, and v2 before issue #86's Codecs redesign): whether a
    /// codec the device does not offer falls back to another. Kept in the
    /// file but no longer used: a profile falls back to the first codec in
    /// its order the device offers, unless `codec_forced`
    #[serde(default = "yes")]
    pub codec_fallback: bool,
    /// The codec was chosen on its card while the device did not offer it
    /// (issue #86): it is asked for anyway and the stream stops with an
    /// error if the device refuses it. A codec chosen while offered and no
    /// longer offered later (earbuds switched to another codec) falls back.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub codec_forced: bool,
    /// The codec's settings are not held to what the device lists (its
    /// card's "Show only supported options" off, issue #86)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unlisted_options: bool,
    /// 0 = auto
    pub sample_rate: u32,
    /// 0 = auto, 16, 24
    pub bit_depth: u32,
    /// loopback, virtual, output, app. "output": loopback of the output
    /// device `audio_device_id` (Windows' endpoint ID) whichever is the
    /// default (issue #193)
    #[serde(default = "loopback")]
    pub capture_mode: String,
    pub audio_device_id: String,
    pub audio_device_name: String,
    /// Virtual device mode: make the device the default output while
    /// streaming
    #[serde(default = "yes")]
    pub auto_switch_device: bool,
    pub app_exe: String,
    pub app_name: String,
    /// auto, off, device. Kept as saved, but streams follow
    /// Settings::volume_sync alone (schema 6; issue #86: no screen sets a
    /// followed device any more): off, none syncs; on, Auto (the core's
    /// state.rs)
    #[serde(default = "auto")]
    pub volume_sync: String,
    pub volume_device_id: String,
    pub volume_device_name: String,
    /// Max media packet size (None in files from before v1.1: the legacy
    /// setting applies)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_media_payload: Option<u16>,
    /// SBC / SBC XQ: the bitpool to encode with, within what the device
    /// allows (0 = by quality)
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_bitpool: u8,
    /// The same for SBC XQ (kept apart: XQ is dual channel and goes lower)
    #[serde(skip_serializing_if = "is_zero")]
    pub sbcxq_bitpool: u8,
    /// SBC / SBC XQ: the lowest bitpool to use, the SET_CONFIGURATION's
    /// minimum and the floor of ABR, within what the device allows (0 =
    /// Auto, issue #86)
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_min_bitpool: u8,
    /// The same for SBC XQ
    #[serde(skip_serializing_if = "is_zero")]
    pub sbcxq_min_bitpool: u8,
    /// SBC / SBC XQ: block length 4, 8, 12, 16 (0 = 16). The frame setup
    /// counts in developer mode only ([`crate::settings::Settings::sbc_frame`])
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_blocks: u8,
    /// SBC / SBC XQ: subbands 4, 8 (0 = 8)
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_subbands: u8,
    /// SBC / SBC XQ: loudness, snr ("" = loudness)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sbc_allocation: String,
    /// SBC: joint_stereo, stereo, mono ("" = joint stereo; dual channel is
    /// SBC XQ's, which is always dual channel)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sbc_channel_mode: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub le_audio: bool,
    /// LE Audio: GMAP low-latency presets with a gaming headset (the LC3 LL
    /// card); other headsets get the media configuration either way. Off by
    /// default since issue #86 (LC3), written only when on: before, on was
    /// the default and only off was written, so a profile without the key
    /// that had LC3 LL now has LC3.
    #[serde(default, skip_serializing_if = "is_false")]
    pub le_low_latency: bool,
    /// Headset mode: "off" | "on" | "auto" ([`HeadsetMic`]; written only
    /// when not off). The headphones' microphone is rendered to
    /// `mic_device_id`; classic devices play over the voice link meanwhile
    /// (Auto: while an app records from its capture side), LE Audio keeps
    /// its preset
    #[serde(default = "headset_off", deserialize_with = "headset_mode", skip_serializing_if = "is_headset_off")]
    pub headset_mic: String,
    /// The render endpoint id that receives the microphone (a virtual
    /// cable's input). Empty: none chosen, headset mode stays off
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mic_device_id: String,
    /// Its friendly name, for display while the device is missing
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mic_device_name: String,
    /// The microphone muted from Home's or the flyout's chip: the voice link
    /// stays closed as with headset mode off, `headset_mic` is kept for
    /// unmuting (written only when muted)
    #[serde(default, skip_serializing_if = "is_false")]
    pub mic_muted: bool,
    /// The microphone output's recording side ("CABLE Output") is Windows'
    /// default input while the device is connected (on by default, written
    /// only when off; default_input.rs)
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub mic_default_input: bool,
    /// Classic devices: the call codec chosen with Auto-select off
    /// ("cvsd", "msbc", "lc3_swb"; "" = none, written only when chosen):
    /// the best the voice link may use ([`Profile::voice_codec_in_force`])
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice_codec: String,
    /// LE Audio: the microphone's sampling rate chosen with Auto-select off
    /// (Hz; 0 = none, written only when chosen)
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub mic_rate: u32,
    /// The microphone's gain, noise suppression and gate (written only when
    /// not all off); a change applies to the running microphone in place
    #[serde(default, skip_serializing_if = "ipc::MicProcessing::is_default")]
    pub mic_processing: ipc::MicProcessing,
    /// The settings of the other codecs, by codec key, as they were last
    /// chosen: choosing one of them again brings them back (the codec in
    /// use has its own in the fields above)
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub codec_settings: BTreeMap<String, CodecValues>,
    /// The device's codec order (codec keys), as its cards were dragged on
    /// the Codecs screen (issue #86 round 8): Auto, under Auto-select too,
    /// and a chosen codec the device does not offer, take the first one in
    /// it the device offers. Empty: highest sound quality first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub codec_order: Vec<String>,
    /// The playback EQ for this device (written only when on or with
    /// points)
    #[serde(skip_serializing_if = "MediaEq::is_default")]
    pub media_eq: MediaEq,
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: String::new(),
            device_address: String::new(),
            device_name: String::new(),
            codec: auto(),
            quality: hq(),
            abr: false,
            lhdcv5_low_latency: false,
            codec_fallback: true,
            codec_forced: false,
            unlisted_options: false,
            sample_rate: 0,
            bit_depth: 0,
            capture_mode: loopback(),
            audio_device_id: String::new(),
            audio_device_name: String::new(),
            auto_switch_device: true,
            app_exe: String::new(),
            app_name: String::new(),
            volume_sync: auto(),
            volume_device_id: String::new(),
            volume_device_name: String::new(),
            max_media_payload: None,
            sbc_bitpool: 0,
            sbcxq_bitpool: 0,
            sbc_min_bitpool: 0,
            sbcxq_min_bitpool: 0,
            sbc_blocks: 0,
            sbc_subbands: 0,
            sbc_allocation: String::new(),
            sbc_channel_mode: String::new(),
            le_audio: false,
            le_low_latency: false,
            headset_mic: headset_off(),
            mic_device_id: String::new(),
            mic_device_name: String::new(),
            mic_muted: false,
            mic_default_input: true,
            voice_codec: String::new(),
            mic_rate: 0,
            mic_processing: ipc::MicProcessing::default(),
            codec_settings: BTreeMap::new(),
            codec_order: Vec::new(),
            media_eq: MediaEq::default(),
            other: BTreeMap::new(),
        }
    }
}

/// One codec's settings in a profile (what its "..." on the Codecs screen
/// sets)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CodecValues {
    pub quality: String,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub abr: bool,
    /// LHDC V5: low latency asked for ([`Profile::lhdcv5_low_latency`])
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lhdcv5_low_latency: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_bitpool: u8,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbcxq_bitpool: u8,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_min_bitpool: u8,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbcxq_min_bitpool: u8,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_blocks: u8,
    #[serde(skip_serializing_if = "is_zero")]
    pub sbc_subbands: u8,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sbc_allocation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sbc_channel_mode: String,
    /// Not held to what the device lists ([`Profile::unlisted_options`])
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unlisted_options: bool,
}

impl CodecValues {
    /// Puts these settings into `req` (one codec's, under Auto-select)
    pub fn apply_to(&self, req: &mut StreamRequest) {
        req.quality = Quality::from_key(&self.quality);
        req.abr = self.abr;
        req.lhdcv5_low_latency = self.lhdcv5_low_latency;
        req.sbc_bitpool = self.sbc_bitpool;
        req.sbcxq_bitpool = self.sbcxq_bitpool;
        req.sbc_min_bitpool = self.sbc_min_bitpool;
        req.sbcxq_min_bitpool = self.sbcxq_min_bitpool;
        req.sbc = codec::sbc::SbcParams {
            blocks: self.sbc_blocks,
            subbands: self.sbc_subbands,
            allocation: codec::sbc::Allocation::from_key(&self.sbc_allocation),
            channel_mode: codec::sbc::ChannelMode::from_key(&self.sbc_channel_mode),
            min_bitpool: 0,
        };
        req.sample_rate = (self.sample_rate != 0).then_some(self.sample_rate);
        req.bit_depth = match self.bit_depth {
            16 => Some(16),
            24 => Some(24),
            _ => None,
        };
    }
}

impl Default for CodecValues {
    fn default() -> Self {
        CodecValues {
            quality: hq(),
            sample_rate: 0,
            bit_depth: 0,
            abr: false,
            lhdcv5_low_latency: false,
            sbc_bitpool: 0,
            sbcxq_bitpool: 0,
            sbc_min_bitpool: 0,
            sbcxq_min_bitpool: 0,
            sbc_blocks: 0,
            sbc_subbands: 0,
            sbc_allocation: String::new(),
            sbc_channel_mode: String::new(),
            unlisted_options: false,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("invalid Bluetooth address \"{0}\"")]
    Address(String),
}

/// The Codecs screen's card of aptX Adaptive's low latency mode (issue #86):
/// saved as codec "aptxad" with quality "ll", its settings kept apart from
/// aptX Adaptive's under this key
pub const APTXAD_LL_CARD: &str = "aptxadll";

/// The card a codec and quality show on, the key of its settings in
/// `codec_settings`: aptX Adaptive in Low Latency is [`APTXAD_LL_CARD`]
pub fn card_key<'a>(codec: &'a str, quality: &str) -> &'a str {
    if codec == "aptxad" && quality == "ll" { APTXAD_LL_CARD } else { codec }
}

impl Profile {
    /// The card of the codec in use ([`card_key`])
    pub fn card(&self) -> &str {
        card_key(&self.codec, &self.quality)
    }

    /// Settings an earlier version kept for aptX Adaptive in Low Latency
    /// (`codec_settings["aptxad"]` with quality "ll", before aptX Adaptive LL
    /// had a card of its own) go to that card's key, unless it has its own
    /// already (then they are dropped: they are not aptX Adaptive's).
    /// Returns whether anything moved.
    pub fn separate_card_settings(&mut self) -> bool {
        let legacy = self.codec_settings.get("aptxad").is_some_and(|v| v.quality == "ll");
        if !legacy {
            return false;
        }
        let v = self.codec_settings.remove("aptxad").unwrap_or_default();
        if self.card() != APTXAD_LL_CARD {
            self.codec_settings.entry(APTXAD_LL_CARD.into()).or_insert(v);
        }
        true
    }

    /// The settings of the codec in use
    pub fn codec_values(&self) -> CodecValues {
        CodecValues {
            quality: self.quality.clone(),
            sample_rate: self.sample_rate,
            bit_depth: self.bit_depth,
            abr: self.abr,
            lhdcv5_low_latency: self.lhdcv5_low_latency,
            sbc_bitpool: self.sbc_bitpool,
            sbcxq_bitpool: self.sbcxq_bitpool,
            sbc_min_bitpool: self.sbc_min_bitpool,
            sbcxq_min_bitpool: self.sbcxq_min_bitpool,
            sbc_blocks: self.sbc_blocks,
            sbc_subbands: self.sbc_subbands,
            sbc_allocation: self.sbc_allocation.clone(),
            sbc_channel_mode: self.sbc_channel_mode.clone(),
            unlisted_options: self.unlisted_options,
        }
    }

    /// Each codec's settings: the codec in use's, then the others' kept
    /// ones (what Auto-select streams with in developer mode,
    /// `DevOverrides::auto_codec_settings`). Auto picks aptX Adaptive, never
    /// its LL card: aptX Adaptive's are its card's, also while LL is chosen.
    pub fn all_codec_values(&self) -> Vec<(CodecKind, CodecValues)> {
        // None for aptX Adaptive LL's card, which is no codec of Auto's
        let current = CodecKind::from_key(self.card());
        let kept = self
            .codec_settings
            .iter()
            // An earlier version's aptX Adaptive in Low Latency is LL's
            .filter(|(key, v)| card_key(key, &v.quality) == key.as_str())
            .filter_map(|(key, v)| Some((CodecKind::from_key(key)?, v.clone())));
        current.map(|k| (k, self.codec_values())).into_iter().chain(kept.filter(|(k, _)| Some(*k) != current)).collect()
    }

    /// The profile as Auto streams it while "Auto-select" is on for every
    /// device (settings `auto_codec`): codec "auto" with Auto's default
    /// settings, as turning Auto on for one device gave before. The codec
    /// chosen and its settings stay in the saved profile. An LE Audio
    /// profile (LC3) is returned as it is.
    pub fn as_auto(&self) -> Profile {
        if self.le_audio {
            return self.clone();
        }
        let d = CodecValues::default();
        Profile {
            codec: auto(),
            quality: d.quality,
            sample_rate: d.sample_rate,
            bit_depth: d.bit_depth,
            abr: d.abr,
            lhdcv5_low_latency: d.lhdcv5_low_latency,
            sbc_bitpool: d.sbc_bitpool,
            sbcxq_bitpool: d.sbcxq_bitpool,
            sbc_min_bitpool: d.sbc_min_bitpool,
            sbcxq_min_bitpool: d.sbcxq_min_bitpool,
            sbc_blocks: d.sbc_blocks,
            sbc_subbands: d.sbc_subbands,
            sbc_allocation: d.sbc_allocation,
            sbc_channel_mode: d.sbc_channel_mode,
            unlisted_options: false,
            codec_forced: false,
            // The device's own order stays: Auto takes the first card offered
            ..self.clone()
        }
    }

    /// The codec order streams use ([`Profile::codec_order`])
    pub fn order(&self) -> codec::AutoOrder {
        codec::AutoOrder::from_keys(&self.codec_order)
    }

    /// Where the profile's audio comes from
    pub fn source(&self) -> SourceRequest {
        match self.capture_mode.as_str() {
            "virtual" => SourceRequest::VirtualDevice {
                id: self.audio_device_id.clone(),
                name: self.audio_device_name.clone(),
                auto_switch: self.auto_switch_device,
            },
            "output" if !self.audio_device_id.is_empty() => SourceRequest::OutputDevice {
                id: self.audio_device_id.clone(),
                name: self.audio_device_name.clone(),
                auto_switch: self.auto_switch_device,
            },
            "app" => SourceRequest::Application { exe: self.app_exe.clone() },
            // "loopback", "output" without a device (none chosen: the
            // default output), and v1's legacy "apo"
            _ => SourceRequest::SystemLoopback,
        }
    }

    /// `new` is this profile with another source and nothing else changed
    pub fn only_source_differs(&self, new: &Profile) -> bool {
        let same_source = Profile {
            capture_mode: new.capture_mode.clone(),
            audio_device_id: new.audio_device_id.clone(),
            audio_device_name: new.audio_device_name.clone(),
            auto_switch_device: new.auto_switch_device,
            app_exe: new.app_exe.clone(),
            app_name: new.app_name.clone(),
            ..self.clone()
        };
        same_source == *new && self.source() != new.source()
    }

    /// `new` is this profile with only the settings kept for other codecs
    /// changed: nothing a stream uses
    pub fn only_kept_settings_differ(&self, new: &Profile) -> bool {
        self.codec_settings != new.codec_settings
            && Profile { codec_settings: new.codec_settings.clone(), ..self.clone() } == *new
    }

    /// The SBC / SBC XQ bitpools, which a stream takes in place
    pub fn bitpools(&self) -> Bitpools {
        Bitpools {
            sbc: self.sbc_bitpool,
            sbcxq: self.sbcxq_bitpool,
            sbc_min: self.sbc_min_bitpool,
            sbcxq_min: self.sbcxq_min_bitpool,
        }
    }

    /// `new` is this profile with other bitpools and nothing else changed
    pub fn only_bitpools_differ(&self, new: &Profile) -> bool {
        let same = Profile {
            sbc_bitpool: new.sbc_bitpool,
            sbcxq_bitpool: new.sbcxq_bitpool,
            sbc_min_bitpool: new.sbc_min_bitpool,
            sbcxq_min_bitpool: new.sbcxq_min_bitpool,
            ..self.clone()
        };
        same == *new && self.bitpools() != new.bitpools()
    }

    /// `new` is this profile with another headset mode, microphone output,
    /// mute or microphone codec (call codec, LE Audio microphone rate) and
    /// nothing else changed
    pub fn only_headset_mic_differs(&self, new: &Profile) -> bool {
        let same = Profile {
            headset_mic: new.headset_mic.clone(),
            mic_device_id: new.mic_device_id.clone(),
            mic_device_name: new.mic_device_name.clone(),
            mic_muted: new.mic_muted,
            voice_codec: new.voice_codec.clone(),
            mic_rate: new.mic_rate,
            ..self.clone()
        };
        // Only those fields left to differ, and they do
        same == *new && *self != *new
    }

    /// `new` is this profile with another playback EQ and nothing else
    /// changed: the stream goes on and takes it in place
    pub fn only_media_eq_differs(&self, new: &Profile) -> bool {
        self.media_eq != new.media_eq && Profile { media_eq: new.media_eq.clone(), ..self.clone() } == *new
    }

    /// `new` is this profile with other microphone processing and nothing
    /// else changed: the running microphone takes it in place
    pub fn only_mic_processing_differs(&self, new: &Profile) -> bool {
        self.mic_processing != new.mic_processing
            && Profile { mic_processing: new.mic_processing, ..self.clone() } == *new
    }

    /// `new` differs only in what a running stream takes in place: the
    /// playback EQ, the microphone processing, or both
    pub fn only_live_processing_differs(&self, new: &Profile) -> bool {
        *self != *new
            && Profile { media_eq: new.media_eq.clone(), mic_processing: new.mic_processing, ..self.clone() } == *new
    }

    /// `new` is this profile with "Set as Windows default input while
    /// connected" turned and nothing else changed: the stream goes on (the
    /// core's state follows it, default_input.rs)
    pub fn only_default_input_differs(&self, new: &Profile) -> bool {
        self.mic_default_input != new.mic_default_input
            && Profile { mic_default_input: new.mic_default_input, ..self.clone() } == *new
    }

    /// The headset mode as saved, Off while the microphone is muted
    pub fn headset_mic_mode(&self) -> HeadsetMic {
        if self.mic_muted { HeadsetMic::Off } else { HeadsetMic::parse(&self.headset_mic) }
    }

    /// The call codec chosen, while Auto-select (`auto_select`) is off:
    /// the best the voice link may use. None: Auto (also for an LE Audio
    /// profile, or a name not known).
    pub fn voice_codec_in_force(&self, auto_select: bool) -> Option<voice_link::VoiceCodec> {
        if auto_select || self.le_audio { None } else { voice_link::VoiceCodec::from_name(&self.voice_codec) }
    }

    /// LE Audio: the microphone's rate chosen, while Auto-select is off.
    /// None: Auto.
    pub fn mic_rate_in_force(&self, auto_select: bool) -> Option<u32> {
        (!auto_select && self.le_audio && self.mic_rate != 0).then_some(self.mic_rate)
    }

    /// Why headset mode must not run with this profile's microphone output:
    /// it would feed the microphone back into the stream. The output is the
    /// virtual or output device the profile captures, or the Windows
    /// default output (`default_output_id`) while it captures System
    /// Loopback. Returns the message key, None when there is no loop (or no
    /// output chosen, which the UI notes on its own).
    pub fn mic_device_conflicts(&self, default_output_id: &str) -> Option<&'static str> {
        mic_output_conflicts(&self.source(), &self.mic_device_id, default_output_id)
    }

    /// `new` is this profile with only "Show only options it supports"
    /// turned
    pub fn only_unlisted_differs(&self, new: &Profile) -> bool {
        self.unlisted_options != new.unlisted_options
            && Profile { unlisted_options: new.unlisted_options, ..self.clone() } == *new
    }

    /// What to stream for this profile. An LE Audio (experimental) profile
    /// gives an LE request (`le` Some: the le_stream session, which refuses
    /// with error.le_audio_disabled while the setting is off); its codec,
    /// quality, rate and depth are not used (LC3 as the device and the
    /// preset decide).
    pub fn to_request(&self, legacy_max_media_payload: u16) -> Result<StreamRequest, ProfileError> {
        let device = bt_host::parse_addr(&self.device_address)
            .ok_or_else(|| ProfileError::Address(self.device_address.clone()))?;
        let source = self.source();
        let volume_sync = match self.volume_sync.as_str() {
            "off" => VolumeSync::Off,
            "device" => VolumeSync::Device(self.volume_device_id.clone()),
            _ => VolumeSync::Auto,
        };
        tracing::debug!(
            "profile \"{}\": codec {:?} quality {:?} rate {} depth {} capture {:?} volume sync {:?} payload {:?} (legacy {legacy_max_media_payload}) LE {}",
            self.name,
            self.codec,
            self.quality,
            self.sample_rate,
            self.bit_depth,
            self.capture_mode,
            self.volume_sync,
            self.max_media_payload,
            self.le_audio
        );
        Ok(StreamRequest {
            device,
            device_name: self.device_name.clone(),
            codec: CodecKind::from_key(&self.codec),
            codec_fallback: true,
            codec_forced: self.codec_forced,
            unlisted_options: self.unlisted_options,
            // Settings' developer_mode, filled in by whoever starts the stream
            developer_codecs: false,
            auto_order: self.order(),
            quality: Quality::from_key(&self.quality),
            abr: self.abr,
            lhdcv5_low_latency: self.lhdcv5_low_latency,
            sbc_bitpool: self.sbc_bitpool,
            sbcxq_bitpool: self.sbcxq_bitpool,
            sbc_min_bitpool: self.sbc_min_bitpool,
            sbcxq_min_bitpool: self.sbcxq_min_bitpool,
            // Whoever starts the stream passes it through Settings::sbc_frame
            sbc: codec::sbc::SbcParams {
                blocks: self.sbc_blocks,
                subbands: self.sbc_subbands,
                allocation: codec::sbc::Allocation::from_key(&self.sbc_allocation),
                channel_mode: codec::sbc::ChannelMode::from_key(&self.sbc_channel_mode),
                min_bitpool: 0,
            },
            sample_rate: (self.sample_rate != 0).then_some(self.sample_rate),
            bit_depth: match self.bit_depth {
                16 => Some(16),
                24 => Some(24),
                _ => None,
            },
            codec_values: Vec::new(),
            source,
            volume_sync,
            mute_output: false,
            max_media_payload: clamp_media_payload_limit(
                self.max_media_payload.unwrap_or(legacy_max_media_payload) as u32
            ),
            le: self
                .le_audio
                .then(|| bt_host::LeRequest { low_latency: self.le_low_latency, ..bt_host::LeRequest::default() }),
            // Whoever starts the stream decides (an output chosen, the
            // app's start_profile)
            headset_mic: HeadsetMic::Off,
            mic_output: None,
            voice_codec: None,
            mic_processing: self.mic_processing,
        })
    }
}

/// [`Profile::mic_device_conflicts`] for a stream capturing `source` with
/// the microphone on render endpoint `mic` (the session checks the source
/// it captures now, which may stand in for the profile's)
pub fn mic_output_conflicts(source: &SourceRequest, mic: &str, default_output_id: &str) -> Option<&'static str> {
    if mic.is_empty() {
        return None;
    }
    let looped = match source {
        SourceRequest::VirtualDevice { id, .. } | SourceRequest::OutputDevice { id, .. } => id == mic,
        SourceRequest::SystemLoopback => !default_output_id.is_empty() && default_output_id == mic,
        _ => false,
    };
    looped.then_some("error.headset_mic_loop")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(flatten)]
    other: BTreeMap<String, serde_json::Value>,
}

pub fn path(dir: &Path) -> PathBuf {
    crate::paths::profiles_file(dir)
}

/// Gives every profile a name of its own: v1 named profiles "<device>
/// <codec>" and told them apart by position, so two alike had the same
/// name, while v2 finds them by name. A repeated name gets " (2)", " (3)"…
/// (the first keeps its name, so a last_profile naming it still does).
/// Returns how many were renamed.
pub fn make_names_unique(list: &mut [Profile]) -> usize {
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut renamed = 0;
    for i in 0..list.len() {
        if taken.insert(list[i].name.clone()) {
            continue;
        }
        let base = list[i].name.clone();
        let name = (2..)
            .map(|n| format!("{base} ({n})"))
            // Not one taken already, nor one a later profile has
            .find(|c| !taken.contains(c) && !list[i + 1..].iter().any(|p| &p.name == c))
            .unwrap_or_default();
        taken.insert(name.clone());
        list[i].name = name;
        renamed += 1;
    }
    renamed
}

/// Folds the profiles of a device entry (address, LE Audio or not) into
/// one, as the Codecs screen keeps them: the one `last_profile` names if it
/// is of that device, else the first. A profile folded away gives its codec
/// settings to the kept one's `codec_settings` when the kept one has none
/// for that codec's card yet (an Auto profile has none of its own); its name,
/// capture source, volume sync and packet size go (the Audio Devices screen
/// already kept those the same for all of a device's profiles). Profiles
/// without a valid address are left as they are. Returns the names of the
/// profiles folded away, in order.
pub fn consolidate(list: &mut Vec<Profile>, last_profile: &str) -> Vec<String> {
    let key = |p: &Profile| bt_host::parse_addr(&p.device_address).map(|a| (a, p.le_audio));
    // The kept profile of each device
    let mut keeper: std::collections::HashMap<_, usize> = std::collections::HashMap::new();
    for (i, p) in list.iter().enumerate() {
        if let Some(k) = key(p) {
            match keeper.get(&k) {
                None => {
                    keeper.insert(k, i);
                }
                Some(&j) if p.name == last_profile && list[j].name != last_profile => {
                    keeper.insert(k, i);
                }
                _ => {}
            }
        }
    }
    let mut folded = Vec::new();
    let mut kept: Vec<Profile> = Vec::with_capacity(list.len());
    // Where each device's profile is in `kept`: the place of its first one
    let mut placed: std::collections::HashMap<_, usize> = std::collections::HashMap::new();
    let old = std::mem::take(list);
    let keepers: Vec<Profile> = old.clone();
    for (i, p) in old.into_iter().enumerate() {
        let Some(k) = key(&p) else {
            kept.push(p);
            continue;
        };
        let at = *placed.entry(k).or_insert_with(|| {
            kept.push(keepers[keeper[&k]].clone());
            kept.len() - 1
        });
        if i == keeper[&k] {
            continue;
        }
        let into = &mut kept[at];
        // By card: aptX Adaptive and its LL card keep their own
        let card = p.card().to_string();
        let gives = !p.le_audio && card != "auto" && card != into.card() && !into.codec_settings.contains_key(&card);
        tracing::info!(
            "profiles: \"{}\" folded into \"{}\" (same device){}",
            p.name,
            into.name,
            if gives { format!(", its {card} settings kept") } else { String::new() }
        );
        if gives {
            into.codec_settings.insert(card, p.codec_values());
        }
        for (c, v) in p.codec_settings {
            if c != into.card() {
                into.codec_settings.entry(c).or_insert(v);
            }
        }
        folded.push(p.name);
    }
    *list = kept;
    folded
}

/// VB-Audio's Hi-Fi Cable ("Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)"):
/// the free second cable, for the headset microphone (issue #86)
pub fn is_hifi_cable(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("hi-fi cable") || n.contains("hifi cable") || n.contains("hifi-cable")
}

/// VB-CABLE's playback side ("CABLE Input", "CABLE In 16ch"): a virtual
/// device named a cable that is not the Hi-Fi Cable
pub fn is_vb_cable(name: &str, is_virtual: bool) -> bool {
    is_virtual && name.to_lowercase().contains("cable") && !is_hifi_cable(name)
}

/// One of VB-Audio's cables Windows has: its playback endpoint
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cable {
    pub id: String,
    pub name: String,
}

/// The cables BlueCaster sets up by itself (issue #86): the Hi-Fi Cable
/// for the sound (less delay), VB-CABLE for the microphone
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cables {
    /// VB-CABLE's 2-channel cable, else its 16-channel one
    pub vb: Option<Cable>,
    pub hifi: Option<Cable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CableKind {
    Vb,
    Hifi,
}

/// Which of [`Cables`] were there
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CableSet {
    pub vb: bool,
    pub hifi: bool,
}

impl CableSet {
    fn has(&self, kind: CableKind) -> bool {
        match kind {
            CableKind::Vb => self.vb,
            CableKind::Hifi => self.hifi,
        }
    }

    /// The cable the sound was given when it went to one: the Hi-Fi Cable,
    /// else VB-CABLE
    fn sound(&self) -> Option<CableKind> {
        [CableKind::Hifi, CableKind::Vb].into_iter().find(|&k| self.has(k))
    }

    /// The microphone's cable beside a source of kind `source`: VB-CABLE,
    /// else the Hi-Fi Cable
    fn mic(&self, source: Option<CableKind>) -> Option<CableKind> {
        [CableKind::Vb, CableKind::Hifi].into_iter().find(|&k| self.has(k) && Some(k) != source)
    }
}

impl Cables {
    pub fn set(&self) -> CableSet {
        CableSet { vb: self.vb.is_some(), hifi: self.hifi.is_some() }
    }

    fn get(&self, kind: CableKind) -> Option<&Cable> {
        match kind {
            CableKind::Vb => self.vb.as_ref(),
            CableKind::Hifi => self.hifi.as_ref(),
        }
    }

    /// Which cable the endpoint `id` named `name` is: by its id, else by
    /// its name (a cable missing now, or installed again under another id).
    /// The 16-channel cable and VB-Audio's other cables (A, B...) are none:
    /// only chosen by hand.
    fn kind(&self, id: &str, name: &str) -> Option<CableKind> {
        if id.is_empty() {
            return None;
        }
        let is = |c: &Option<Cable>| c.as_ref().is_some_and(|c| c.id == id);
        let n = name.to_lowercase();
        if is(&self.vb) {
            Some(CableKind::Vb)
        } else if is(&self.hifi) || is_hifi_cable(name) {
            Some(CableKind::Hifi)
        } else if n.contains("vb-audio virtual cable") && !n.contains("16ch") {
            Some(CableKind::Vb)
        } else {
            None
        }
    }

    /// An endpoint `id` that is one of these cables under the id it had
    /// before (installed again): that cable's
    fn follow(&self, id: &mut String, name: &mut String) {
        if let Some(c) = self.kind(id, name).and_then(|k| self.get(k))
            && c.id != *id
        {
            *id = c.id.clone();
            *name = c.name.clone();
        }
    }

    /// The microphone's cable for a profile capturing `source`: VB-CABLE,
    /// else the Hi-Fi Cable, not one the microphone would loop into
    pub fn mic_for(&self, source: &SourceRequest, default_output_id: &str) -> Option<&Cable> {
        [&self.vb, &self.hifi]
            .into_iter()
            .flatten()
            .find(|c| mic_output_conflicts(source, &c.id, default_output_id).is_none())
    }
}

/// The microphone plays into `cable`. An LE Audio profile on Auto that had
/// no output goes Off with it: LE Audio takes Auto as On, so the
/// microphone would open by itself (issue #86).
fn give_mic(p: &mut Profile, cable: &Cable) {
    if p.le_audio && p.mic_device_id.is_empty() && HeadsetMic::parse(&p.headset_mic) == HeadsetMic::Auto {
        p.headset_mic = headset_off();
    }
    p.mic_device_id = cable.id.clone();
    p.mic_device_name = cable.name.clone();
}

/// What a device gets with no cable where Windows has both (issue #86): the
/// sound from a monitor's or TV's audio, the microphone into the sound card
#[derive(Debug, Clone, Copy)]
pub struct Bare<'a> {
    pub sound: &'a Cable,
    pub mic: &'a Cable,
}

/// The microphone plays into the sound card. On Auto it goes Off with it:
/// Auto looks at what is recorded from the card's own inputs (its microphone
/// jack), not at who listens to its output, so it would open for another
/// microphone, or never.
fn give_card_mic(p: &mut Profile, card: &Cable) {
    if HeadsetMic::parse(&p.headset_mic) == HeadsetMic::Auto {
        p.headset_mic = headset_off();
    }
    p.mic_device_id = card.id.clone();
    p.mic_device_name = card.name.clone();
}

/// The profiles whose microphone is Auto or On without an output get the
/// microphone's cable ([`Cables::mic_for`]; issue #86: "-" with the cables
/// installed, when Windows had not listed them as the profile was made, and
/// on LE Audio). No output is never the user's choice: the list cannot
/// choose it. How many changed.
pub fn fill_mic_outputs(list: &mut [Profile], cables: &Cables, default_output_id: &str) -> usize {
    let mut changed = 0;
    for p in
        list.iter_mut().filter(|p| p.mic_device_id.is_empty() && HeadsetMic::parse(&p.headset_mic) != HeadsetMic::Off)
    {
        if let Some(c) = cables.mic_for(&p.source(), default_output_id).cloned() {
            give_mic(p, &c);
            changed += 1;
        }
    }
    changed
}

/// The sound the rules give with the cables there
#[derive(Clone, Copy)]
enum Sound<'a> {
    /// A real output, captured as an Output Device
    Output(&'a Cable),
    /// A cable, captured as a virtual device
    Virtual(&'a Cable),
}

impl Sound<'_> {
    /// Makes it the profile's source; false when it already was
    fn give(self, p: &mut Profile) -> bool {
        let (mode, c) = match self {
            Sound::Output(c) => ("output", c),
            Sound::Virtual(c) => ("virtual", c),
        };
        if p.capture_mode == mode && p.audio_device_id == c.id {
            return false;
        }
        p.capture_mode = mode.into();
        p.audio_device_id = c.id.clone();
        p.audio_device_name = c.name.clone();
        p.auto_switch_device = true;
        true
    }
}

impl Cables {
    /// The sound as the first-run setup gives it (the UI's setupAudio; issue
    /// #86), whatever Windows' default output is: the source is made the
    /// default while its device is connected. Both cables: the Hi-Fi Cable.
    /// One, the microphone's: `real_output`, or the cable itself where
    /// there is none.
    fn sound_for<'a>(&'a self, real_output: Option<&'a Cable>) -> Option<Sound<'a>> {
        match (&self.vb, &self.hifi) {
            (Some(_), Some(hifi)) => Some(Sound::Virtual(hifi)),
            (None, None) => None,
            (Some(c), None) | (None, Some(c)) => Some(real_output.map_or(Sound::Virtual(c), Sound::Output)),
        }
    }
}

impl Cables {
    /// No cable is free for the microphone because the one there is the
    /// sound (the source, or Windows' default output under System Loopback):
    /// the sound moves as the rules give it with that cable alone
    /// ([`Cables::sound_for`]), which the microphone can take then
    fn make_room(&self, p: &mut Profile, default_output_id: &str, real_output: Option<&Cable>) {
        let on_cable = match p.capture_mode.as_str() {
            "loopback" => true,
            "virtual" | "output" => self.kind(&p.audio_device_id, &p.audio_device_name).is_some(),
            _ => false,
        };
        if on_cable
            && self.set() != CableSet::default()
            && self.mic_for(&p.source(), default_output_id).is_none()
            && let Some(sound) = self.sound_for(real_output)
        {
            sound.give(p);
        }
    }
}

/// A profile on System Loopback whose microphone plays into the cable that
/// is Windows' default output (issue #86: a cable's setup makes it the
/// default; the microphone would play in the headphones) gets the sound the
/// rules give instead ([`Cables::sound_for`]). How many changed.
pub fn unloop_sounds(
    list: &mut [Profile],
    now: &Cables,
    default_output_id: &str,
    real_output: Option<&Cable>,
) -> usize {
    let mut changed = 0;
    for p in list.iter_mut().filter(|p| {
        p.capture_mode == "loopback"
            && HeadsetMic::parse(&p.headset_mic) != HeadsetMic::Off
            && p.mic_device_conflicts(default_output_id).is_some()
    }) {
        match now.sound_for(real_output) {
            // The microphone's own cable is no way out
            Some(Sound::Virtual(c)) if c.id == p.mic_device_id => {}
            Some(sound) => changed += usize::from(sound.give(p)),
            None => {}
        }
    }
    changed
}

/// The names of the outputs the profiles capture and play the microphone
/// into, as Windows has them now (`outputs`: every output; issue #86: a
/// cable's output is listed under Windows' own name, "Speakers", before its
/// setup names it, and the lists showed that one). How many changed.
pub fn refresh_names(list: &mut [Profile], outputs: &[Cable]) -> usize {
    let name = |id: &str| outputs.iter().find(|d| !id.is_empty() && d.id == id).map(|d| d.name.as_str());
    let mut changed = 0;
    for p in list.iter_mut() {
        let before = changed;
        if matches!(p.capture_mode.as_str(), "virtual" | "output")
            && let Some(n) = name(&p.audio_device_id)
            && p.audio_device_name != n
        {
            p.audio_device_name = n.into();
            changed += 1;
        }
        if let Some(n) = name(&p.mic_device_id)
            && p.mic_device_name != n
        {
            p.mic_device_name = n.into();
            changed = before + 1;
        }
    }
    changed
}

/// A source or a microphone on a cable that is gone (uninstalled; issue #86:
/// the lists showed nothing chosen) is chosen again as the first-run setup
/// would with what is left: the sound on the cable left where both were
/// there ([`Cables::sound_for`]); the microphone on a cable left, the sound
/// making room for it where that cable was the sound. With none
/// left: `bare` where Windows has both, else System Loopback and no
/// microphone, also for the real output given beside the one cable. Gone
/// as well: another of VB-Audio's outputs chosen by hand (the 16-channel
/// cable) that is not among `outputs`, all Windows has (none: not looked
/// at). Other devices come back and stay. How many changed.
pub fn drop_cables(
    list: &mut [Profile],
    now: &Cables,
    default_output_id: &str,
    real_output: Option<&Cable>,
    bare: Option<Bare>,
    outputs: &[Cable],
    displays: &[Cable],
) -> usize {
    let given = |id: &str| real_output.is_some_and(|r| r.id == id) || displays.iter().any(|d| d.id == id);
    let gone = |id: &str, name: &str| {
        now.kind(id, name).is_some_and(|k| now.get(k).is_none())
            || (!id.is_empty()
                && !outputs.is_empty()
                && name.to_lowercase().contains("vb-audio")
                && !outputs.iter().any(|d| d.id == id))
    };
    let none_left = now.set() == CableSet::default();
    let mut changed = 0;
    for p in list.iter_mut() {
        let captured = matches!(p.capture_mode.as_str(), "virtual" | "output");
        let source_gone = captured && gone(&p.audio_device_id, &p.audio_device_name);
        let mic_gone = gone(&p.mic_device_id, &p.mic_device_name);
        let beside = mic_gone && none_left && p.capture_mode == "output" && given(&p.audio_device_id);
        if !source_gone && !mic_gone {
            continue;
        }
        if source_gone || beside {
            match now.sound_for(real_output).filter(|_| source_gone).or(bare.map(|b| Sound::Output(b.sound))) {
                Some(sound) => {
                    sound.give(p);
                }
                None => {
                    p.capture_mode = "loopback".into();
                    p.audio_device_id.clear();
                    p.audio_device_name.clear();
                }
            }
        }
        // Also one left that the new source loops into
        if mic_gone || mic_output_conflicts(&p.source(), &p.mic_device_id, default_output_id).is_some() {
            // The cable left is the sound (the Hi-Fi Cable, VB-CABLE gone)
            now.make_room(p, default_output_id, real_output);
            let card = bare
                .map(|b| b.mic)
                .filter(|c| none_left && mic_output_conflicts(&p.source(), &c.id, default_output_id).is_none());
            match (now.mic_for(&p.source(), default_output_id).cloned(), card) {
                (Some(c), _) => give_mic(p, &c),
                (None, Some(c)) => give_card_mic(p, c),
                (None, None) => {
                    p.mic_device_id.clear();
                    p.mic_device_name.clear();
                }
            }
        }
        changed += 1;
    }
    changed
}

/// Sets the cables up again once one came, as the first-run setup would
/// with them (issue #86: a cable installed later became the sound, with no
/// microphone): both cables, the sound on the Hi-Fi Cable and the microphone
/// on VB-CABLE; one, the microphone on it and the sound on a real output
/// ([`Cables::sound_for`]; `real_output`: a monitor's or TV's audio, else
/// another output that is no virtual device). Only what these rules gave
/// with the cables
/// there before (`was`) moves: such a source, System Loopback (every device
/// starts there) with such a microphone, a microphone they gave, none, or
/// one that loops into the source, and what `bare` gave while there was no
/// cable (its microphone goes back to Auto on the cable). Anything else is
/// the user's choice and stays. `displays`: the monitors' and TVs' audio,
/// any of which the rules may have given (the one that was Windows' default
/// output then). How many profiles changed.
pub fn reassign_cables(
    list: &mut [Profile],
    was: CableSet,
    now: &Cables,
    default_output_id: &str,
    real_output: Option<&Cable>,
    bare: Option<Bare>,
    displays: &[Cable],
) -> usize {
    let given = |id: &str| real_output.is_some_and(|r| r.id == id) || displays.iter().any(|d| d.id == id);
    let mut changed = 0;
    for p in list.iter_mut() {
        let before = p.clone();
        // A cable installed again under another id is the same cable
        if matches!(p.capture_mode.as_str(), "virtual" | "output") {
            now.follow(&mut p.audio_device_id, &mut p.audio_device_name);
        }
        now.follow(&mut p.mic_device_id, &mut p.mic_device_name);
        let source_kind = match p.capture_mode.as_str() {
            "virtual" | "output" => now.kind(&p.audio_device_id, &p.audio_device_name),
            _ => None,
        };
        let loops = |p: &Profile| mic_output_conflicts(&p.source(), &p.mic_device_id, default_output_id).is_some();
        let mic_kind = now.kind(&p.mic_device_id, &p.mic_device_name);
        let card_mic = was == CableSet::default() && bare.is_some_and(|b| b.mic.id == p.mic_device_id);
        let mic_auto = p.mic_device_id.is_empty()
            || loops(p)
            || card_mic
            || (mic_kind.is_some() && mic_kind == was.mic(source_kind));
        let no_mic = p.mic_device_id.is_empty() && HeadsetMic::parse(&p.headset_mic) != HeadsetMic::Off;
        let sound_auto = match p.capture_mode.as_str() {
            "loopback" => mic_auto,
            // Also the first cable as an earlier version set it up: the
            // sound on it, no microphone
            "virtual" => {
                source_kind.is_some() && (source_kind == was.sound() || (was == CableSet::default() && no_mic))
            }
            // The real output given beside the one cable
            "output" => {
                (was.vb != was.hifi && mic_kind.is_some() && mic_kind == was.mic(None) && given(&p.audio_device_id))
                    || (card_mic && given(&p.audio_device_id))
            }
            _ => false,
        };
        if let Some(sound) = now.sound_for(real_output).filter(|_| sound_auto) {
            sound.give(p);
        }
        // A microphone the new source loops into moves too
        if (mic_auto || loops(p))
            && let Some(c) = now.mic_for(&p.source(), default_output_id).cloned()
            && p.mic_device_id != c.id
        {
            if card_mic && !p.le_audio && is_headset_off(&p.headset_mic) {
                p.headset_mic = "auto".into();
            }
            give_mic(p, &c);
        }
        if *p != before {
            changed += 1;
        }
    }
    changed
}

pub fn load(dir: &Path) -> Vec<Profile> {
    load_counted(dir).0
}

/// [`load`], and how many profiles it renamed
pub fn load_counted(dir: &Path) -> (Vec<Profile>, usize) {
    let text = match std::fs::read_to_string(path(dir)) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!("profiles: {} not read ({e}), no profiles", path(dir).display());
            return (Vec::new(), 0);
        }
    };
    match serde_json::from_str::<ProfilesFile>(&text) {
        Ok(mut f) => {
            let renamed = make_names_unique(&mut f.profiles);
            let separated = f.profiles.iter_mut().map(Profile::separate_card_settings).filter(|m| *m).count();
            if separated > 0 {
                tracing::info!("profiles: {separated} profile(s) with aptX Adaptive LL settings of their own now");
            }
            tracing::debug!(
                "profiles: loaded {} profile(s) from {}, {renamed} renamed",
                f.profiles.len(),
                path(dir).display()
            );
            (f.profiles, renamed)
        }
        Err(e) => {
            tracing::warn!("profiles: {} is not valid ({e})", path(dir).display());
            (Vec::new(), 0)
        }
    }
}

pub fn save(dir: &Path, profiles: &[Profile]) -> std::io::Result<()> {
    let file = ProfilesFile { profiles: profiles.to_vec(), other: BTreeMap::new() };
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = path(dir).with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    let r = std::fs::rename(&tmp, path(dir));
    tracing::debug!("profiles: saved {} profile(s) to {}: {r:?}", profiles.len(), path(dir).display());
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hifi_cable_is_told_apart_from_vb_cable() {
        assert!(is_hifi_cable("Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)"));
        assert!(!is_vb_cable("Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)", true));
        assert!(is_vb_cable("CABLE Input (VB-Audio Virtual Cable)", true));
        assert!(is_vb_cable("CABLE In 16ch (VB-Audio Virtual Cable)", true));
        assert!(!is_hifi_cable("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(!is_vb_cable("CABLE Input (VB-Audio Virtual Cable)", false));
    }

    #[test]
    fn mic_processing_changes_alone_and_is_written_only_when_set() {
        let p = Profile { name: "WH".into(), device_address: "00:11:22:33:44:55".into(), ..Profile::default() };
        assert!(serde_json::to_value(&p).unwrap().get("mic_processing").is_none());
        let on = Profile {
            mic_processing: ipc::MicProcessing { enabled: true, gain_db: 6, suppression: 100, ..Default::default() },
            ..p.clone()
        };
        assert!(p.only_mic_processing_differs(&on));
        assert!(!p.only_mic_processing_differs(&p));
        assert!(!p.only_mic_processing_differs(&Profile { codec: "sbc".into(), ..on.clone() }));
        assert!(!p.only_headset_mic_differs(&on));
        let v = serde_json::to_value(&on).unwrap();
        assert_eq!(v["mic_processing"]["gain_db"], 6);
        let back: Profile = serde_json::from_value(v).unwrap();
        assert_eq!(back.mic_processing, on.mic_processing);
        assert_eq!(on.to_request(679).unwrap().mic_processing, on.mic_processing);
    }

    const VB: &str = "CABLE Input (VB-Audio Virtual Cable)";
    const HIFI: &str = "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)";

    fn cables(vb: bool, hifi: bool) -> Cables {
        let cable = |id: &str, name: &str| Cable { id: id.into(), name: name.into() };
        Cables { vb: vb.then(|| cable("{vb}", VB)), hifi: hifi.then(|| cable("{hifi}", HIFI)) }
    }

    /// A profile capturing `source` (an endpoint id, "" for System
    /// Loopback) with its microphone on `mic` ("" for none), on Auto
    fn cabled(source: &str, mic: &str) -> Profile {
        let name = |id: &str| match id {
            "{vb}" => VB,
            "{hifi}" => HIFI,
            _ => "Voicemeeter Input (VB-Audio Voicemeeter VAIO)",
        };
        let mut p = Profile { headset_mic: "auto".into(), ..Profile::default() };
        if !source.is_empty() {
            p.capture_mode = "virtual".into();
            p.audio_device_id = source.into();
            p.audio_device_name = name(source).into();
        }
        if !mic.is_empty() {
            p.mic_device_id = mic.into();
            p.mic_device_name = name(mic).into();
        }
        p
    }

    /// (source, microphone) of each profile: "" for System Loopback / none
    fn setup(list: &[Profile]) -> Vec<(&str, &str)> {
        list.iter()
            .map(|p| {
                let source = if p.capture_mode == "loopback" { "" } else { p.audio_device_id.as_str() };
                (source, p.mic_device_id.as_str())
            })
            .collect()
    }

    #[test]
    fn vb_cable_after_the_hifi_cable_takes_the_microphone_not_the_sound() {
        // issue #86: devices set up with the Hi-Fi Cable alone, VB-CABLE
        // installed later (and made the Windows default output by its setup)
        let was = CableSet { vb: false, hifi: true };
        let mut list = vec![
            cabled("{hifi}", ""),              // the setup's: no microphone
            cabled("", "{hifi}"),              // Windows default, Hi-Fi Cable microphone
            cabled("{voicemeeter}", ""),       // a source chosen by hand stays
            cabled("{hifi}", "{voicemeeter}"), // a microphone chosen by hand stays
        ];
        assert_eq!(reassign_cables(&mut list, was, &cables(true, true), "{vb}", None, None, &[]), 3);
        assert_eq!(
            setup(&list),
            [("{hifi}", "{vb}"), ("{hifi}", "{vb}"), ("{voicemeeter}", "{vb}"), ("{hifi}", "{voicemeeter}")]
        );
        assert_eq!((list[1].audio_device_name.as_str(), list[1].mic_device_name.as_str()), (HIFI, VB));
        assert!(list[1].auto_switch_device);
        // A second time changes nothing
        assert_eq!(
            reassign_cables(&mut list, cables(true, true).set(), &cables(true, true), "{vb}", None, None, &[]),
            0
        );
    }

    #[test]
    fn the_hifi_cable_after_vb_cable_takes_the_sound_and_vb_cable_the_microphone() {
        // The other order: the sound on VB-CABLE while it was alone
        let was = CableSet { vb: true, hifi: false };
        let mut list = vec![
            cabled("{vb}", ""), // what VB-CABLE alone gave
            cabled("", "{vb}"), // Windows default with VB-CABLE's microphone
            cabled("", ""),     // nothing yet
            // Windows default with a microphone chosen by hand: the user's
            cabled("", "{voicemeeter}"),
            Profile { capture_mode: "app".into(), ..cabled("", "{vb}") },
        ];
        assert_eq!(reassign_cables(&mut list, was, &cables(true, true), "", None, None, &[]), 3);
        assert_eq!(
            setup(&list),
            [("{hifi}", "{vb}"), ("{hifi}", "{vb}"), ("{hifi}", "{vb}"), ("", "{voicemeeter}"), ("", "{vb}")]
        );
        assert_eq!(list[4].capture_mode, "app");
    }

    #[test]
    fn a_source_chosen_by_hand_keeps_its_cable() {
        // VB-CABLE as the source while the Hi-Fi Cable was there: not what
        // the rules give, so the user's; the microphone moves off it
        let was = CableSet { vb: false, hifi: true };
        let mut list = vec![cabled("{vb}", "{hifi}"), cabled("{vb}", "")];
        reassign_cables(&mut list, was, &cables(true, true), "", None, None, &[]);
        assert_eq!(setup(&list), [("{vb}", "{hifi}"), ("{vb}", "{hifi}")]);
    }

    #[test]
    fn one_cable_alone_takes_the_microphone() {
        // issue #86: set up with a monitor's HDMI alone, VB-CABLE installed
        // later became the sound, with no microphone
        let real = hdmi();
        let fresh = || vec![cabled("", ""), Profile { capture_mode: "app".into(), ..cabled("", "") }];
        // The real output is the sound, whichever is Windows' default
        for default in ["{hdmi}", "{vb}"] {
            let mut list = fresh();
            assert_eq!(
                reassign_cables(&mut list, CableSet::default(), &cables(true, false), default, Some(&real), None, &[]),
                2
            );
            assert_eq!(setup(&list), [("{hdmi}", "{vb}"), ("", "{vb}")]);
        }
        let mut list = fresh();
        reassign_cables(&mut list, CableSet::default(), &cables(true, false), "{vb}", Some(&real), None, &[]);
        assert_eq!((list[0].capture_mode.as_str(), list[0].auto_switch_device), ("output", true));
        assert_eq!(list[0].audio_device_name, real.name);
        // A second time changes nothing
        assert_eq!(
            reassign_cables(&mut list, cables(true, false).set(), &cables(true, false), "{vb}", Some(&real), None, &[]),
            0
        );
        // No real output: the cable is the sound, without a microphone
        let mut list = vec![cabled("", "")];
        reassign_cables(&mut list, CableSet::default(), &cables(false, true), "{hifi}", None, None, &[]);
        assert_eq!(setup(&list), [("{hifi}", "")]);
        assert_eq!(list[0].capture_mode, "virtual");
    }

    fn hdmi() -> Cable {
        Cable { id: "{hdmi}".into(), name: "LG TV (NVIDIA High Definition Audio)".into() }
    }

    #[test]
    fn windows_default_with_the_microphone_on_the_default_cable_moves_to_a_real_output() {
        // issue #86: Windows default (CABLE Input) with CABLE Output as the
        // microphone, long after the cable came
        let off = Profile { headset_mic: "off".into(), ..cabled("", "{vb}") };
        let mut list = vec![cabled("", "{vb}"), off, cabled("", "{voicemeeter}"), cabled("{hifi}", "{vb}")];
        assert_eq!(unloop_sounds(&mut list, &cables(true, false), "{vb}", Some(&hdmi())), 1);
        assert_eq!(setup(&list), [("{hdmi}", "{vb}"), ("", "{vb}"), ("", "{voicemeeter}"), ("{hifi}", "{vb}")]);
        assert_eq!((list[0].capture_mode.as_str(), list[0].auto_switch_device), ("output", true));
        // Another default output: nothing loops
        let mut list = vec![cabled("", "{vb}")];
        assert_eq!(unloop_sounds(&mut list, &cables(true, false), "{hdmi}", Some(&hdmi())), 0);
        // Both cables: the Hi-Fi Cable
        assert_eq!(unloop_sounds(&mut list, &cables(true, true), "{vb}", Some(&hdmi())), 1);
        assert_eq!(setup(&list), [("{hifi}", "{vb}")]);
        // No other output: it stays, the UI warns
        let mut list = vec![cabled("", "{vb}")];
        assert_eq!(unloop_sounds(&mut list, &cables(true, false), "{vb}", None), 0);
    }

    #[test]
    fn a_cable_that_is_gone_is_chosen_again() {
        // issue #86: the Hi-Fi Cable uninstalled left the lists empty
        let output = |id: &str, mic: &str| Profile {
            capture_mode: "output".into(),
            audio_device_id: id.into(),
            ..cabled("", mic)
        };
        let fresh = || {
            vec![
                cabled("{hifi}", "{vb}"),          // both cables, as set up
                cabled("{vb}", "{hifi}"),          // the other way, by hand
                cabled("{voicemeeter}", "{hifi}"), // a source chosen by hand stays
                output("{hdmi}", "{vb}"),          // nothing of it gone
            ]
        };
        let mut list = fresh();
        assert_eq!(drop_cables(&mut list, &cables(true, false), "{spk}", Some(&hdmi()), None, &[], &[]), 3);
        assert_eq!(
            setup(&list),
            [("{hdmi}", "{vb}"), ("{hdmi}", "{vb}"), ("{voicemeeter}", "{vb}"), ("{hdmi}", "{vb}")]
        );
        assert_eq!(
            (list[0].capture_mode.as_str(), list[0].audio_device_name.as_str()),
            ("output", hdmi().name.as_str())
        );
        // A second time changes nothing
        assert_eq!(drop_cables(&mut list, &cables(true, false), "{spk}", Some(&hdmi()), None, &[], &[]), 0);
        // VB-CABLE gone, the Hi-Fi Cable the source: the sound moves to the
        // real output, the microphone to the Hi-Fi Cable; without a real
        // output the cable stays the sound, with no microphone
        let mut list = vec![cabled("{hifi}", "{vb}"), cabled("", "{vb}")];
        assert_eq!(drop_cables(&mut list, &cables(false, true), "{hifi}", Some(&hdmi()), None, &[], &[]), 2);
        assert_eq!(setup(&list), [("{hdmi}", "{hifi}"), ("{hdmi}", "{hifi}")]);
        let mut list = vec![cabled("{hifi}", "{vb}")];
        assert_eq!(drop_cables(&mut list, &cables(false, true), "{hifi}", None, None, &[], &[]), 1);
        assert_eq!(setup(&list), [("{hifi}", "")]);
        // The last cable: Windows default, no microphone; also the real
        // output given beside it
        let mut list = vec![output("{hdmi}", "{vb}"), cabled("{vb}", ""), output("{spk}", "{vb}")];
        assert_eq!(drop_cables(&mut list, &Cables::default(), "{spk}", Some(&hdmi()), None, &[], &[]), 3);
        assert_eq!(setup(&list), [("", ""), ("", ""), ("{spk}", "")]);
        assert!(list[0].audio_device_id.is_empty());
        // Both there: nothing
        let mut list = fresh();
        assert_eq!(drop_cables(&mut list, &cables(true, true), "{spk}", Some(&hdmi()), None, &[], &[]), 0);
    }

    #[test]
    fn a_vb_audio_output_chosen_by_hand_that_is_gone_is_chosen_again() {
        // issue #86: the 16-channel cable as the microphone, VB-CABLE
        // installed again under other ids: "(not connected)"
        let c16 = "CABLE In 16ch (VB-Audio Virtual Cable)";
        let mut list = vec![cabled("{hifi}", "{c16}"), cabled("{hifi}", "{voicemeeter}"), cabled("{c16}", "{vb}")];
        list[0].mic_device_name = c16.into();
        list[2].audio_device_name = c16.into();
        let now = cables(true, true);
        let outputs = [
            now.vb.clone().unwrap(),
            now.hifi.clone().unwrap(),
            Cable { id: "{c16b}".into(), name: c16.into() },
            Cable { id: "{voicemeeter}".into(), name: "Voicemeeter Input (VB-Audio Voicemeeter VAIO)".into() },
        ];
        assert_eq!(drop_cables(&mut list, &now, "", None, None, &outputs, &[]), 2);
        assert_eq!(setup(&list), [("{hifi}", "{vb}"), ("{hifi}", "{voicemeeter}"), ("{hifi}", "{vb}")]);
        assert_eq!(list[0].mic_device_name, VB);
    }

    #[test]
    fn names_follow_windows() {
        // The Hi-Fi Cable's output as Windows lists it at first
        let mut list = vec![cabled("{hifi}", "{vb}"), cabled("", "{vb}"), cabled("{gone}", "")];
        list[0].audio_device_name = "Speakers (VB-Audio Hi-Fi Cable)".into();
        list[1].audio_device_name = "kept".into();
        let outputs = [cables(true, true).hifi.unwrap(), cables(true, true).vb.unwrap()];
        assert_eq!(refresh_names(&mut list, &outputs), 1);
        assert_eq!(list[0].audio_device_name, HIFI);
        // Not a source that is no device, nor one Windows does not list
        assert_eq!(list[1].audio_device_name, "kept");
        assert_eq!(refresh_names(&mut list, &outputs), 0);
    }

    #[test]
    fn the_last_cable_gone_leaves_a_display_and_the_sound_card() {
        let card = Cable { id: "{spk}".into(), name: "Speakers (Realtek(R) Audio)".into() };
        let tv = hdmi();
        let bare = Some(Bare { sound: &tv, mic: &card });
        let on = Profile { headset_mic: "on".into(), ..cabled("{vb}", "") };
        let mut list = vec![cabled("{vb}", ""), cabled("", "{vb}"), cabled("{voicemeeter}", "{vb}"), on];
        // The sound card is Windows' default output: Windows default would
        // capture the microphone played into it
        assert_eq!(drop_cables(&mut list, &Cables::default(), "{spk}", Some(&tv), bare, &[], &[]), 4);
        assert_eq!(setup(&list), [("{hdmi}", ""), ("", ""), ("{voicemeeter}", "{spk}"), ("{hdmi}", "")]);
        // A microphone it gives is Off, unless it was On
        assert_eq!((list[2].headset_mic.as_str(), list[3].headset_mic.as_str()), ("off", "on"));
        let mut list = vec![
            Profile { capture_mode: "output".into(), audio_device_id: "{hdmi}".into(), ..cabled("", "{vb}") },
            Profile { headset_mic: "on".into(), ..cabled("{voicemeeter}", "{vb}") },
        ];
        assert_eq!(drop_cables(&mut list, &Cables::default(), "{hdmi}", Some(&tv), bare, &[], &[]), 2);
        assert_eq!(setup(&list), [("{hdmi}", "{spk}"), ("{voicemeeter}", "{spk}")]);
        assert_eq!((list[0].headset_mic.as_str(), list[1].headset_mic.as_str()), ("off", "on"));
        // A cable again: what it gave moves to the cable, on Auto; On stays
        assert_eq!(reassign_cables(&mut list, CableSet::default(), &cables(true, false), "", Some(&tv), bare, &[]), 2);
        assert_eq!(setup(&list), [("{hdmi}", "{vb}"), ("{voicemeeter}", "{vb}")]);
        assert_eq!((list[0].headset_mic.as_str(), list[1].headset_mic.as_str()), ("auto", "on"));
    }

    #[test]
    fn the_first_cable_as_an_earlier_version_set_it_up_is_set_up_again() {
        // The sound on the cable and no microphone; the cable removed and
        // installed again. A microphone turned Off: the user's, it stays
        let mut list = vec![cabled("{vb}", ""), Profile { headset_mic: "off".into(), ..cabled("{vb}", "") }];
        assert_eq!(
            reassign_cables(&mut list, CableSet::default(), &cables(true, false), "{vb}", Some(&hdmi()), None, &[]),
            1
        );
        assert_eq!(setup(&list), [("{hdmi}", "{vb}"), ("{vb}", "")]);
    }

    #[test]
    fn the_second_cable_takes_the_sound_from_the_real_output_the_rules_gave() {
        let output =
            |id: &str| Profile { capture_mode: "output".into(), audio_device_id: id.into(), ..cabled("", "{vb}") };
        // Another output than the rules give: chosen by hand, it stays
        let mut list = vec![output("{hdmi}"), output("{speakers}")];
        let was = CableSet { vb: true, hifi: false };
        assert_eq!(reassign_cables(&mut list, was, &cables(true, true), "", Some(&hdmi()), None, &[]), 1);
        assert_eq!(setup(&list), [("{hifi}", "{vb}"), ("{speakers}", "{vb}")]);
        assert_eq!(list[0].capture_mode, "virtual");
    }

    #[test]
    fn a_cable_installed_again_under_another_id_is_followed() {
        let now = Cables { vb: Some(Cable { id: "{vb2}".into(), name: VB.into() }), ..cables(false, true) };
        let mut list = vec![cabled("{hifi}", "{vb}")];
        assert_eq!(reassign_cables(&mut list, CableSet { vb: false, hifi: true }, &now, "", None, None, &[]), 1);
        assert_eq!(setup(&list), [("{hifi}", "{vb2}")]);
    }

    #[test]
    fn a_microphone_on_without_an_output_gets_a_cable() {
        // issue #86: "-" with the cables installed
        let mut list = vec![
            cabled("{hifi}", ""),
            cabled("", ""),
            Profile { headset_mic: "off".into(), ..cabled("", "") }, // Off: stays
            cabled("", "{voicemeeter}"),                             // has one: stays
            cabled("{vb}", ""),                                      // VB-CABLE is the source
        ];
        assert_eq!(fill_mic_outputs(&mut list, &cables(true, true), ""), 3);
        assert_eq!(
            setup(&list),
            [("{hifi}", "{vb}"), ("", "{vb}"), ("", ""), ("", "{voicemeeter}"), ("{vb}", "{hifi}")]
        );
        assert_eq!(list[0].headset_mic, "auto");
        // VB-CABLE as the Windows default output: Windows default takes the
        // Hi-Fi Cable
        let mut lb = vec![cabled("", "")];
        fill_mic_outputs(&mut lb, &cables(true, true), "{vb}");
        assert_eq!(setup(&lb), [("", "{hifi}")]);
        // No cable it can use: none
        let mut only = vec![cabled("{vb}", "")];
        assert_eq!(fill_mic_outputs(&mut only, &cables(true, false), ""), 0);
    }

    #[test]
    fn an_le_audio_microphone_gets_a_cable_but_stays_closed() {
        // LE Audio takes Auto as On: the cable is chosen and the microphone
        // Off until it is turned on; On stays On
        let le = |mode: &str| Profile { le_audio: true, headset_mic: mode.into(), ..cabled("{hifi}", "") };
        let mut list = vec![le("auto"), le("on")];
        assert_eq!(fill_mic_outputs(&mut list, &cables(true, true), ""), 2);
        assert_eq!(setup(&list), [("{hifi}", "{vb}"), ("{hifi}", "{vb}")]);
        assert_eq!((list[0].headset_mic.as_str(), list[1].headset_mic.as_str()), ("off", "on"));
    }

    #[test]
    fn an_output_device_profile_captures_that_device() {
        let p = Profile {
            capture_mode: "output".into(),
            audio_device_id: "{0.0.0.00000000}.{speakers}".into(),
            audio_device_name: "Speakers".into(),
            ..Profile::default()
        };
        assert_eq!(
            p.source(),
            SourceRequest::OutputDevice {
                id: "{0.0.0.00000000}.{speakers}".into(),
                name: "Speakers".into(),
                auto_switch: true
            }
        );
        // The default output switch as saved (issue #86)
        let kept = Profile { auto_switch_device: false, ..p.clone() };
        assert_eq!(kept.source().default_switch(), None);
        // None chosen: the default output
        let none = Profile { audio_device_id: String::new(), ..p.clone() };
        assert_eq!(none.source(), SourceRequest::SystemLoopback);
        // Kept as saved
        let back: Profile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn v1_profile() {
        let text = r#"{"profiles":[{"name":"WH","device_address":"00:A2:D0:00:00:02","codec":"ldac","quality":"sq",
            "bit_depth":24,"capture_mode":"virtual","audio_device_id":"{x}","max_media_payload":1023,"extra":1}]}"#;
        let f: ProfilesFile = serde_json::from_str(text).unwrap();
        let p = &f.profiles[0];
        assert_eq!(p.other.get("extra"), Some(&serde_json::json!(1)));
        let r = p.to_request(679).unwrap();
        assert_eq!(r.codec, Some(CodecKind::Ldac));
        assert_eq!(r.quality, Quality::Standard);
        assert_eq!(r.bit_depth, Some(24));
        assert_eq!(r.max_media_payload, 1023);
        assert!(matches!(r.source, SourceRequest::VirtualDevice { auto_switch: true, .. }));
        let mut legacy = p.clone();
        legacy.max_media_payload = None;
        assert_eq!(legacy.to_request(1500).unwrap().max_media_payload, 1500);
        assert_eq!(r.le, None);
    }

    #[test]
    fn a_profile_always_falls_back() {
        // Saved with the old option off (issue #86: e.g. LHDC chosen for
        // headphones without it): the request still falls back, so the
        // codec is never tried on a device that does not offer it
        let text = r#"{"name":"EarFun | LHDC V5","device_address":"00:A2:D0:00:00:02","codec":"lhdcv5","codec_fallback":false}"#;
        let p: Profile = serde_json::from_str(text).unwrap();
        assert!(!p.codec_fallback);
        let r = p.to_request(1023).unwrap();
        assert_eq!(r.codec, Some(CodecKind::LhdcV5));
        assert!(r.codec_fallback);
        // Auto (switch on) is no codec of its own
        let auto = Profile { codec: "auto".into(), ..p };
        assert_eq!(auto.to_request(1023).unwrap().codec, None);
    }

    #[test]
    fn as_auto_streams_auto_with_its_defaults() {
        let p = Profile {
            device_address: "AA:BB:CC:DD:EE:FF".into(),
            codec: "ldac".into(),
            quality: "mq".into(),
            sample_rate: 96000,
            bit_depth: 24,
            sbc_bitpool: 40,
            volume_sync: "off".into(),
            ..Profile::default()
        };
        let a = p.as_auto();
        assert_eq!(a.codec, "auto");
        assert_eq!(a.codec_values(), CodecValues::default());
        // What belongs to the device stays
        assert_eq!(a.volume_sync, "off");
        assert_eq!(a.to_request(1023).unwrap().codec, None);
        let le = Profile { le_audio: true, ..p };
        assert_eq!(le.as_auto(), le);
    }

    #[test]
    fn a_forced_codec_and_unlisted_options_reach_the_request_but_not_auto() {
        let p = Profile {
            device_address: "AA:BB:CC:DD:EE:FF".into(),
            codec: "aptx".into(),
            codec_forced: true,
            unlisted_options: true,
            ..Profile::default()
        };
        let r = p.to_request(1023).unwrap();
        assert!(r.codec_forced && r.unlisted_options);
        let a = p.as_auto().to_request(1023).unwrap();
        assert!(!a.codec_forced && !a.unlisted_options);
        // Written only when set
        let text = serde_json::to_string(&Profile::default()).unwrap();
        assert!(!text.contains("codec_forced") && !text.contains("unlisted_options"));
    }

    #[test]
    fn aptx_adaptive_ll_is_a_card_of_its_own() {
        let ad = CodecValues { sample_rate: 96000, abr: true, ..CodecValues::default() };
        let ll = CodecValues { quality: "ll".into(), unlisted_options: true, ..CodecValues::default() };
        let p = Profile {
            device_address: "AA:BB:CC:DD:EE:FF".into(),
            codec: "aptxad".into(),
            quality: "ll".into(),
            sample_rate: 48000,
            codec_settings: BTreeMap::from([("aptxad".to_string(), ad.clone())]),
            ..Profile::default()
        };
        assert_eq!(p.card(), APTXAD_LL_CARD);
        assert_eq!(card_key("aptxad", "hq"), "aptxad");
        assert_eq!(card_key("ldac", "ll"), "ldac");
        // The LL card chosen streams LL
        let r = p.to_request(1023).unwrap();
        assert_eq!(
            (r.codec, r.quality, r.sample_rate),
            (Some(CodecKind::AptxAdaptive), Quality::LowLatency, Some(48000))
        );
        // Auto-select picks aptX Adaptive with aptX Adaptive's own, not LL's
        let all = p.all_codec_values();
        assert_eq!(all, vec![(CodecKind::AptxAdaptive, ad.clone())]);
        let mut req = p.as_auto().to_request(1023).unwrap();
        all[0].1.apply_to(&mut req);
        assert_eq!((req.quality, req.sample_rate, req.abr), (Quality::High, Some(96000), true));
        // aptX Adaptive chosen: its own, LL's kept aside are no codec of Auto's
        let q = Profile {
            quality: "hq".into(),
            codec_settings: BTreeMap::from([(APTXAD_LL_CARD.to_string(), ll.clone())]),
            ..p.clone()
        };
        assert_eq!(q.all_codec_values(), vec![(CodecKind::AptxAdaptive, q.codec_values())]);
        // Each round-trips under its own key
        let text = serde_json::to_string(&q).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&text).unwrap().codec_settings.get(APTXAD_LL_CARD), Some(&ll));
    }

    #[test]
    fn an_earlier_aptx_adaptive_low_latency_loads_as_ll_settings() {
        // Kept for aptX Adaptive in Low Latency before LL had a card
        let old = CodecValues { quality: "ll".into(), sample_rate: 48000, ..CodecValues::default() };
        let mut p = Profile {
            codec: "ldac".into(),
            codec_settings: BTreeMap::from([("aptxad".to_string(), old.clone())]),
            ..Profile::default()
        };
        assert!(p.separate_card_settings());
        assert_eq!(p.codec_settings.get(APTXAD_LL_CARD), Some(&old));
        assert!(!p.codec_settings.contains_key("aptxad"));
        assert!(!p.separate_card_settings());
        // Not under Auto-select's aptX Adaptive before it moves either
        let q = Profile { codec_settings: BTreeMap::from([("aptxad".to_string(), old.clone())]), ..p.clone() };
        assert!(q.all_codec_values().iter().all(|(k, _)| *k != CodecKind::AptxAdaptive));
        // LL's own already there: it stays
        let own = CodecValues { quality: "ll".into(), ..CodecValues::default() };
        let mut p = Profile {
            codec_settings: BTreeMap::from([
                ("aptxad".to_string(), old.clone()),
                (APTXAD_LL_CARD.to_string(), own.clone()),
            ]),
            ..Profile::default()
        };
        assert!(p.separate_card_settings());
        assert_eq!(p.codec_settings, BTreeMap::from([(APTXAD_LL_CARD.to_string(), own)]));
        // aptX Adaptive's own (High) are left alone
        let mut p = Profile {
            codec_settings: BTreeMap::from([("aptxad".to_string(), CodecValues::default())]),
            ..Profile::default()
        };
        assert!(!p.separate_card_settings());
        assert!(p.codec_settings.contains_key("aptxad"));
        // Loaded from a file
        let dir = std::env::temp_dir().join(format!("bc-profiles-ll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = path(&dir);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            r#"{"profiles":[{"name":"EarFun","device_address":"E8:AB:FA:AA:BB:CC","codec":"aac",
                "codec_settings":{"aptxad":{"quality":"ll","sample_rate":48000,"bit_depth":0,"abr":false}}}]}"#,
        )
        .unwrap();
        let list = load(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list[0].codec_settings.get(APTXAD_LL_CARD).map(|v| v.sample_rate), Some(48000));
        assert!(!list[0].codec_settings.contains_key("aptxad"));
    }

    #[test]
    fn folding_keeps_aptx_adaptive_and_ll_apart() {
        let base = Profile { device_address: "AA:BB:CC:DD:EE:FF".into(), codec: "aptxad".into(), ..Profile::default() };
        let mut list = vec![
            Profile { name: "AD".into(), sample_rate: 96000, ..base.clone() },
            Profile { name: "LL".into(), quality: "ll".into(), sample_rate: 48000, ..base.clone() },
        ];
        assert_eq!(consolidate(&mut list, "AD"), ["LL"]);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].card(), list[0].sample_rate), ("aptxad", 96000));
        let ll = &list[0].codec_settings[APTXAD_LL_CARD];
        assert_eq!((ll.quality.as_str(), ll.sample_rate), ("ll", 48000));
        // The other way round
        let mut list = vec![
            Profile { name: "AD".into(), sample_rate: 96000, ..base.clone() },
            Profile { name: "LL".into(), quality: "ll".into(), ..base },
        ];
        consolidate(&mut list, "LL");
        assert_eq!(list[0].card(), APTXAD_LL_CARD);
        assert_eq!(list[0].codec_settings["aptxad"].sample_rate, 96000);
    }

    #[test]
    fn every_codec_values_for_auto_select_in_developer_mode() {
        let aac = CodecValues { quality: "sq".into(), abr: true, ..CodecValues::default() };
        let p = Profile {
            device_address: "AA:BB:CC:DD:EE:FF".into(),
            codec: "ldac".into(),
            sample_rate: 96000,
            bit_depth: 24,
            codec_settings: BTreeMap::from([
                ("aac".to_string(), aac.clone()),
                // Stale copy of the codec in use, and a key no codec has
                ("ldac".to_string(), CodecValues::default()),
                ("nope".to_string(), CodecValues::default()),
            ]),
            ..Profile::default()
        };
        let all = p.all_codec_values();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], (CodecKind::Ldac, p.codec_values()));
        assert_eq!(all[1], (CodecKind::Aac, aac.clone()));

        // Put into an Auto request, as stream.rs does for the codec picked
        let mut req = p.as_auto().to_request(1023).unwrap();
        assert_eq!((req.sample_rate, req.bit_depth), (None, None));
        all[0].1.apply_to(&mut req);
        assert_eq!((req.sample_rate, req.bit_depth), (Some(96000), Some(24)));
        aac.apply_to(&mut req);
        assert_eq!((req.quality, req.abr, req.sample_rate), (Quality::Standard, true, None));
        assert_eq!(req.codec, None);
    }

    #[test]
    fn codec_order_in_the_request_written_only_when_set() {
        let text =
            r#"{"name":"EarFun","device_address":"00:A2:D0:00:00:02","codec":"opus","codec_order":["sbcxq","sbc"]}"#;
        let p: Profile = serde_json::from_str(text).unwrap();
        let order = p.to_request(1023).unwrap().auto_order;
        assert_eq!(&order.codecs()[..2], &[CodecKind::SbcXq, CodecKind::Sbc]);
        assert_eq!(order.codecs().len(), CodecKind::AUTO_PRIORITY.len());
        // Auto-select: the default order
        // Under Auto-select too
        assert_eq!(p.as_auto().to_request(1023).unwrap().auto_order, order);
        assert_eq!(serde_json::to_value(&p).unwrap()["codec_order"], serde_json::json!(["sbcxq", "sbc"]));
        let plain = Profile { codec_order: Vec::new(), ..p };
        assert!(plain.to_request(1023).unwrap().auto_order.is_default());
        assert!(serde_json::to_value(&plain).unwrap().get("codec_order").is_none());
    }

    #[test]
    fn sbc_setup_in_the_request_and_written_only_when_set() {
        use codec::sbc::{Allocation, ChannelMode, SbcParams};
        let text = r#"{"name":"WH","device_address":"00:A2:D0:00:00:02","codec":"sbc","sbc_bitpool":40,
            "sbc_min_bitpool":20,"sbcxq_min_bitpool":30,
            "sbc_blocks":8,"sbc_subbands":4,"sbc_allocation":"snr","sbc_channel_mode":"stereo"}"#;
        let p: Profile = serde_json::from_str(text).unwrap();
        let r = p.to_request(1023).unwrap();
        assert_eq!(r.sbc_bitpool, 40);
        assert_eq!(
            r.sbc,
            SbcParams {
                blocks: 8,
                subbands: 4,
                allocation: Some(Allocation::Snr),
                channel_mode: Some(ChannelMode::Stereo),
                min_bitpool: 0,
            }
        );
        // Each codec its own minimum bitpool
        assert_eq!(r.sbc_params(CodecKind::Sbc), SbcParams { min_bitpool: 20, ..r.sbc });
        assert_eq!(r.sbc_params(CodecKind::SbcXq).min_bitpool, 30);
        assert_eq!(r.sbc_params(CodecKind::Aac).min_bitpool, 0);
        assert_eq!((p.codec_values().sbc_min_bitpool, p.codec_values().sbcxq_min_bitpool), (20, 30));
        // Auto (the defaults) stays out of the file and gives today's setup
        let plain = Profile { device_address: "00:A2:D0:00:00:02".into(), ..Profile::default() };
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("sbc_"), "{json}");
        assert!(plain.to_request(1023).unwrap().sbc.is_default());
        // Unknown values are Auto
        let odd = Profile { sbc_allocation: "max".into(), sbc_channel_mode: "auto".into(), ..plain };
        assert!(odd.to_request(1023).unwrap().sbc.is_default());
        // Auto-select streams the defaults, the profile keeps its own
        assert!(p.as_auto().to_request(1023).unwrap().sbc.is_default());
        assert_eq!(p.sbc_blocks, 8);
    }

    fn named(names: &[&str]) -> Vec<Profile> {
        names.iter().map(|n| Profile { name: n.to_string(), ..Profile::default() }).collect()
    }

    fn names(list: &[Profile]) -> Vec<&str> {
        list.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn repeated_names_get_a_number() {
        let mut list = named(&["WH LDAC", "WH LDAC", "Buds AAC", "WH LDAC"]);
        assert_eq!(make_names_unique(&mut list), 2);
        assert_eq!(names(&list), ["WH LDAC", "WH LDAC (2)", "Buds AAC", "WH LDAC (3)"]);
        // Already unique: unchanged
        assert_eq!(make_names_unique(&mut list), 0);
        // A number some profile already has is skipped, also a later one's
        let mut list = named(&["A", "A (2)", "A", "A", "A (3)"]);
        assert_eq!(make_names_unique(&mut list), 2);
        assert_eq!(names(&list), ["A", "A (2)", "A (4)", "A (5)", "A (3)"]);
    }

    #[test]
    fn load_makes_names_unique_in_memory() {
        let dir = std::env::temp_dir().join(format!("bluecaster-profiles-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = r#"{"profiles":[{"name":"WH LDAC","device_address":"00:A2:D0:00:00:02","codec":"ldac"},
            {"name":"WH LDAC","device_address":"00:A2:D0:00:00:02","codec":"ldac","quality":"sq"}]}"#;
        std::fs::write(path(&dir), text).unwrap();
        let list = load(&dir);
        assert_eq!(names(&list), ["WH LDAC", "WH LDAC (2)"]);
        assert_eq!(list[1].quality, "sq");
        // The file itself is left until the next save
        assert_eq!(std::fs::read_to_string(path(&dir)).unwrap(), text);
        save(&dir, &list).unwrap();
        assert_eq!(names(&load(&dir)), ["WH LDAC", "WH LDAC (2)"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_audio_profile_gives_an_le_request() {
        let text = r#"{"profiles":[{"name":"Buds LE Audio","device_address":"00:a2:d0:00:00:12","device_name":"Buds",
            "capture_mode":"loopback","le_audio":true}]}"#;
        let f: ProfilesFile = serde_json::from_str(text).unwrap();
        let p = &f.profiles[0];
        assert!(p.le_audio);
        let r = p.to_request(1023).unwrap();
        assert_eq!(r.device, [0x00, 0xA2, 0xD0, 0x00, 0x00, 0x12]);
        // GMAP low latency (LC3 LL): off unless the profile says otherwise
        // (issue #86: LC3 is the default)
        assert_eq!(r.le, Some(bt_host::LeRequest { low_latency: false, ..bt_host::LeRequest::default() }));
        assert_eq!(r.source, SourceRequest::SystemLoopback);
        // Written back as v1 wrote it; a classic profile has no key
        let out = serde_json::to_value(p).unwrap();
        assert_eq!(out["le_audio"], true);
        assert!(out.get("le_low_latency").is_none());
        let ll = Profile { le_low_latency: true, ..p.clone() };
        assert!(ll.to_request(1023).unwrap().le.unwrap().low_latency);
        assert_eq!(serde_json::to_value(&ll).unwrap()["le_low_latency"], true);
        // Off as written before stays off
        let off: Profile = serde_json::from_value(
            serde_json::json!({"name": "Buds LE Audio", "le_audio": true, "le_low_latency": false}),
        )
        .unwrap();
        assert!(!off.le_low_latency);
        let classic = Profile { le_audio: false, ..p.clone() };
        assert!(serde_json::to_value(&classic).unwrap().get("le_audio").is_none());
        // A bad address is refused as for A2DP
        let bad = Profile { device_address: "nope".into(), ..p.clone() };
        assert_eq!(bad.to_request(1023), Err(ProfileError::Address("nope".into())));
    }

    #[test]
    fn headset_mic_round_trips_and_is_written_only_when_set() {
        let p = Profile::default();
        let out = serde_json::to_value(&p).unwrap();
        for key in ["headset_mic", "mic_device_id", "mic_device_name"] {
            assert!(out.get(key).is_none(), "{key}");
        }
        let on = Profile {
            headset_mic: "on".into(),
            mic_device_id: "{cable}".into(),
            mic_device_name: "CABLE Input".into(),
            ..p.clone()
        };
        let out = serde_json::to_value(&on).unwrap();
        assert_eq!(out["headset_mic"], "on");
        assert_eq!(out["mic_device_id"], "{cable}");
        assert_eq!(serde_json::from_value::<Profile>(out).unwrap(), on);
        assert!(p.only_headset_mic_differs(&on));
        assert!(!p.only_headset_mic_differs(&p));
        assert!(!p.only_headset_mic_differs(&Profile { codec: "sbc".into(), ..on.clone() }));
        assert!(p.only_headset_mic_differs(&Profile { headset_mic: "on".into(), ..p.clone() }));
        assert!(on.only_headset_mic_differs(&Profile { headset_mic: "auto".into(), ..on.clone() }));
        // The modes, the first development builds' bool, anything else off
        let mut j = serde_json::to_value(&on).unwrap();
        for (saved, mode) in [
            (serde_json::json!("on"), HeadsetMic::On),
            (serde_json::json!("auto"), HeadsetMic::Auto),
            (serde_json::json!("off"), HeadsetMic::Off),
            (serde_json::json!(true), HeadsetMic::On),
            (serde_json::json!(false), HeadsetMic::Off),
            (serde_json::json!("ON"), HeadsetMic::Off),
            (serde_json::json!(1), HeadsetMic::Off),
        ] {
            j["headset_mic"] = saved.clone();
            let back: Profile = serde_json::from_value(j.clone()).unwrap();
            assert_eq!(back.headset_mic_mode(), mode, "{saved}");
            assert_eq!(back.headset_mic, mode.as_str(), "{saved}");
        }
        let auto = Profile { headset_mic: "auto".into(), ..on.clone() };
        assert_eq!(serde_json::to_value(&auto).unwrap()["headset_mic"], "auto");
        // Muted: Off in force, the mode kept for unmuting; only a change
        // of the microphone
        assert!(serde_json::to_value(&auto).unwrap().get("mic_muted").is_none());
        let muted = Profile { mic_muted: true, ..auto.clone() };
        assert_eq!(muted.headset_mic_mode(), HeadsetMic::Off);
        assert!(auto.only_headset_mic_differs(&muted));
        let back: Profile = serde_json::from_value(serde_json::to_value(&muted).unwrap()).unwrap();
        assert!(back.mic_muted && back.headset_mic == "auto");
    }

    #[test]
    fn the_microphone_codec_chosen_counts_only_with_auto_select_off() {
        let p = Profile::default();
        let out = serde_json::to_value(&p).unwrap();
        assert!(out.get("voice_codec").is_none() && out.get("mic_rate").is_none());
        assert_eq!(p.voice_codec_in_force(false), None);
        let msbc = Profile { voice_codec: "msbc".into(), ..p.clone() };
        assert_eq!(serde_json::to_value(&msbc).unwrap()["voice_codec"], "msbc");
        assert_eq!(msbc.voice_codec_in_force(false), Some(voice_link::VoiceCodec::Msbc));
        // Auto-select on: Auto, the choice kept
        assert_eq!(msbc.voice_codec_in_force(true), None);
        assert_eq!(Profile { voice_codec: "lc3".into(), ..p.clone() }.voice_codec_in_force(false), None);
        // A change of it alone goes the headset microphone's way
        assert!(p.only_headset_mic_differs(&msbc));
        assert!(!p.only_headset_mic_differs(&Profile { codec: "sbc".into(), ..msbc.clone() }));
        // LE Audio: the microphone's rate
        let le = Profile { le_audio: true, ..p.clone() };
        let rate = Profile { mic_rate: 16000, ..le.clone() };
        assert_eq!(serde_json::to_value(&rate).unwrap()["mic_rate"], 16000);
        assert_eq!((rate.mic_rate_in_force(false), rate.mic_rate_in_force(true)), (Some(16000), None));
        assert_eq!(le.mic_rate_in_force(false), None);
        assert!(le.only_headset_mic_differs(&rate));
        // Neither applies to the other kind of device
        assert_eq!(Profile { mic_rate: 16000, ..p.clone() }.mic_rate_in_force(false), None);
        assert_eq!(Profile { voice_codec: "msbc".into(), ..le }.voice_codec_in_force(false), None);
    }

    #[test]
    fn mic_device_feeding_the_capture_is_refused() {
        let loop_key = Some("error.headset_mic_loop");
        let p = Profile { headset_mic: "on".into(), mic_device_id: "{cable}".into(), ..Profile::default() };
        // System Loopback: only when the cable is the default output
        assert_eq!(p.mic_device_conflicts("{speakers}"), None);
        assert_eq!(p.mic_device_conflicts(""), None);
        assert_eq!(p.mic_device_conflicts("{cable}"), loop_key);
        // The captured virtual or output device
        let virt = Profile { capture_mode: "virtual".into(), audio_device_id: "{cable}".into(), ..p.clone() };
        assert_eq!(virt.mic_device_conflicts("{speakers}"), loop_key);
        let other = Profile { audio_device_id: "{cable 2}".into(), ..virt.clone() };
        assert_eq!(other.mic_device_conflicts("{cable}"), None);
        let output = Profile { capture_mode: "output".into(), ..virt.clone() };
        assert_eq!(output.mic_device_conflicts(""), loop_key);
        // An application, and no output chosen
        let app = Profile { capture_mode: "app".into(), app_exe: "game.exe".into(), ..p.clone() };
        assert_eq!(app.mic_device_conflicts("{cable}"), None);
        let none = Profile { mic_device_id: String::new(), ..p.clone() };
        assert_eq!(none.mic_device_conflicts(""), None);
    }

    fn of(name: &str, address: &str, codec: &str) -> Profile {
        Profile { name: name.into(), device_address: address.into(), codec: codec.into(), ..Profile::default() }
    }

    #[test]
    fn consolidate_keeps_one_profile_per_device() {
        const WH: &str = "00:A2:D0:00:00:02";
        const BUDS: &str = "00:A2:D0:00:00:12";
        let mut list = vec![
            of("WH | Auto", WH, "auto"),
            of("Buds | AAC", BUDS, "aac"),
            Profile { quality: "sq".into(), sample_rate: 48000, ..of("WH | LDAC", WH, "ldac") },
            Profile { abr: true, ..of("WH AAC", &WH.to_lowercase(), "aac") },
            of("WH LDAC again", WH, "ldac"),
            Profile { le_audio: true, ..of("Buds LE Audio", BUDS, "auto") },
            of("No address", "", "sbc"),
        ];
        // The last used one is kept, at the place of the device's first
        let folded = consolidate(&mut list, "WH AAC");
        assert_eq!(folded, ["WH | Auto", "WH | LDAC", "WH LDAC again"]);
        assert_eq!(names(&list), ["WH AAC", "Buds | AAC", "Buds LE Audio", "No address"]);
        let wh = &list[0];
        assert_eq!(wh.codec, "aac");
        assert!(wh.abr);
        // The first LDAC profile's settings; Auto has none of its own
        assert_eq!(wh.codec_settings.keys().collect::<Vec<_>>(), ["ldac"]);
        assert_eq!(wh.codec_settings["ldac"].quality, "sq");
        assert_eq!(wh.codec_settings["ldac"].sample_rate, 48000);
        // Nothing more to fold
        assert!(consolidate(&mut list, "WH AAC").is_empty());
        // Without a last profile of the device: the first
        let mut list = vec![of("A", WH, "aac"), of("B", WH, "ldac")];
        assert_eq!(consolidate(&mut list, "Buds | AAC"), ["B"]);
        assert_eq!(names(&list), ["A"]);
        assert!(list[0].codec_settings.contains_key("ldac"));
    }

    #[test]
    fn what_a_stream_can_take_without_starting_again() {
        let p = Profile { name: "A".into(), device_address: "AA:BB:CC:DD:EE:FF".into(), ..Default::default() };
        let bitpool = Profile { sbc_bitpool: 30, ..p.clone() };
        assert!(p.only_bitpools_differ(&bitpool));
        assert!(!p.only_bitpools_differ(&p));
        assert!(!p.only_bitpools_differ(&Profile { sample_rate: 44100, ..bitpool.clone() }));
        let mut kept = p.clone();
        kept.codec_settings.insert("ldac".into(), CodecValues::default());
        assert!(p.only_kept_settings_differ(&kept));
        assert!(!p.only_kept_settings_differ(&p));
        assert!(!p.only_kept_settings_differ(&Profile { sbc_bitpool: 30, ..kept }));
        // "Show only options it supports" turned, nothing else (issue #86)
        let unlisted = Profile { unlisted_options: !p.unlisted_options, ..p.clone() };
        assert!(p.only_unlisted_differs(&unlisted));
        let eq = Profile {
            media_eq: crate::media_eq::MediaEq { enabled: true, bands: vec![dsp::EqBand::peaking(1000.0, 3.0, 1.0)] },
            ..p.clone()
        };
        assert!(p.only_media_eq_differs(&eq));
        assert!(!p.only_media_eq_differs(&p));
        assert!(!p.only_media_eq_differs(&Profile { sbc_bitpool: 30, ..eq.clone() }));
        // The EQ and the microphone processing saved together: both in place
        let both = Profile {
            mic_processing: ipc::MicProcessing { enabled: true, gain_db: 6, ..Default::default() },
            ..eq.clone()
        };
        assert!(p.only_live_processing_differs(&eq));
        assert!(p.only_live_processing_differs(&both));
        assert!(!p.only_live_processing_differs(&p));
        assert!(!p.only_live_processing_differs(&Profile { sbc_bitpool: 30, ..both }));
        assert!(!p.only_unlisted_differs(&p));
        assert!(!p.only_unlisted_differs(&Profile { sbc_bitpool: 30, ..unlisted }));
    }

    #[test]
    fn codec_settings_written_only_when_there_are_some() {
        let mut p = of("WH", "00:A2:D0:00:00:02", "aac");
        assert!(serde_json::to_value(&p).unwrap().get("codec_settings").is_none());
        p.codec_settings.insert("ldac".into(), CodecValues { quality: "mq".into(), ..CodecValues::default() });
        let out = serde_json::to_value(&p).unwrap();
        assert_eq!(
            out["codec_settings"]["ldac"],
            serde_json::json!({"quality": "mq", "sample_rate": 0, "bit_depth": 0, "abr": false})
        );
        let back: Profile = serde_json::from_value(out).unwrap();
        assert_eq!(back, p);
    }
}
