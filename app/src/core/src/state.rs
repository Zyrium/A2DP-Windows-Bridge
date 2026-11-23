//! The resident core's state as the UI sees it (an ipc::Snapshot with a
//! revision), the events that change it, and the requests that act on it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bluecaster_core::default_input::{DefaultInput, Want as InputWant};
use bluecaster_core::diag::AudioRates;
use bluecaster_core::media_eq::{self, MediaEq};
use bluecaster_core::paths;
use bluecaster_core::profiles::{self, Profile as CoreProfile};
use bluecaster_core::receiver::{self, ReceiverClient};
use bluecaster_core::settings::Settings as CoreSettings;
use bluecaster_core::vendor;
use bluecaster_core::{
    CodecStopKind, LeStreamDetails, ScannedDevice, Service, ServiceEvent, SourceRequest, StreamRequest,
    StreamState as CoreStreamState, VolumeSync, devices, le_sets,
};
use bt_host::chips::{self, ChipVendor};
use bt_host::{HeadsetMic, LinkStats};
use ipc::{
    AdapterInfo, AdapterState, AdapterStatus, AudioApp, AudioDevice, CaptureState, DevLog, DevOverrides, Device,
    DiagInfo, DriverKind, ErrorInfo, Event, FirmwareFile, LeStreamInfo, LinkTotals, LoaderStatus, Method, MixerApp,
    Msg, OtherRadio, ProbeResult, Profile, ReceiverConnection, ReceiverState, ScriptCheck, ScriptResult, Settings,
    SettingsApplied, Snapshot, SourceInfo, StreamState, StreamStatus, UpdateState, WifiAccessPoint,
    WindowsPairedDevice,
};
use serde_json::Value;
use tokio::sync::broadcast;

/// Work of a request that blocks (scan, driver change), run off the runtime
type BlockingJob = Box<dyn FnOnce(&Arc<CoreState>) -> Result<Value, ErrorInfo> + Send>;

/// A tray balloon to show (on the tray's thread)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalloonRequest {
    pub title: String,
    pub text: String,
    /// Opened by a click on it
    pub url: Option<String>,
}

/// settings.json key (kept among the unknown ones): the "still running in
/// the tray" hint was shown once
const TRAY_HINT_SHOWN: &str = "tray_hint_shown";

/// The low-battery balloon is shown again only after the level was back
/// this much above the setting's level (charged)
const BATTERY_REARM_MARGIN: u8 = 5;

/// How long a restart for a save waits for a newer request to take over
/// ([`CoreState::restart_profile`])
const RESTART_COALESCE: std::time::Duration = std::time::Duration::from_millis(250);

/// How long the "connected" balloon waits for the battery: HFP is opened once
/// the stream is established, so the level often comes just after it
const BATTERY_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// How long after a cable came its set-up is done again at each look at
/// Windows' outputs: its setup may make it the default output only after it
/// is listed
const CABLES_SETTLE: std::time::Duration = std::time::Duration::from_secs(15);

/// Whether a battery report shows the low-battery balloon: once when the
/// level is below `low` (the setting; also the first report of a
/// connection), again only after it was back [`BATTERY_REARM_MARGIN`]
/// above. Never when `low` is 0. `warned` is cleared for each connection.
fn battery_low_alert(warned: &mut bool, level: u8, low: u8) -> bool {
    if level >= low.saturating_add(BATTERY_REARM_MARGIN) {
        *warned = false;
        return false;
    }
    if level < low && !*warned {
        *warned = true;
        return true;
    }
    false
}

/// The streamed profile's device: its name, else its address
fn streamed_device(s: &Snapshot) -> Option<&str> {
    s.stream.profile.as_ref().and_then(|n| s.profiles.iter().find(|p| &p.name == n)).map(|p| device_name(&s.devices, p))
}

/// What users call a profile (issue #86: never its own name): its saved
/// device's name (renamed after the profile was made, too), else the name
/// the profile kept, else the address
fn device_name<'a>(devices: &'a [Device], p: &'a Profile) -> &'a str {
    devices
        .iter()
        .find(|d| d.saved && d.le_audio == p.le_audio && d.address.eq_ignore_ascii_case(&p.device_address))
        .map(|d| d.name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(if p.device_name.is_empty() { p.device_address.as_str() } else { p.device_name.as_str() })
}

/// What the tray shows
#[derive(Debug, Clone)]
pub struct TrayView {
    pub streaming: bool,
    pub tooltip: String,
    pub profiles: Vec<String>,
    /// The menu's item for each of `profiles`: its device, as the flyout
    /// lists it
    pub labels: Vec<String>,
    pub active_profile: Option<String>,
    pub language: String,
}

pub struct CoreState {
    snapshot: Mutex<(u64, Snapshot)>,
    events: broadcast::Sender<(u64, Arc<Event>)>,
    pub service: Arc<Service>,
    /// Clients subscribed to link samples
    pub link_subscribers: AtomicUsize,
    /// Clients subscribed to the receiver test's state
    pub receiver_subscribers: AtomicUsize,
    /// The peer receiver test's connection: kept while no window is open
    pub receiver: ReceiverClient,
    pub started: Instant,
    /// What the stream moved over the last second (the sampler), for the
    /// flow's "Audio sending" step
    rates: Mutex<Option<AudioRates>>,
    /// The profile the last stream.start asked for: the session stopped for
    /// it reports Idle after it, which clears the snapshot's profile
    starting_profile: Mutex<Option<String>>,
    /// The message a microphone note replaced (a codec's fallback reason),
    /// back once the note is out of date
    mic_note_over: Mutex<Option<Msg>>,
    /// The low-battery balloon was shown for this connection
    battery_warned: AtomicBool,
    /// A new connection started: its balloon is shown once it streams with
    /// the codec settled (not after a reconnect of the same connection)
    connect_announce: AtomicBool,
    /// Until when the balloon of this connection waits for the battery
    /// ([`BATTERY_WAIT`])
    battery_deadline: Mutex<Option<Instant>>,
    /// The session starting is the profile in use started again with what
    /// was saved ([`CoreState::restart_profile`]): no balloon for it
    quiet_connect: AtomicBool,
    /// Set by [`CoreState::restart_profile`] for the [`CoreState::begin_stream`]
    /// it calls
    restarting: AtomicBool,
    /// The headset microphone's cable as Windows' default input while its
    /// device is connected ([`default_input_want`])
    default_input: DefaultInput,
    /// When a cable last came and which were there before it
    /// ([`CoreState::watch_cables`], for [`CABLES_SETTLE`])
    cables_came: Mutex<Option<(Instant, profiles::CableSet)>>,
    tray: Box<dyn Fn(TrayView) + Send + Sync>,
    quit: Box<dyn Fn() + Send + Sync>,
    balloon: Box<dyn Fn(BalloonRequest) + Send + Sync>,
}

pub fn msg(m: &bluecaster_core::Msg) -> Msg {
    Msg { key: m.key.to_string(), args: m.args.clone() }
}

pub(crate) fn err(code: &str, key: &str) -> ErrorInfo {
    ErrorInfo { code: code.into(), message: Msg::new(key) }
}

/// A request whose parameters do not make sense (ipc.error.bad_request)
pub(crate) fn bad_request(code: &str, method: &str, why: String) -> ErrorInfo {
    ErrorInfo {
        code: code.into(),
        message: Msg { key: "ipc.error.bad_request".into(), args: vec![method.into(), why] },
    }
}

/// Developer mode is on (the HID host's requests)
fn developer(state: &CoreState) -> Result<(), ErrorInfo> {
    if state.service.settings().debug_mode { Ok(()) } else { Err(err("developer.off", "ipc.error.developer_only")) }
}

/// A result as JSON (plain data: it cannot fail)
fn json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn to_ipc_profile(p: &CoreProfile, legacy: u16) -> Profile {
    Profile {
        name: p.name.clone(),
        device_address: p.device_address.clone(),
        device_name: p.device_name.clone(),
        codec: p.codec.clone(),
        quality: p.quality.clone(),
        abr: p.abr,
        lhdcv5_low_latency: p.lhdcv5_low_latency,
        codec_fallback: p.codec_fallback,
        codec_forced: p.codec_forced,
        unlisted_options: p.unlisted_options,
        sample_rate: p.sample_rate,
        bit_depth: p.bit_depth,
        capture_mode: p.capture_mode.clone(),
        audio_device_id: p.audio_device_id.clone(),
        audio_device_name: p.audio_device_name.clone(),
        auto_switch_device: p.auto_switch_device,
        app_exe: p.app_exe.clone(),
        app_name: p.app_name.clone(),
        volume_sync: p.volume_sync.clone(),
        volume_device_id: p.volume_device_id.clone(),
        volume_device_name: p.volume_device_name.clone(),
        max_media_payload: p.max_media_payload.unwrap_or(legacy),
        le_audio: p.le_audio,
        le_low_latency: p.le_low_latency,
        headset_mic: p.headset_mic.clone(),
        mic_device_id: p.mic_device_id.clone(),
        mic_device_name: p.mic_device_name.clone(),
        mic_muted: p.mic_muted,
        mic_default_input: p.mic_default_input,
        voice_codec: p.voice_codec.clone(),
        mic_rate: p.mic_rate,
        mic_processing: p.mic_processing,
        sbc_bitpool: p.sbc_bitpool,
        sbcxq_bitpool: p.sbcxq_bitpool,
        sbc_min_bitpool: p.sbc_min_bitpool,
        sbcxq_min_bitpool: p.sbcxq_min_bitpool,
        sbc_blocks: p.sbc_blocks,
        sbc_subbands: p.sbc_subbands,
        sbc_allocation: p.sbc_allocation.clone(),
        sbc_channel_mode: p.sbc_channel_mode.clone(),
        codec_settings: p.codec_settings.iter().map(|(c, v)| (c.clone(), to_ipc_codec_values(v))).collect(),
        codec_order: p.codec_order.clone(),
        media_eq: p.media_eq.to_ipc(),
    }
}

fn to_ipc_codec_values(v: &profiles::CodecValues) -> ipc::CodecValues {
    ipc::CodecValues {
        quality: v.quality.clone(),
        sample_rate: v.sample_rate,
        bit_depth: v.bit_depth,
        abr: v.abr,
        lhdcv5_low_latency: v.lhdcv5_low_latency,
        sbc_bitpool: v.sbc_bitpool,
        sbcxq_bitpool: v.sbcxq_bitpool,
        sbc_min_bitpool: v.sbc_min_bitpool,
        sbcxq_min_bitpool: v.sbcxq_min_bitpool,
        sbc_blocks: v.sbc_blocks,
        sbc_subbands: v.sbc_subbands,
        sbc_allocation: v.sbc_allocation.clone(),
        sbc_channel_mode: v.sbc_channel_mode.clone(),
        unlisted_options: v.unlisted_options,
    }
}

fn from_ipc_codec_values(v: &ipc::CodecValues) -> profiles::CodecValues {
    profiles::CodecValues {
        quality: v.quality.clone(),
        sample_rate: v.sample_rate,
        bit_depth: v.bit_depth,
        abr: v.abr,
        lhdcv5_low_latency: v.lhdcv5_low_latency,
        sbc_bitpool: v.sbc_bitpool,
        sbcxq_bitpool: v.sbcxq_bitpool,
        sbc_min_bitpool: v.sbc_min_bitpool,
        sbcxq_min_bitpool: v.sbcxq_min_bitpool,
        sbc_blocks: v.sbc_blocks,
        sbc_subbands: v.sbc_subbands,
        sbc_allocation: v.sbc_allocation.clone(),
        sbc_channel_mode: v.sbc_channel_mode.clone(),
        unlisted_options: v.unlisted_options,
    }
}

/// The microphone's processing within the ranges the chain takes (a gate
/// below its lowest threshold is off)
fn clamp_mic_processing(p: ipc::MicProcessing) -> ipc::MicProcessing {
    ipc::MicProcessing {
        enabled: p.enabled,
        gain_db: p.gain_db.clamp(-12, 24),
        suppression: p.suppression.min(100),
        gate_db: p.gate_db.clamp(ipc::MicProcessing::GATE_OFF_DB, -20),
    }
}

/// Applies the UI's profile onto the stored one (unknown keys kept)
fn merge_profile(base: Option<&CoreProfile>, p: &Profile) -> CoreProfile {
    let mut c = base.cloned().unwrap_or_default();
    c.name = p.name.clone();
    c.device_address = p.device_address.to_uppercase();
    c.device_name = p.device_name.clone();
    c.codec = p.codec.clone();
    c.quality = p.quality.clone();
    c.abr = p.abr;
    c.lhdcv5_low_latency = p.lhdcv5_low_latency;
    c.codec_fallback = p.codec_fallback;
    c.codec_forced = p.codec_forced;
    c.unlisted_options = p.unlisted_options;
    c.sample_rate = p.sample_rate;
    c.bit_depth = p.bit_depth;
    c.capture_mode = p.capture_mode.clone();
    c.audio_device_id = p.audio_device_id.clone();
    c.audio_device_name = p.audio_device_name.clone();
    c.auto_switch_device = p.auto_switch_device;
    c.app_exe = p.app_exe.clone();
    c.app_name = p.app_name.clone();
    c.volume_sync = p.volume_sync.clone();
    c.volume_device_id = p.volume_device_id.clone();
    c.volume_device_name = p.volume_device_name.clone();
    c.max_media_payload = Some(bt_host::media::clamp_media_payload_limit(p.max_media_payload as u32));
    c.le_audio = p.le_audio;
    c.le_low_latency = p.le_low_latency;
    // Normalized: anything but "on" / "auto" is "off"
    c.headset_mic = HeadsetMic::parse(&p.headset_mic).as_str().into();
    c.mic_device_id = p.mic_device_id.clone();
    c.mic_device_name = p.mic_device_name.clone();
    c.mic_muted = p.mic_muted;
    c.mic_default_input = p.mic_default_input;
    // A call codec not known reads as none chosen
    c.voice_codec = bluecaster_core::VoiceCodec::from_name(&p.voice_codec).map_or(String::new(), |v| v.name().into());
    c.mic_rate = p.mic_rate;
    c.mic_processing = clamp_mic_processing(p.mic_processing);
    c.sbc_bitpool = p.sbc_bitpool;
    c.sbcxq_bitpool = p.sbcxq_bitpool;
    c.sbc_min_bitpool = p.sbc_min_bitpool;
    c.sbcxq_min_bitpool = p.sbcxq_min_bitpool;
    c.sbc_blocks = p.sbc_blocks;
    c.sbc_subbands = p.sbc_subbands;
    c.sbc_allocation = p.sbc_allocation.clone();
    c.sbc_channel_mode = p.sbc_channel_mode.clone();
    // The codec in use has its settings in the fields above, by card: aptX
    // Adaptive's own stay while its LL card is chosen, and LL's while it is
    // not (issue #86)
    let card = profiles::card_key(&p.codec, &p.quality);
    c.codec_settings = p
        .codec_settings
        .iter()
        .filter(|(k, _)| k.as_str() != card)
        .map(|(k, v)| (k.clone(), from_ipc_codec_values(v)))
        .collect();
    // The codecs' keys, each once (the cards' order: FastStream and Opus 05
    // too, which Auto and the fallback never take); the UI sends none for
    // the default order; aptX Adaptive LL's card has its own place
    c.media_eq = MediaEq::from_ipc(&p.media_eq);
    c.codec_order = Vec::new();
    let keys = p.codec_order.iter().filter_map(|k| {
        if k.eq_ignore_ascii_case(profiles::APTXAD_LL_CARD) {
            Some(profiles::APTXAD_LL_CARD)
        } else {
            codec::CodecKind::from_key(k).map(codec::CodecKind::key)
        }
    });
    for k in keys {
        if !c.codec_order.iter().any(|x| x == k) {
            c.codec_order.push(k.to_string());
        }
    }
    c
}

fn to_ipc_settings(s: &CoreSettings) -> Settings {
    Settings {
        language: s.language.clone(),
        theme: s.theme.clone(),
        start_with_windows: s.start_with_windows,
        start_minimized: s.start_minimized,
        minimize_to_tray: s.minimize_to_tray,
        tray_click_opens_window: s.tray_click_opens_window,
        flyout_with_quick_settings: s.flyout_with_quick_settings,
        notify_connected: s.notify_connected,
        battery_low_percent: s.battery_low_percent,
        setup_done: s.setup_done,
        check_updates_on_startup: s.check_updates_on_startup,
        include_prereleases: s.include_prereleases,
        include_alphas: s.include_alphas,
        media_keys: s.media_keys,
        auto_reconnect: s.auto_reconnect,
        vb_cable_note_hidden: s.vb_cable_note_hidden,
        hifi_cable_note_hidden: s.hifi_cable_note_hidden,
        cables_dialog_shown: s.cables_dialog_shown,
        unlisted_options_warned: s.unlisted_options_warned,
        volume_sync: s.volume_sync,
        mute_output: s.mute_output,
        volume_mixer: s.volume_mixer,
        all_audio_devices: s.all_audio_devices,
        developer_mode: s.debug_mode,
        dev_overrides: DevOverrides {
            rtp_marker: s.dev_overrides.rtp_marker,
            packet_dump: s.dev_overrides.packet_dump,
            aac_config_bitrate0: s.dev_overrides.aac_config_bitrate0,
            aac_bitrate_kbps: s.dev_overrides.aac_bitrate_kbps,
            aac_no_peak: s.dev_overrides.aac_no_peak,
            aac_no_afterburner: s.dev_overrides.aac_no_afterburner,
            aac_latm_v0: s.dev_overrides.aac_latm_v0,
            auto_codec_settings: s.dev_overrides.auto_codec_settings,
            match_source: s.dev_overrides.match_source,
            ignore_sink_max: s.dev_overrides.ignore_sink_max,
            queue_max_wait_ms: s.dev_overrides.queue_max_wait_ms,
            encode_at_send: s.dev_overrides.encode_at_send,
            keep_bitrate_on_slow_link: s.dev_overrides.keep_bitrate_on_slow_link,
            auto_mic_tick_poll: s.dev_overrides.auto_mic_tick_poll,
        },
        dev_vendors: s
            .dev_vendors
            .iter()
            .map(|(id, o)| {
                let mode = match o.mode {
                    vendor::Mode::Auto => ipc::VendorMode::Auto,
                    vendor::Mode::Always => ipc::VendorMode::Always,
                    vendor::Mode::Never => ipc::VendorMode::Never,
                };
                (id.clone(), ipc::VendorOverride { mode, options: o.options.clone() })
            })
            .collect(),
        dev_log: DevLog {
            keep_days: s.dev_log.keep_days,
            day_cap_mb: s.dev_log.day_cap_mb,
            always_verbose: match s.dev_log.always_verbose.as_str() {
                "debug" => Some(ipc::LogLevel::Debug),
                "trace" => Some(ipc::LogLevel::Trace),
                _ => None,
            },
            health_every_interval: s.dev_log.health_every_interval,
        },
        dev_debug: ipc::DevDebug {
            mtk_iso_bulk: s.dev_debug.mtk_iso_bulk,
            mtk_iso_pad: s.dev_debug.mtk_iso_pad,
            mtk_command: s.dev_debug.mtk_command.clone(),
            mtk_iso_queue: s.dev_debug.mtk_iso_queue,
            mtk_command_before_reset: s.dev_debug.mtk_command_before_reset,
            mtk_register: s.dev_debug.mtk_register.clone(),
            le_keys_to_windows: s.dev_debug.le_keys_to_windows,
            le_latency_c_to_p_ms: s.dev_debug.le_latency_c_to_p_ms,
            le_latency_p_to_c_ms: s.dev_debug.le_latency_p_to_c_ms,
            le_rtn_c_to_p: s.dev_debug.le_rtn_c_to_p,
            le_rtn_p_to_c: s.dev_debug.le_rtn_p_to_c,
            le_interleaved: s.dev_debug.le_interleaved,
            le_mic_preset: s.dev_debug.le_mic_preset.clone(),
            le_mic_retry_same_link: s.dev_debug.le_mic_retry_same_link,
        },
        bt_chip_pid: s.bt_chip_pid,
        bt_chip_fw_stem: s.bt_chip_fw_stem.clone(),
        realtek_custom_firmware: s.realtek_custom_firmware,
        adapter_id: s.adapter_id.clone(),
        auracast_adapter_id: s.auracast_adapter_id.clone(),
        afh: s.afh.clone(),
        remote_sink: s.remote_sink.clone(),
        le_audio_experimental: s.le_audio_experimental,
        le_audio_low_latency: s.le_audio_low_latency,
        auto_codec: s.auto_codec,
        hid_host: s.hid_host,
    }
}

/// The Hi-Fi Cable's playback device ("Hi-Fi Cable Input"): the headset
/// microphone's cable (issue #86)
fn hifi_cable(devices: &[audio_capture::DeviceInfo]) -> Option<&audio_capture::DeviceInfo> {
    devices.iter().find(|d| profiles::is_hifi_cable(&d.name))
}

/// Service mode (crate::rules): whether settings.set with `s` changes
/// anything only an elevated client may change: developer mode and what
/// it sets, the chip, its firmware and radio settings, the receiver host.
/// Not the adapter to use (or the Auracast adapter): a choice among the
/// adapters an administrator already gave to BlueCaster.
pub(crate) fn admin_settings_change(base: &CoreSettings, s: &Settings) -> bool {
    let m = merge_settings(base, s);
    m.debug_mode != base.debug_mode
        || m.dev_overrides != base.dev_overrides
        || m.dev_vendors != base.dev_vendors
        || m.dev_log != base.dev_log
        || m.dev_debug != base.dev_debug
        || m.bt_chip_pid != base.bt_chip_pid
        || m.bt_chip_fw_stem != base.bt_chip_fw_stem
        || m.realtek_custom_firmware != base.realtek_custom_firmware
        || m.afh != base.afh
        || m.remote_sink != base.remote_sink
}

#[cfg(test)]
pub(crate) fn test_ipc_settings(s: &CoreSettings) -> Settings {
    to_ipc_settings(s)
}

#[cfg(test)]
pub(crate) fn test_ipc_profile() -> Profile {
    to_ipc_profile(&CoreProfile::default(), 0)
}

fn merge_settings(base: &CoreSettings, s: &Settings) -> CoreSettings {
    let mut c = base.clone();
    c.language = s.language.clone();
    c.theme = s.theme.clone();
    c.start_with_windows = s.start_with_windows;
    c.start_minimized = s.start_minimized;
    c.minimize_to_tray = s.minimize_to_tray;
    c.tray_click_opens_window = s.tray_click_opens_window;
    c.flyout_with_quick_settings = s.flyout_with_quick_settings;
    c.notify_connected = s.notify_connected;
    c.battery_low_percent = s.battery_low_percent;
    c.setup_done = s.setup_done;
    c.check_updates_on_startup = s.check_updates_on_startup;
    c.include_prereleases = s.include_prereleases;
    c.include_alphas = s.include_alphas;
    c.media_keys = s.media_keys;
    c.auto_reconnect = s.auto_reconnect;
    c.vb_cable_note_hidden = s.vb_cable_note_hidden;
    c.hifi_cable_note_hidden = s.hifi_cable_note_hidden;
    c.cables_dialog_shown = s.cables_dialog_shown;
    c.unlisted_options_warned = s.unlisted_options_warned;
    c.volume_sync = s.volume_sync;
    c.mute_output = s.mute_output;
    c.volume_mixer = s.volume_mixer;
    c.all_audio_devices = s.all_audio_devices;
    c.debug_mode = s.developer_mode;
    c.dev_overrides = codec::dev::DevOverrides {
        rtp_marker: s.dev_overrides.rtp_marker,
        packet_dump: s.dev_overrides.packet_dump,
        aac_config_bitrate0: s.dev_overrides.aac_config_bitrate0,
        // A bit rate AAC can use at all: 0 (off) or 32..=512 kbit/s
        aac_bitrate_kbps: match s.dev_overrides.aac_bitrate_kbps {
            0 => 0,
            k => k.clamp(32, 512),
        },
        aac_no_peak: s.dev_overrides.aac_no_peak,
        aac_no_afterburner: s.dev_overrides.aac_no_afterburner,
        aac_latm_v0: s.dev_overrides.aac_latm_v0,
        auto_codec_settings: s.dev_overrides.auto_codec_settings,
        match_source: s.dev_overrides.match_source,
        ignore_sink_max: s.dev_overrides.ignore_sink_max,
        // 0 (the default) or a limit the queue can hold
        queue_max_wait_ms: match s.dev_overrides.queue_max_wait_ms {
            0 => 0,
            ms => ms.clamp(codec::dev::QUEUE_WAIT_MIN, codec::dev::QUEUE_WAIT_MAX),
        },
        encode_at_send: s.dev_overrides.encode_at_send,
        keep_bitrate_on_slow_link: s.dev_overrides.keep_bitrate_on_slow_link,
        auto_mic_tick_poll: s.dev_overrides.auto_mic_tick_poll,
    };
    // Known vendors and options only, defaults left out
    c.dev_vendors = s
        .dev_vendors
        .iter()
        .filter_map(|(id, o)| {
            let v = vendor::vendor(id)?;
            let mode = match o.mode {
                ipc::VendorMode::Auto => vendor::Mode::Auto,
                ipc::VendorMode::Always => vendor::Mode::Always,
                ipc::VendorMode::Never => vendor::Mode::Never,
            };
            let options = o.options.iter().filter(|x| v.options.contains(&x.as_str())).cloned().collect();
            let o = vendor::Override { mode, options };
            (!o.is_default()).then(|| (id.clone(), o))
        })
        .collect();
    c.dev_log = bluecaster_core::settings::DevLog {
        keep_days: s.dev_log.keep_days,
        day_cap_mb: s.dev_log.day_cap_mb,
        always_verbose: match s.dev_log.always_verbose {
            Some(ipc::LogLevel::Debug) => "debug".into(),
            Some(ipc::LogLevel::Trace) => "trace".into(),
            _ => String::new(),
        },
        health_every_interval: s.dev_log.health_every_interval,
    }
    .clamped();
    c.dev_debug = bluecaster_core::settings::DevDebug {
        mtk_iso_bulk: s.dev_debug.mtk_iso_bulk,
        mtk_iso_pad: s.dev_debug.mtk_iso_pad.min(hci_transport::iso_intr::PAD_MAX as u16),
        mtk_command: s.dev_debug.mtk_command.trim().to_string(),
        mtk_iso_queue: s.dev_debug.mtk_iso_queue.min(12),
        mtk_command_before_reset: s.dev_debug.mtk_command_before_reset,
        mtk_register: s.dev_debug.mtk_register.trim().to_string(),
        le_keys_to_windows: s.dev_debug.le_keys_to_windows,
        le_latency_c_to_p_ms: s.dev_debug.le_latency_c_to_p_ms.min(4000),
        le_latency_p_to_c_ms: s.dev_debug.le_latency_p_to_c_ms.min(4000),
        le_rtn_c_to_p: s.dev_debug.le_rtn_c_to_p.map(|r| r.min(15)),
        le_rtn_p_to_c: s.dev_debug.le_rtn_p_to_c.map(|r| r.min(15)),
        le_interleaved: s.dev_debug.le_interleaved,
        le_mic_preset: s.dev_debug.le_mic_preset.trim().to_string(),
        le_mic_retry_same_link: s.dev_debug.le_mic_retry_same_link,
    };
    c.bt_chip_pid = s.bt_chip_pid;
    c.bt_chip_fw_stem = s.bt_chip_fw_stem.clone();
    c.realtek_custom_firmware = s.realtek_custom_firmware;
    c.adapter_id = s.adapter_id.trim().to_string();
    c.auracast_adapter_id = s.auracast_adapter_id.trim().to_string();
    c.afh = s.afh.clone();
    c.remote_sink = s.remote_sink.clone();
    c.le_audio_experimental = s.le_audio_experimental;
    c.le_audio_low_latency = s.le_audio_low_latency;
    c.auto_codec = s.auto_codec;
    // Keyboards and mice through BlueCaster: only hid.enable turns them on
    // (issue #413). Developer mode off turns them off for good, so that
    // turning it on again later does not bring them back unasked.
    c.hid_host = base.hid_host && s.hid_host && c.debug_mode;
    c
}

fn to_ipc_hid_devices(list: Vec<bluecaster_core::HidDevice>) -> Vec<ipc::HidDevice> {
    list.into_iter()
        .map(|d| ipc::HidDevice { address: d.address, name: d.name, connected: d.connected, le: d.le })
        .collect()
}

fn to_ipc_hid_passkey(p: Option<(String, String)>) -> Option<ipc::HidPasskey> {
    p.map(|(address, passkey)| ipc::HidPasskey { address, passkey })
}

/// The devices being added, with the time left as of now
fn to_ipc_hid_checks(service: &Service) -> Vec<ipc::HidCheck> {
    let ms = |d: std::time::Duration| d.as_millis().min(u128::from(u32::MAX)) as u32;
    service
        .hid_checks()
        .into_iter()
        .map(|c| ipc::HidCheck {
            address: c.address,
            name: c.name,
            kind: c.kind.to_string(),
            waiting: c.waiting,
            remaining_ms: ms(c.remaining),
            window_ms: ms(c.window),
        })
        .collect()
}

fn to_ipc_hid_rolled_back(service: &Service) -> Option<ipc::HidRolledBack> {
    service.hid_rolled_back().map(|r| ipc::HidRolledBack {
        address: r.address,
        name: r.name,
        le: r.le,
        reason: r.reason.as_str().to_string(),
    })
}

fn to_ipc_hid_health(service: &Service) -> Option<ipc::HidHealth> {
    service.hid_health().map(|h| {
        let (state, reason) = h.parts();
        ipc::HidHealth { state: state.to_string(), reason: reason.map(str::to_string) }
    })
}

/// A Msg error of crate::hid ("error.hid.off") with its code ("hid.off")
fn hid_err(m: &bluecaster_core::Msg) -> ErrorInfo {
    ErrorInfo { code: m.key.strip_prefix("error.").unwrap_or(m.key).to_string(), message: msg(m) }
}

/// What the UI tells about an adapter's firmware besides Realtek's files.
/// Per chip: a vendor's chips without a loader stay unsupported when others
/// have one (bt_host::chips::LOADER_CHIPS)
fn vendor_note(vendor: ChipVendor, vid: u16, pid: u16) -> Option<Msg> {
    if !chips::chip_supported(vendor, vid, pid) {
        return Some(Msg::new("firmware.vendor_unsupported"));
    }
    let key = match vendor {
        ChipVendor::Intel => "firmware.vendor_intel",
        ChipVendor::Broadcom => "firmware.vendor_broadcom",
        ChipVendor::Csr => "firmware.vendor_csr",
        ChipVendor::Barrot => "firmware.vendor_barrot",
        ChipVendor::Marvell => "firmware.vendor_marvell",
        ChipVendor::Actions => "firmware.vendor_actions",
        ChipVendor::MediaTek => "firmware.vendor_mediatek",
        ChipVendor::Qualcomm => "firmware.vendor_qualcomm",
        // Chips whose firmware comes from Realtek's Windows driver: the
        // RTL8852BD's note names it, the others' (RTL8922A, RTL8852BT) not
        ChipVendor::Realtek => match bt_host::realtek_btnic::chip(vid, pid) {
            Some(("RTL8852BD", _)) => "firmware.vendor_realtek_driver",
            Some(_) => "firmware.vendor_realtek_driver_other",
            None => return None,
        },
        ChipVendor::Unknown => return None,
    };
    Some(Msg::new(key))
}

/// Device Manager's "Windows cannot start this device until it restarts"
const CM_PROB_NEED_RESTART: u32 = 14;

/// LE Audio of the adapter BlueCaster uses (AdapterState::le_audio): the
/// running stack's, else what a stack found out on the first WinUSB adapter
fn le_audio_now(running: Option<bool>, adapters: &[AdapterInfo]) -> Option<bool> {
    running.or_else(|| adapters.iter().find(|a| a.driver == DriverKind::Winusb).and_then(|a| a.le_audio))
}

/// The adapter in use (AdapterState::active) from its entry in the list:
/// the one on WinUSB if two have this VID:PID. One not listed (yet) gets
/// what its VID:PID tells.
fn active_adapter(vid: u16, pid: u16, adapters: &[AdapterInfo]) -> ipc::ActiveAdapter {
    let same = |a: &&AdapterInfo| a.vid == vid && a.pid == pid;
    let listed =
        adapters.iter().filter(same).find(|a| a.driver == DriverKind::Winusb).or_else(|| adapters.iter().find(same));
    let realtek_driver_chip = || bt_host::realtek_btnic::chip(vid, pid).map(|(chip, _)| chip.to_owned());
    match listed {
        Some(a) => ipc::ActiveAdapter {
            display_name: a.display_name.clone(),
            windows_name: a.windows_name.clone(),
            vid,
            pid,
            vendor: a.vendor.clone(),
            chip: a
                .realtek_chip
                .clone()
                .or_else(|| (a.vendor == ChipVendor::Realtek.name()).then(realtek_driver_chip).flatten()),
            bt_version: a.bt_version.clone(),
        },
        None => {
            let vendor = chips::vendor_for_usb(vid, pid, None);
            ipc::ActiveAdapter {
                display_name: chips::display_name(vid, pid, None, vendor),
                windows_name: None,
                vid,
                pid,
                vendor: vendor.name().to_owned(),
                chip: (vendor == ChipVendor::Realtek).then(realtek_driver_chip).flatten(),
                bt_version: None,
            }
        }
    }
}

/// The Realtek chip (PID, firmware stem) whose files an adapter's card
/// reports on. The chip setting is one for all adapters, so an adapter of
/// another known vendor (MediaTek, Intel, ...) gets none, else it showed the
/// Realtek adapter's "Firmware OK" (issue #86)
fn adapter_chip(vendor: ChipVendor, realtek_pid: Option<u16>, settings: &CoreSettings) -> (u16, &str) {
    if !matches!(vendor, ChipVendor::Realtek | ChipVendor::Unknown) {
        return (0, "");
    }
    (chips::resolve_chip_pid(settings.bt_chip_pid, &settings.bt_chip_fw_stem, [realtek_pid]), &settings.bt_chip_fw_stem)
}

/// The devices paired in Windows' Bluetooth (its list is one for all its
/// radios): what BlueCaster adds may then be taken by both
fn windows_paired() -> Vec<String> {
    match win_sys::bt_paired::paired_devices() {
        Ok(list) => {
            list.iter().filter(|d| d.remembered || d.authenticated).map(|d| bt_host::addr_str(&d.address)).collect()
        }
        Err(e) => {
            tracing::debug!("Windows' paired devices: {e}");
            Vec::new()
        }
    }
}

/// A radio Windows' Bluetooth has switched on while BlueCaster has an adapter of
/// its own (WinUSB): Windows' list of paired devices is one for all radios
fn other_radio(adapters: &[AdapterInfo]) -> Option<OtherRadio> {
    if !adapters.iter().any(|a| a.driver == DriverKind::Winusb) {
        return None;
    }
    let radio = win_sys::bt_radios::radios().into_iter().find(|r| !r.name.is_empty())?;
    if win_sys::bt_radios::switched_on() == Some(false) {
        return None;
    }
    let paired =
        win_sys::bt_paired::paired_devices().is_ok_and(|list| list.iter().any(|d| d.remembered || d.authenticated));
    Some(OtherRadio { name: radio.name, paired })
}

/// The adapters, and whether one seems hung (AdapterState::hung)
fn adapter_infos(config_dir: &Path, settings: &CoreSettings) -> (Vec<AdapterInfo>, bool) {
    let radios = win_sys::bt_radios::radios();
    let (adapters, unreadable) = bluecaster_core::adapter::scan(false);
    let list: Vec<AdapterInfo> = adapters
        .into_iter()
        .map(|a| {
            // Windows' Bluetooth runs it: what Device Manager shows, kept
            // for when it is on WinUSB (issue #86). A radio it cannot be asked
            // on or that is not running (a vendor's driver, a MediaTek
            // composite device) may still have what Windows' stack stored on
            // its device node: also for an adapter BlueCaster does not support
            let radio = radios
                .iter()
                .find(|r| r.on_device(&a.device.instance_id))
                .cloned()
                .map(|mut r| {
                    if r.address.is_none()
                        && let Some(p) = win_sys::bt_radios::device_radio(&a.device.instance_id)
                    {
                        r.address = p.address;
                        r.hci_version = r.hci_version.or(p.hci_version);
                        r.lmp_version = r.lmp_version.or(p.lmp_version);
                    }
                    r
                })
                .or_else(|| win_sys::bt_radios::device_radio(&a.device.instance_id));
            if let Some(r) = &radio {
                bluecaster_core::adapter::save_windows_radio(
                    config_dir,
                    a.device.vid,
                    a.device.pid,
                    &r.name,
                    r.address.as_deref(),
                    r.hci_version,
                );
            }
            let driver = match a.device.driver_kind {
                win_sys::driver::DriverKind::WinUsb => DriverKind::Winusb,
                win_sys::driver::DriverKind::Bluetooth => DriverKind::Bluetooth,
                win_sys::driver::DriverKind::Other => DriverKind::Other,
                win_sys::driver::DriverKind::None => DriverKind::None,
            };
            let (chip_pid, fw_stem) = adapter_chip(a.vendor, a.realtek_pid, settings);
            let files = chips::firmware_files(chip_pid, fw_stem);
            let mut check = chips::firmware_check(&paths::firmware_dir(config_dir), chip_pid, fw_stem);
            let mut file_names = files.clone().map(|(f, c)| vec![f, c]).unwrap_or_default();
            // Realtek's Windows driver's firmware comes first, with no config
            // file (issue #86): no download is asked for then. Custom
            // firmware: the folder's files as before.
            let driver_fw = files
                .as_ref()
                .and_then(|(fw, _)| bluecaster_core::adapter::realtek_driver_firmware(fw))
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let from_driver = driver_fw.is_some() && !settings.realtek_custom_firmware;
            if let Some(name) = driver_fw.as_ref().filter(|_| from_driver) {
                check = Some(chips::FirmwareCheck { present: true, invalid_file: None });
                file_names = vec![name.clone()];
            }
            AdapterInfo {
                instance_id: a.device.instance_id.clone(),
                vid: a.device.vid,
                pid: a.device.pid,
                display_name: a.display_name.clone(),
                device_name: a.device.display_name().to_owned(),
                vendor: a.vendor.name().to_string(),
                driver,
                driver_service: a.device.driver.service.clone(),
                realtek_chip: a.realtek_pid.and_then(chips::realtek_chip_name).map(str::to_owned),
                firmware_present: check.as_ref().map(|c| c.present),
                firmware_download: files.as_ref().is_some_and(|(f, _)| bluecaster_core::firmware::available(f)),
                firmware_files: file_names,
                firmware_from_driver: from_driver,
                firmware_driver: driver_fw.is_some(),
                firmware_invalid: check.and_then(|c| c.invalid_file),
                vendor_note: vendor_note(a.vendor, a.device.vid, a.device.pid),
                restart_needed: a.device.problem_code == Some(CM_PROB_NEED_RESTART),
                le_audio: bluecaster_core::adapter::le_audio(config_dir, a.device.vid, a.device.pid),
                // What Windows' radio of this very device reads now, else
                // what it or a stack of BlueCaster read when one ran it (kept per
                // VID:PID), else a Realtek adapter's USB serial: an adapter
                // neither ran yet (Windows uses one radio only, the second
                // one tells nothing) still shows it (issue #86)
                address: radio
                    .as_ref()
                    .and_then(|r| r.address.clone())
                    .or_else(|| bluecaster_core::adapter::address(config_dir, a.device.vid, a.device.pid))
                    .or_else(|| bluecaster_core::adapter::serial_address(a.vendor, &a.device.instance_id)),
                // The running firmware's: on WinUSB BlueCaster's own read
                // (issue #427: the firmware it loaded, not Windows' driver's)
                bt_version: bluecaster_core::adapter::shown_hci_version(
                    driver == DriverKind::Winusb,
                    radio.as_ref().and_then(|r| r.hci_version),
                    bluecaster_core::adapter::hci_version(config_dir, a.device.vid, a.device.pid),
                    bluecaster_core::adapter::windows_hci_version(config_dir, a.device.vid, a.device.pid),
                )
                .and_then(bluecaster_core::adapter::bt_core_version)
                .map(str::to_owned),
                // An adapter already on WinUSB when BlueCaster came to know
                // radios: the device's own product string, as Device
                // Manager shows it (the WinUSB INF only names it "WinUSB
                // Device", in Windows' language)
                windows_name: bluecaster_core::adapter::windows_name(config_dir, a.device.vid, a.device.pid).or_else(
                    || {
                        a.device
                            .bus_reported_name
                            .as_deref()
                            .map(str::trim)
                            .filter(|n| !n.is_empty())
                            .map(str::to_owned)
                    },
                ),
            }
        })
        .collect();
    let hung = unreadable > 0
        && bluecaster_core::adapter::hung_adapter(
            unreadable,
            list.iter().any(|a| a.driver == DriverKind::Winusb),
            bluecaster_core::adapter::used_before(config_dir),
        );
    (list, hung)
}

/// What a new adapter list tells about the firmware against the last one
#[derive(Debug, Default, PartialEq, Eq)]
struct FirmwareChange {
    /// An adapter's firmware was missing or invalid and is there now
    fixed: bool,
    /// Adapters (display name) with a file newly found invalid
    invalid: Vec<(String, String)>,
}

fn firmware_change(old: &[AdapterInfo], new: &[AdapterInfo]) -> FirmwareChange {
    let mut change = FirmwareChange::default();
    for a in new {
        let before = old.iter().find(|o| o.instance_id == a.instance_id);
        if a.firmware_present == Some(true) && before.is_some_and(|o| o.firmware_present == Some(false)) {
            change.fixed = true;
        }
        if let Some(file) = &a.firmware_invalid
            && before.is_none_or(|o| o.firmware_invalid.as_ref() != Some(file))
        {
            change.invalid.push((a.display_name.clone(), file.clone()));
        }
    }
    change
}

/// The log line of v1 check_firmware_present for each file found invalid
fn warn_invalid_firmware(config_dir: &Path, invalid: &[(String, String)]) {
    for (adapter, file) in invalid {
        tracing::warn!(
            "{adapter}: {file} in {} is not a Realtek firmware file (a saved web page?)",
            config_dir.display()
        );
    }
}

/// The saved devices (classic, then LE Audio), then the scanned ones; with
/// the codecs each classic device offered. An entry is its address AND its
/// kind: a dual-mode device can be listed twice, classic and LE Audio.
fn saved_devices(
    config_dir: &Path,
    scanned: &[Device],
    codecs: &BTreeMap<String, bluecaster_core::KnownCodecs>,
) -> Vec<Device> {
    let saved = devices::load(config_dir).into_iter().chain(devices::load_le(config_dir));
    let mut list: Vec<Device> = saved
        .map(|d| Device {
            address: d.address,
            name: d.name,
            saved: true,
            audio: true,
            rssi: None,
            codecs: Vec::new(),
            sbc: None,
            codec_caps: Vec::new(),
            codecs_on_request: Vec::new(),
            le_audio: d.le_audio,
            le_audio_set: false,
            hid_le: None,
            set_addresses: Vec::new(),
        })
        .collect();
    // A Bluetooth LE keyboard or mouse is never an audio device (issue #413)
    for s in scanned.iter().filter(|s| s.hid_le.is_none()) {
        match list.iter_mut().find(|d| d.le_audio == s.le_audio && d.address.eq_ignore_ascii_case(&s.address)) {
            Some(d) => {
                d.rssi = s.rssi;
                d.le_audio_set |= s.le_audio_set;
                if d.name.is_empty() {
                    d.name = s.name.clone();
                }
            }
            None => list.push(s.clone()),
        }
    }
    for d in list.iter_mut().filter(|d| !d.le_audio) {
        if let Some(c) = codecs.get(&d.address.to_uppercase()) {
            d.codecs = c.keys.clone();
            d.sbc = c.sbc.clone().map(to_ipc_sbc_caps);
            d.codec_caps = c
                .caps
                .iter()
                .map(|k| ipc::CodecDeviceCaps {
                    codec: k.codec.clone(),
                    sample_rates: k.sample_rates.clone(),
                    bit_depths: k.bit_depths.clone(),
                    max_bitrate: k.max_bitrate,
                    lhdcv5_low_latency: k.lhdcv5_low_latency,
                })
                .collect();
        }
        // These list LDAC only after it was asked for (the stream does)
        if bluecaster_core::is_huawei(&d.name) {
            d.codecs_on_request = vec!["ldac".to_string()];
        }
    }
    let sets = le_sets::KnownSets::of(le_sets::load(config_dir));
    for d in list.iter_mut().filter(|d| d.le_audio) {
        let group = sets.group_of(&d.address).unwrap_or_default();
        d.set_addresses = group.iter().filter(|a| !a.eq_ignore_ascii_case(&d.address)).cloned().collect();
    }
    list
}

/// Saved LE Audio entries that are members of one coordinated set (two
/// earbuds saved as two devices) become one entry: the one with the user's
/// profiles or volume stays, else the one saved first, and the others'
/// profiles and volume move to it (bluecaster_core::le_sets). Returns how
/// many entries went, the profiles when they changed (not saved yet), and
/// whether `settings` (the saved volumes) changed.
fn merge_le_set_entries(
    dir: &Path,
    sets: &le_sets::KnownSets,
    settings: &mut CoreSettings,
) -> (usize, Option<Vec<CoreProfile>>, bool) {
    let saved_le = devices::load_le(dir);
    let mut list = profiles::load(dir);
    let merges = le_sets::merges(&saved_le, sets, |a| {
        list.iter().any(|p| p.le_audio && p.device_address.eq_ignore_ascii_case(a))
            || settings.saved_volumes.contains_key(&a.to_uppercase())
    });
    if merges.is_empty() {
        return (0, None, false);
    }
    for m in &merges {
        if let Err(e) = devices::remove_le(dir, &m.drop) {
            tracing::warn!("devices: merging a coordinated set's entries: {e}");
        }
        // Kept with the device from now on
        if let Some(group) = sets.group_of(&m.keep)
            && let Err(e) = le_sets::remember(dir, group)
        {
            tracing::warn!("devices: cannot keep the coordinated set: {e}");
        }
    }
    tracing::info!("devices: {} saved LE Audio entries merged into the entry of their coordinated set", merges.len());
    let profiles_moved = le_sets::move_profiles(&mut list, &merges, &saved_le);
    let volumes_moved = le_sets::move_volumes(&mut settings.saved_volumes, &merges);
    (merges.len(), profiles_moved.then_some(list), volumes_moved)
}

/// A scan's entry as the UI lists it (not saved yet)
fn scanned_device(d: &ScannedDevice) -> Device {
    Device {
        address: d.address.clone(),
        name: d.name.clone(),
        saved: false,
        audio: d.audio,
        rssi: Some(d.rssi),
        codecs: Vec::new(),
        sbc: None,
        codec_caps: Vec::new(),
        codecs_on_request: Vec::new(),
        le_audio: d.le_audio,
        le_audio_set: d.le_audio_set,
        hid_le: d.hid_le,
        set_addresses: Vec::new(),
    }
}

/// A scan's entries as the UI gets them: the device list the audio screens
/// use, and apart from it the Bluetooth LE keyboards and mice for the HID
/// host (issue #413), so that those never reach a stream, a profile or the
/// saved devices
fn split_scan(found: &[ScannedDevice]) -> (Vec<Device>, Vec<Device>) {
    let (hid, audio): (Vec<&ScannedDevice>, Vec<&ScannedDevice>) = found.iter().partition(|d| d.hid_le.is_some());
    (audio.into_iter().map(scanned_device).collect(), hid.into_iter().map(scanned_device).collect())
}

/// The codec the UI shows for an LE Audio (experimental) stream
const LE_CODEC: &str = "LC3";

/// The service's state message while a scan's LE Audio part runs
const LE_SCANNING_KEY: &str = "status.le_scanning";

/// StreamInfo.le of an LE Audio (experimental) stream
fn to_ipc_sbc_caps(c: bluecaster_core::SbcDeviceCaps) -> ipc::SbcDeviceCaps {
    ipc::SbcDeviceCaps {
        sample_rates: c.sample_rates,
        channel_modes: c.channel_modes,
        blocks: c.blocks,
        subbands: c.subbands,
        allocations: c.allocations,
        min_bitpool: c.min_bitpool,
        max_bitpool: c.max_bitpool,
    }
}

/// The codec details as the IPC sends them (the same fields)
fn to_ipc_codec_details(d: bluecaster_core::CodecDetails) -> ipc::CodecDetails {
    ipc::CodecDetails {
        mtu: d.mtu,
        rtp: d.rtp,
        media_header: d.media_header,
        frame_samples: d.frame_samples,
        frame_bytes: d.frame_bytes,
        frames_per_packet: d.frames_per_packet,
        sbc: d.sbc.map(|s| ipc::SbcDetails {
            channel_mode: s.channel_mode,
            blocks: s.blocks,
            subbands: s.subbands,
            allocation: s.allocation,
            bitpool: s.bitpool,
            config_bitpool: s.config_bitpool,
            device: s.device.map(to_ipc_sbc_caps),
            abr_bitpool: s.abr_bitpool,
        }),
        aac: d.aac.map(|a| ipc::AacDetails {
            object_type: a.object_type,
            bit_rate: a.bit_rate,
            device: a.device.map(|c| ipc::AacDeviceCaps {
                object_types: c.object_types,
                sample_rates: c.sample_rates,
                channels: c.channels,
                max_bitrate: c.max_bitrate,
                vbr: c.vbr,
            }),
        }),
        device: d.device.map(|c| ipc::CodecDeviceCaps {
            codec: c.codec,
            sample_rates: c.sample_rates,
            bit_depths: c.bit_depths,
            max_bitrate: c.max_bitrate,
            lhdcv5_low_latency: c.lhdcv5_low_latency,
        }),
    }
}

fn to_ipc_le_stream(d: &LeStreamDetails) -> LeStreamInfo {
    LeStreamInfo {
        preset: d.preset.clone(),
        frame_us: d.frame_us,
        octets: d.octets,
        cis: d.cis,
        devices: d.devices,
        low_latency: d.low_latency,
        summary: d.summary.clone(),
    }
}

/// The stream info with what an ABR encoder runs at now (the encode
/// thread's [`LinkStats::live_codec`]): the bit rate, and SBC's bitpool,
/// frame length and frames per packet. None when nothing changed, or the
/// stream is not an A2DP one under ABR (the values may be a stream's that
/// ended).
pub fn live_info(info: &ipc::StreamInfo, live: bt_host::LiveCodec) -> Option<ipc::StreamInfo> {
    if !info.abr || info.le.is_some() {
        return None;
    }
    let mut next = info.clone();
    next.bitrate_kbps = live.bitrate_kbps;
    if let Some(d) = next.details.as_deref_mut() {
        if let Some(s) = d.sbc.as_mut().filter(|s| s.abr_bitpool.is_some() && live.bitpool > 0) {
            s.bitpool = live.bitpool.into();
        }
        if live.frame_bytes > 0 && d.frame_bytes.is_some() {
            let bytes = u32::from(live.frame_bytes);
            d.frame_bytes = Some(bytes);
            // As stream::codec_details: whole frames in the payload, SBC
            // counts them in 4 bits
            let n = d.mtu.saturating_sub(d.media_header) / bytes;
            d.frames_per_packet = Some(if d.media_header == 1 { n.min(15) } else { n });
        }
    }
    (next != *info).then_some(next)
}

/// The cumulative media counters, those when the current stream started,
/// and the number of streams started
pub fn link_totals(s: &LinkStats) -> (LinkTotals, LinkTotals, u32) {
    let r = Ordering::Relaxed;
    // stream_starts is written after the start_* values
    let starts = s.stream_starts.load(Ordering::Acquire);
    let start = LinkTotals {
        packets: s.start_packets.load(r),
        bytes: s.start_bytes.load(r),
        dropped: s.start_dropped.load(r),
        capture_frames: s.start_capture_frames.load(r),
        capture_dropped: s.start_capture_dropped.load(r),
        capture_discontinuities: s.start_capture_discontinuities.load(r),
        capture_lost: s.start_capture_lost.load(r),
    };
    let total = LinkTotals {
        packets: s.packets_sent.load(r),
        bytes: s.bytes_sent.load(r),
        dropped: s.queue_drops.load(r) + s.send_errors.load(r),
        capture_frames: s.capture_frames.load(r),
        capture_dropped: s.capture_dropped_frames.load(r),
        capture_discontinuities: s.capture_discontinuities.load(r),
        capture_lost: s.capture_lost_frames.load(r),
    };
    (total, start, starts)
}

/// The remote device's chip of the A2DP link (None before its controller
/// answered)
pub fn remote_chip(d: &bt_host::LinkDetails) -> Option<ipc::RemoteChip> {
    if d.remote_version.is_none() && d.remote_features.is_none() {
        return None;
    }
    let v = d.remote_version;
    Some(ipc::RemoteChip {
        company_id: v.map(|v| v.company_id),
        company: v.map(|v| {
            chips::company_name(v.company_id).map_or_else(|| format!("0x{:04X}", v.company_id), str::to_owned)
        }),
        version: v.and_then(|v| bluecaster_core::adapter::bt_core_version(v.lmp_version)).map(str::to_owned),
        lmp_version: v.map(|v| v.lmp_version),
        lmp_subversion: v.map(|v| v.lmp_subversion),
        features: d.remote_features.map(|f| ipc::RemoteChipFeatures {
            edr_2m: f.edr_2m(),
            edr_3m: f.edr_3m(),
            three_slot: f.three_slot(),
            five_slot: f.five_slot(),
            edr_three_slot: f.edr_three_slot(),
            edr_five_slot: f.edr_five_slot(),
            largest_packet: bt_host::link_details::best_packet(&f).to_owned(),
        }),
    })
}

/// The A2DP link's state and readings (None without the link)
pub fn link_details(d: &bt_host::LinkDetails) -> Option<ipc::LinkDetails> {
    use bluecaster_core::link::slots_ms;
    if d.central.is_none() && d.mode.is_none() {
        return None;
    }
    Some(ipc::LinkDetails {
        role: d.central.map(|c| if c { ipc::LinkRole::Central } else { ipc::LinkRole::Peripheral }),
        role_changes: d.role_changes,
        mode: d.mode.map(|m| match m {
            bt_host::LinkMode::Active => ipc::LinkMode::Active,
            bt_host::LinkMode::Hold => ipc::LinkMode::Hold,
            bt_host::LinkMode::Sniff => ipc::LinkMode::Sniff,
            bt_host::LinkMode::Park => ipc::LinkMode::Park,
        }),
        sniff_interval_ms: d.sniff_interval_slots.map(slots_ms),
        sniff_entries: d.sniff_entries,
        encryption_key_size: d.encryption_key_size,
        supervision_timeout_ms: d.supervision_timeout_slots.map(slots_ms),
        failed_contacts: d.failed_contacts,
        failed_contacts_stream: d.failed_contacts.map(|_| d.failed_contacts_stream),
        failed_contacts_per_second: d.failed_contacts_per_second,
        link_quality: d.link_quality,
    })
}

/// The core's receiver types to the protocol's (the same fields)
fn convert<T: serde::Serialize, U: serde::de::DeserializeOwned>(v: &T) -> Option<U> {
    serde_json::to_value(v).ok().and_then(|j| serde_json::from_value(j).ok())
}

fn to_ipc_source(s: &bluecaster_core::SourceInfo) -> SourceInfo {
    SourceInfo { kind: s.kind.to_string(), name: s.name.clone(), waiting: s.waiting }
}

/// The parameters of stream.start_custom
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CustomStream {
    pub device: String,
    pub device_name: Option<String>,
    pub codec: String,
    pub quality: String,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub source: String,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub max_media_payload: Option<u16>,
    pub abr: Option<bool>,
    pub lhdcv5_low_latency: Option<bool>,
    pub codec_fallback: Option<bool>,
}

/// stream.start_broadcast's parameters
#[derive(Debug, Clone, Default)]
pub struct BroadcastStream {
    pub name: String,
    pub code: Option<String>,
    /// "hq" or "sq"
    pub quality: String,
    /// A matrix layout's rear: "hq" or "sq"; None: `quality`
    pub rear_quality: Option<String>,
    pub mono: Option<bool>,
    /// Left and right as two broadcasts of their own (layout "split"
    /// when no `layout` is given)
    pub split: Option<bool>,
    /// The channels and broadcasts (`BroadcastLayout::as_str`); None:
    /// stereo (or split)
    pub layout: Option<String>,
    pub source: String,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
}

/// What to broadcast for stream.start_broadcast: the source as for
/// stream.start_custom, no device, LE Audio with the broadcast
pub fn broadcast_request(b: &BroadcastStream, legacy_max_media_payload: u16) -> Result<StreamRequest, ErrorInfo> {
    const METHOD: &str = "stream.start_broadcast";
    let preset_of = |q: &str| match q.trim().to_ascii_lowercase().as_str() {
        "" | "hq" => Ok(bluecaster_core::BROADCAST_PRESET_HQ),
        "sq" => Ok(bluecaster_core::BROADCAST_PRESET_SQ),
        other => Err(bad_request("stream.bad_quality", METHOD, format!("unknown quality \"{other}\""))),
    };
    let preset = preset_of(&b.quality)?;
    let rear_preset = b.rear_quality.as_deref().map(preset_of).transpose()?;
    let layout = match b.layout.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        None if b.split == Some(true) => bt_host::BroadcastLayout::Split,
        None => bt_host::BroadcastLayout::Stereo,
        Some(l) => bt_host::BroadcastLayout::parse(l).ok_or_else(|| {
            bad_request(
                "stream.bad_layout",
                METHOD,
                format!("unknown layout \"{l}\" (one of {})", bt_host::BroadcastLayout::NAMES),
            )
        })?,
    };
    // One mono channel, or left and right as broadcasts of their own (or
    // any layout of more than one channel): not both
    let one_channel = matches!(layout, bt_host::BroadcastLayout::Stereo | bt_host::BroadcastLayout::Mono);
    if b.mono == Some(true) && !one_channel {
        return Err(bad_request("stream.bad_channels", METHOD, "mono and split cannot be used together".into()));
    }
    let c = CustomStream {
        device: "00:00:00:00:00:00".into(),
        device_name: Some(b.name.clone()),
        codec: "auto".into(),
        quality: "hq".into(),
        source: b.source.clone(),
        source_id: b.source_id.clone(),
        source_name: b.source_name.clone(),
        ..CustomStream::default()
    };
    let mut req = custom_request(&c, legacy_max_media_payload)?;
    req.le = Some(bt_host::LeRequest {
        preset: Some(preset.into()),
        broadcast: Some(bt_host::LeBroadcast {
            name: b.name.clone(),
            code: b.code.clone(),
            mono: b.mono.unwrap_or(false),
            layout,
            rear_preset: rear_preset.map(Into::into),
        }),
        ..bt_host::LeRequest::default()
    });
    Ok(req)
}

/// What to stream for stream.start_custom: built as a profile is (same
/// checks and defaults), with the test tone as a source of its own
pub fn custom_request(c: &CustomStream, legacy_max_media_payload: u16) -> Result<StreamRequest, ErrorInfo> {
    const METHOD: &str = "stream.start_custom";
    let codec = c.codec.trim().to_ascii_lowercase();
    let codec = if codec.is_empty() { "auto".to_string() } else { codec };
    if codec != "auto" && codec::CodecKind::from_key(&codec).is_none() {
        return Err(bad_request("stream.bad_codec", METHOD, format!("unknown codec \"{codec}\"")));
    }
    let source = c.source.trim().to_ascii_lowercase();
    let (mode, tone) = match source.as_str() {
        "tone" => ("loopback", true),
        "" | "loopback" => ("loopback", false),
        "virtual" => ("virtual", false),
        "app" => ("app", false),
        other => return Err(bad_request("stream.bad_source", METHOD, format!("unknown source \"{other}\""))),
    };
    let id = c.source_id.clone().unwrap_or_default();
    let name = c.source_name.clone().unwrap_or_default();
    let p = CoreProfile {
        name: String::new(),
        device_address: c.device.trim().to_uppercase(),
        device_name: c.device_name.clone().unwrap_or_default(),
        codec,
        quality: if c.quality.trim().is_empty() { "hq".into() } else { c.quality.trim().to_ascii_lowercase() },
        abr: c.abr.unwrap_or(false),
        lhdcv5_low_latency: c.lhdcv5_low_latency.unwrap_or(false),
        codec_fallback: c.codec_fallback.unwrap_or(false),
        sample_rate: c.sample_rate.unwrap_or(0),
        bit_depth: c.bit_depth.unwrap_or(0),
        capture_mode: mode.into(),
        audio_device_id: if mode == "virtual" { id.clone() } else { String::new() },
        audio_device_name: if mode == "virtual" { name.clone() } else { String::new() },
        // A test leaves Windows' default device alone
        auto_switch_device: false,
        app_exe: if mode == "app" { id } else { String::new() },
        app_name: if mode == "app" { name } else { String::new() },
        volume_sync: "off".into(),
        max_media_payload: c.max_media_payload.map(|v| bt_host::media::clamp_media_payload_limit(v as u32)),
        ..CoreProfile::default()
    };
    let mut req = p
        .to_request(legacy_max_media_payload)
        .map_err(|_| err("profile.invalid_address", "error.invalid_address_format"))?;
    // A profile always falls back; a custom stream only when asked
    req.codec_fallback = c.codec_fallback.unwrap_or(false);
    if tone {
        req.source = SourceRequest::TestTone;
    }
    Ok(req)
}

/// A profile's Windows Volume Sync: the setting for every device alone
/// (issue #86), on the captured device (Auto). A profile's own "off" from
/// before that setting, or a device it followed (once set in developer
/// mode), stays in the file but no longer counts: no screen shows or
/// resets it.
fn effective_volume_sync(global: bool, _profile: VolumeSync) -> VolumeSync {
    if global { VolumeSync::Auto } else { VolumeSync::Off }
}

/// HKCU Run value of v2: the resident core, which opens the UI unless
/// started with --background (start_minimized: only the tray at logon).
/// Written by the session host: the agent's user's HKCU when the agent
/// does the session's work.
///
/// Service mode: nothing is written. The service starts with Windows and
/// its agent shows the tray; the Run value keeps pointing at the user's
/// per-user core, which exits at once while the service is installed
/// (resident::EXIT_SERVICE_HOSTS) and works again once it is removed.
/// This program (the service folder's core) must never be written there.
fn apply_autostart(enabled: bool, minimized: bool) -> Result<(), String> {
    use win_sys::autostart::{VALUE_NAME, command_line};
    if bluecaster_core::paths::service_mode() {
        tracing::info!("start with Windows: left as it is in service mode");
        return Ok(());
    }
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        session_host::autostart::set(VALUE_NAME, &command_line(&exe, autostart_args(minimized)))
    } else {
        session_host::autostart::remove(VALUE_NAME).map(|_| ())
    }
}

/// What the start does with the Run value
#[derive(Debug, PartialEq, Eq)]
enum AutostartFix {
    Keep,
    /// Points it at this core (start_with_windows on)
    Write,
    /// Removes it (start_with_windows off)
    Remove,
}

use bluecaster_core::compat::OLD_CORE_EXE;

/// The Run value's program is named `name` (any folder, any case)
fn runs(command: Option<&str>, name: &str) -> bool {
    command
        .and_then(win_sys::autostart::program_of)
        .and_then(|p| p.file_name().map(|n| n.eq_ignore_ascii_case(name)))
        .unwrap_or(false)
}

/// What the start does with the Run values: ours ([`VALUE_NAME`]) and the
/// one of v1 and 2.0.x ([`OLD_VALUE_NAME`])
///
/// [`VALUE_NAME`]: win_sys::autostart::VALUE_NAME
/// [`OLD_VALUE_NAME`]: win_sys::autostart::OLD_VALUE_NAME
#[derive(Debug, PartialEq, Eq)]
struct AutostartPlan {
    ours: AutostartFix,
    /// Delete the old value (it named a core, or v1, and is taken over)
    remove_old: bool,
}

/// The Run values at start. `old`, under v1's and 2.0.x's value name, is
/// taken over when it names a core (2.0.x's a2dpwb-core.exe or a
/// bluecaster-core.exe, any folder) or v1 (A2DPWB.exe), and `replaces_v1`
/// (not a development instance): ours is written when on (unless it names
/// another copy of the core that exists), and the old one is deleted. An
/// old value naming any other program is left alone. Ours: see
/// [`autostart_fix`].
fn autostart_plan(
    ours: Option<&str>,
    old: Option<&str>,
    enabled: bool,
    replaces_v1: bool,
    exists: impl Fn(&std::path::Path) -> bool,
) -> AutostartPlan {
    let fix = autostart_fix(ours, enabled, replaces_v1, &exists);
    let takes_old = replaces_v1
        && [bluecaster_core::compat::V1_EXE, OLD_CORE_EXE, "bluecaster-core.exe"].iter().any(|n| runs(old, n));
    if !takes_old {
        return AutostartPlan { ours: fix, remove_old: false };
    }
    if !enabled {
        return AutostartPlan { ours: fix, remove_old: true };
    }
    // On: ours names this core afterwards, or another copy that exists
    let ours_is_core = runs(ours, "bluecaster-core.exe");
    match fix {
        AutostartFix::Keep if ours_is_core => AutostartPlan { ours: AutostartFix::Keep, remove_old: true },
        // Another program under our name: neither is ours to change
        AutostartFix::Keep if ours.is_some_and(|c| !c.trim().is_empty()) => {
            AutostartPlan { ours: AutostartFix::Keep, remove_old: false }
        }
        _ => AutostartPlan { ours: AutostartFix::Write, remove_old: true },
    }
}

/// Our Run value at start, from `current` and start_with_windows
/// (`enabled`). One naming v1 (its A2DPWB.exe; v2 registers only the core)
/// is taken over as by an update when `replaces_v1`: pointed at this core,
/// or removed when the setting is off, so that v1 does not start at logon.
/// Otherwise it is written only when on and missing, empty, or naming a
/// bluecaster-core.exe that does not `exist` any more. Any other program is
/// not ours to replace.
fn autostart_fix(
    current: Option<&str>,
    enabled: bool,
    replaces_v1: bool,
    exists: impl Fn(&std::path::Path) -> bool,
) -> AutostartFix {
    let program = current.and_then(win_sys::autostart::program_of);
    let named = |name: &str| program.as_ref().and_then(|p| p.file_name()).is_some_and(|n| n.eq_ignore_ascii_case(name));
    if replaces_v1 && named(bluecaster_core::compat::V1_EXE) {
        return if enabled { AutostartFix::Write } else { AutostartFix::Remove };
    }
    if !enabled {
        return AutostartFix::Keep;
    }
    match &program {
        None => AutostartFix::Write,
        Some(p) if named("bluecaster-core.exe") && !exists(p) => AutostartFix::Write,
        Some(_) => AutostartFix::Keep,
    }
}

fn autostart_args(minimized: bool) -> &'static str {
    if minimized { "--background" } else { "" }
}

/// The Run value needs writing for this settings change
fn autostart_changed(before: &CoreSettings, after: &CoreSettings) -> bool {
    before.start_with_windows != after.start_with_windows
        || (after.start_with_windows && before.start_minimized != after.start_minimized)
}

/// The chip or firmware setting, or the adapter to use, changed: the stack
/// must start anew
fn chip_changed(before: &CoreSettings, after: &CoreSettings) -> bool {
    before.bt_chip_pid != after.bt_chip_pid
        || before.bt_chip_fw_stem != after.bt_chip_fw_stem
        || before.realtek_custom_firmware != after.realtek_custom_firmware
        || !before.adapter_id.eq_ignore_ascii_case(&after.adapter_id)
}

/// The codec and what it runs with: "LDAC 990 kbps | 96 kHz | 24-bit | ABR"
/// (AAC's variable bit rate is VBR, as the UI calls it)
fn tray_codec_line(codec: &str, kbps: u32, sample_rate: u32, bits: u32, abr: bool) -> String {
    format!("{codec} {}", codec_params(codec, kbps, sample_rate, bits, abr))
}

/// The codec's values without its name: "990 kbps | 96 kHz | 24-bit | ABR"
fn codec_params(codec: &str, kbps: u32, sample_rate: u32, bits: u32, abr: bool) -> String {
    let mut line = format!("{kbps} kbps | {} kHz", sample_rate as f64 / 1000.0);
    if bits > 0 {
        line.push_str(&format!(" | {bits}-bit"));
    }
    if abr {
        line.push_str(if codec == "AAC" { " | VBR" } else { " | ABR" });
    }
    line
}

/// The balloon of a new connection (issue #86): "WH-1000XM4 connected",
/// then, as the connection info card shows them, the battery when it is
/// known already, the codec, its values on a line of their own
/// ([`codec_params`]), and the codec asked for when another one is used
/// (the UI's note beside the codec). None without the stream info: the
/// codec's values are the negotiated ones, known once it is set up.
fn connected_balloon(lang: &str, device: Option<&str>, stream: &StreamStatus) -> Option<BalloonRequest> {
    let i = stream.info.as_ref()?;
    let tr = |key: &str, default: &str| bluecaster_core::messages::translate(lang, key, default);
    let printf = bluecaster_core::messages::format_printf;
    let title = match device.filter(|d| !d.is_empty()) {
        Some(d) => printf(&tr("notify.connected", "%s connected"), &[d.to_string()]),
        None => tr("status.connected", "Connected"),
    };
    let mut lines = Vec::new();
    if let Some(b) = stream.battery {
        lines.push(printf(&tr("ui.home.battery", "Battery %d%%"), &[b.to_string()]));
    }
    lines.push(i.codec.clone());
    lines.push(codec_params(&i.codec, i.bitrate_kbps, i.sample_rate, i.bits, i.abr));
    if let Some(req) = &i.requested_codec {
        let (key, default) = if stream.message.key == "status.connected_fallback_developer" {
            ("ui.codec.fallback_developer", "(%s needs developer mode)")
        } else if i.requested_in_use {
            ("ui.codec.fallback_in_use", "(%s is in use)")
        } else {
            ("ui.codec.fallback_unsupported", "(%s is unsupported)")
        };
        lines.push(printf(&tr(key, default), std::slice::from_ref(req)));
    }
    Some(BalloonRequest { title, text: lines.join("\n"), url: None })
}

/// The tray icon's tooltip while streaming: "BlueCaster - Streaming (WH-1000XM4)"
/// and one line each for the codec and the battery
fn tray_tooltip(base: &str, device: Option<&str>, lines: &[String]) -> String {
    let mut t = match device.filter(|d| !d.is_empty()) {
        Some(d) => format!("{base} ({d})"),
        None => base.to_string(),
    };
    for l in lines {
        t.push('\n');
        t.push_str(l);
    }
    t
}

/// A codec's name as the UI shows it, by its key ("opus05": "Opus
/// (PipeWire)")
fn codec_label(lang: &str, key: &str) -> String {
    let label = match key {
        "ldac" => "codec.ldac",
        "aptx" => "codec.aptx",
        "aptxhd" => "codec.aptx_hd",
        "aptxll" => "codec.aptx_ll",
        "aptxad" => "codec.aptx_adaptive",
        "aac" => "codec.aac",
        "sbc" => "codec.sbc",
        "sbcxq" => "codec.sbc_xq",
        "faststream" => "codec.faststream",
        "lhdcv5" => "codec.lhdc_v5",
        "opus" => "codec.opus",
        "opus05" => "codec.opus05",
        _ => return key.to_string(),
    };
    bluecaster_core::messages::translate(lang, label, key)
}

/// The few words that say why the stream stopped over its codec: "LDAC is
/// unsupported" (the status line on Home, the flyout and the tray)
fn codec_stop_text(lang: &str, stop: &ipc::CodecStop) -> String {
    let (key, default) = match stop.kind {
        ipc::CodecStopKind::Unsupported => ("status.codec_stop_unsupported", "%s is unsupported"),
        ipc::CodecStopKind::SettingsUnsupported => {
            ("status.codec_stop_settings_unsupported", "%s settings are unsupported")
        }
        ipc::CodecStopKind::InUse => ("status.codec_stop_in_use", "%s is in use"),
        ipc::CodecStopKind::Refused => ("status.codec_stop_refused", "%s was refused"),
    };
    let template = bluecaster_core::messages::translate(lang, key, default);
    bluecaster_core::messages::format_printf(&template, &[codec_label(lang, &stop.codec)])
}

/// The headset microphone's line in the tray tooltip: "Voice: mSBC"
/// (StreamStatus.voice codec key)
fn voice_line(lang: &str, codec: &str) -> String {
    let tr = |key: &str, default: &str| bluecaster_core::messages::translate(lang, key, default);
    let name = match codec {
        "cvsd" => tr("codec.cvsd", "CVSD"),
        "msbc" => tr("codec.msbc", "mSBC"),
        "lc3_swb" => tr("codec.lc3_swb", "LC3"),
        "lc3" => "LC3".to_string(),
        other => other.to_string(),
    };
    bluecaster_core::messages::format_printf(&tr("status.voice_codec", "Voice: %s"), &[name])
}

/// A tray menu item: the device, its LE Audio entry tagged as in the
/// flyout ("WF-1000XM5 (LE Audio)")
/// The profiles the tray offers, one per device as the window lists it
/// (issue #86): with LE Audio on, a device with an LE Audio profile by that
/// one alone; off, no LE Audio profile
fn tray_profiles(profiles: &[Profile], le_on: bool) -> Vec<&Profile> {
    profiles
        .iter()
        .filter(|p| {
            if !le_on {
                return !p.le_audio;
            }
            p.le_audio
                || !profiles.iter().any(|o| o.le_audio && o.device_address.eq_ignore_ascii_case(&p.device_address))
        })
        .collect()
}

fn tray_label(device: &str, le_audio: bool, le_tag: &str) -> String {
    if le_audio { format!("{device} ({le_tag})") } else { device.to_string() }
}

/// A saved profile is the one streaming or connecting (under its name before
/// the save): it is started again with the new settings
/// The profile that connects after LE Audio was turned on or off while
/// `in_use` streamed (issue #86): the same device's LE Audio profile when it
/// is on, its classic one when it is off; `in_use` when the device has no
/// such profile (one that cannot stream says why)
fn le_switched_profile(list: &[CoreProfile], in_use: &CoreProfile, le_on: bool) -> String {
    if in_use.le_audio == le_on {
        return in_use.name.clone();
    }
    list.iter()
        .find(|p| p.le_audio == le_on && p.device_address.eq_ignore_ascii_case(&in_use.device_address))
        .map_or_else(|| in_use.name.clone(), |p| p.name.clone())
}

/// The LE Audio side's profile of a device made from its classic profile
/// `classic` (its capture and device settings), named "<device> LE Audio"
fn le_side_profile(list: &[CoreProfile], classic: &CoreProfile) -> CoreProfile {
    let device = if classic.device_name.is_empty() { &classic.device_address } else { &classic.device_name };
    let base = format!("{device} LE Audio");
    let mut name = base.clone();
    let mut n = 2;
    while list.iter().any(|p| p.name == name) {
        name = format!("{base} ({n})");
        n += 1;
    }
    CoreProfile { name, le_audio: true, ..classic.clone() }
}

fn restart_after_save(stream: &StreamStatus, saved: &str) -> bool {
    matches!(stream.state, StreamState::Connecting | StreamState::Streaming | StreamState::Reconnecting)
        && stream.profile.as_deref() == Some(saved)
}

/// The stream of profile `name` is connecting (or connecting again)
fn connecting(stream: &StreamStatus, name: &str) -> bool {
    matches!(stream.state, StreamState::Connecting | StreamState::Reconnecting)
        && stream.profile.as_deref() == Some(name)
}

/// The profile a tool's profile.set_* is for: `name`, or (None) the one
/// streaming, else the active one. Error profile.not_found.
fn tool_profile<'a>(s: &'a Snapshot, name: Option<&str>) -> Result<&'a Profile, ErrorInfo> {
    let name = match name {
        Some(n) => Some(n),
        None => {
            s.stream.profile.as_deref().filter(|n| restart_after_save(&s.stream, n)).or(s.active_profile.as_deref())
        }
    };
    name.and_then(|n| s.profiles.iter().find(|p| p.name == n))
        .ok_or_else(|| err("profile.not_found", "ipc.error.profile_not_found"))
}

/// profile.set_headset_mic: `p` with the headset mode `mode` as the window's
/// mode buttons set it (InputSource.tsx): Off mutes a profile whose mode is
/// Auto or On, the others set the mode and unmute. None: the mode shown is
/// `mode` already (Off while muted; On outside developer mode shows as Auto).
fn tool_headset_mic(p: &Profile, mode: ipc::HeadsetMicMode, developer: bool) -> Result<Option<Profile>, ErrorInfo> {
    let shown = match HeadsetMic::parse(&p.headset_mic) {
        _ if p.mic_muted => HeadsetMic::Off,
        HeadsetMic::On if !developer => HeadsetMic::Auto,
        m => m,
    };
    let wanted = HeadsetMic::parse(mode.as_str());
    if wanted == shown {
        return Ok(None);
    }
    if wanted == HeadsetMic::On && !developer {
        return Err(err("developer.off", "ipc.error.developer_only"));
    }
    if wanted == HeadsetMic::Off {
        return Ok(Some(Profile { mic_muted: true, ..p.clone() }));
    }
    if p.mic_device_id.is_empty() {
        return Err(err("profile.no_mic_output", "ipc.error.no_mic_output"));
    }
    Ok(Some(Profile { headset_mic: wanted.as_str().into(), mic_muted: false, ..p.clone() }))
}

/// profile.set_mic_muted: `p` muted or not (Home's microphone button); None:
/// it is already
fn tool_mic_muted(p: &Profile, muted: bool) -> Result<Option<Profile>, ErrorInfo> {
    if p.mic_muted == muted {
        Ok(None)
    } else if p.mic_device_id.is_empty() {
        Err(err("profile.no_mic_output", "ipc.error.no_mic_output"))
    } else {
        Ok(Some(Profile { mic_muted: muted, ..p.clone() }))
    }
}

/// The points the window's playback EQ starts with (lib/eq.ts
/// DEFAULT_POINTS): flat, so turning it on changes nothing yet
const FLAT_EQ_HZ: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

/// profile.set_media_eq: `p` with its playback EQ on or off, the points
/// kept; turned on without points, the window's flat ones. None: it is
/// already.
fn tool_media_eq(p: &Profile, enabled: bool) -> Option<Profile> {
    if p.media_eq.enabled == enabled {
        return None;
    }
    let mut eq = p.media_eq.clone();
    eq.enabled = enabled;
    if enabled && eq.bands.is_empty() {
        eq.bands = FLAT_EQ_HZ
            .iter()
            .map(|&frequency| ipc::EqPoint { frequency, gain: 0.0, q: 1.41, band_type: ipc::EqBandType::Peaking })
            .collect();
    }
    Some(Profile { media_eq: eq, ..p.clone() })
}

/// stream.volume_step: the volume `delta` away from the one the headphones
/// last reported, within 0..127. Errors stream.bad_volume_step (beyond
/// ±[`ipc::MAX_VOLUME_STEP`]), stream.volume_unknown (not streaming, or no
/// volume reported yet).
fn volume_step(stream: &StreamStatus, delta: i16) -> Result<u8, ErrorInfo> {
    if !(-ipc::MAX_VOLUME_STEP..=ipc::MAX_VOLUME_STEP).contains(&delta) {
        return Err(bad_request("stream.bad_volume_step", "stream.volume_step", format!("delta {delta}")));
    }
    let Some(volume) = stream.volume.filter(|_| stream.state == StreamState::Streaming) else {
        return Err(err("stream.volume_unknown", "ipc.error.volume_unknown"));
    };
    Ok((i16::from(volume) + delta).clamp(0, 127) as u8)
}

/// The last stream of profile `name` stopped with an error its codec caused
/// (refused, unsupported, in use; issue #86)
fn stopped_by_codec(stream: &StreamStatus, name: &str) -> bool {
    stream.state == StreamState::Error && stream.codec_stop.is_some() && stream.profile.as_deref() == Some(name)
}

/// A save of profile `saved` (its name before the save) whose last stream
/// its codec stopped: another codec, or other settings of it, connect the
/// device again (issue #86; the user did not disconnect it)
fn reconnect_after_codec_stop(
    stream: &StreamStatus,
    saved: &str,
    before: Option<&CoreProfile>,
    new: &CoreProfile,
) -> bool {
    let setup = |p: &CoreProfile| {
        (
            p.codec.clone(),
            p.codec_forced,
            p.unlisted_options,
            p.quality.clone(),
            p.abr,
            p.sample_rate,
            p.bit_depth,
            [p.sbc_bitpool, p.sbcxq_bitpool, p.sbc_min_bitpool, p.sbcxq_min_bitpool, p.sbc_blocks, p.sbc_subbands],
            (p.sbc_allocation.clone(), p.sbc_channel_mode.clone()),
        )
    };
    stopped_by_codec(stream, saved) && before.is_some_and(|b| setup(b) != setup(new))
}

/// The save changed only the source of an A2DP profile streaming: the stream
/// takes it in place ([`Service::change_source`]). What is still connecting
/// starts again, as a restart already on its way shows as connecting too.
fn source_changes_in_place(stream: &StreamStatus, before: Option<&CoreProfile>, saved: &CoreProfile) -> bool {
    stream.state == StreamState::Streaming && !saved.le_audio && before.is_some_and(|b| b.only_source_differs(saved))
}

/// The save named the codec an Auto profile streams with, as a codec card's
/// "..." dialog does on its first change, its name following the codec, the
/// rest as before but "Show only options it supports" (issue #86: the first
/// turn of that switch connected again, the later ones did not). The stream
/// is already that codec with these values: it goes on.
fn codec_named_in_place(stream: &StreamStatus, before: Option<&CoreProfile>, saved: &CoreProfile) -> bool {
    let Some(b) = before else { return false };
    let Some(info) = stream.info.as_ref() else { return false };
    if stream.state != StreamState::Streaming
        || saved.le_audio
        || b.codec != "auto"
        || info.requested_codec.is_some()
        || codec::CodecKind::from_key(&saved.codec).map(|k| k.name()) != Some(info.codec.as_str())
    {
        return false;
    }
    let as_before = CoreProfile {
        name: b.name.clone(),
        codec: b.codec.clone(),
        codec_forced: b.codec_forced,
        unlisted_options: b.unlisted_options,
        ..saved.clone()
    };
    as_before == *b
}

/// A new profile on Auto or On without a microphone output gets VB-CABLE's,
/// else the Hi-Fi Cable's (issue #86), unless its capture source is that
/// cable (the microphone would loop into the stream). An LE Audio one
/// stays closed (Off) with it: LE Audio takes Auto as On
/// ([`profiles::fill_mic_outputs`]). Windows' list can still be missing the
/// cables then: [`CoreState::watch_cables`] gives them later.
fn default_mic_output(p: &mut CoreProfile, stream: &StreamStatus, list: &[CoreProfile]) {
    if !p.mic_device_id.is_empty() || HeadsetMic::parse(&p.headset_mic) == HeadsetMic::Off {
        return;
    }
    let devices = session_host::devices::render_devices().unwrap_or_default();
    let default_output = default_output_for_cables(&devices, stream, list);
    if profiles::fill_mic_outputs(std::slice::from_mut(p), &cables(&devices), default_output) > 0 {
        tracing::info!("profile \"{}\": microphone output {} by default", p.name, p.mic_device_name);
    }
}

/// VB-CABLE and the Hi-Fi Cable among Windows' outputs
fn cables(devices: &[audio_capture::DeviceInfo]) -> profiles::Cables {
    let cable = |d: &audio_capture::DeviceInfo| profiles::Cable { id: d.id.clone(), name: d.name.clone() };
    profiles::Cables { vb: default_cable(devices).map(cable), hifi: hifi_cable(devices).map(cable) }
}

/// The real output the sound is captured from beside the one cable, which
/// is the microphone's (issue #86): a monitor's or TV's audio (nothing
/// plays aloud beside the headphones on most; of several, the one that is
/// Windows' default output), else Windows' default output, else the first,
/// virtual devices aside. As the UI's realOutput.
fn real_output(devices: &[audio_capture::DeviceInfo]) -> Option<&audio_capture::DeviceInfo> {
    let real = || devices.iter().filter(|d| !d.is_virtual);
    real()
        .find(|d| d.is_display && d.is_default)
        .or_else(|| real().find(|d| d.is_display))
        .or_else(|| real().find(|d| d.is_default))
        .or_else(|| real().next())
}

/// The sound card [`profiles::Bare`] plays the microphone into: an output
/// that is neither a display's nor virtual, Windows' default output before
/// the first
fn sound_card(devices: &[audio_capture::DeviceInfo]) -> Option<&audio_capture::DeviceInfo> {
    let cards = || devices.iter().filter(|d| !d.is_virtual && !d.is_display);
    cards().find(|d| d.is_default).or_else(|| cards().next())
}

/// Windows' default output as the cables' rules take it for System
/// Loopback: none while it is the Output Source the stream made the default
/// for its own device (issue #86: one device's Output Source took cables
/// from another device's lists; the default goes back when that stream
/// ends)
fn default_output_for_cables<'a>(
    devices: &'a [audio_capture::DeviceInfo],
    stream: &StreamStatus,
    list: &[CoreProfile],
) -> &'a str {
    let default = devices.iter().find(|d| d.is_default).map_or("", |d| d.id.as_str());
    let switched = matches!(stream.state, StreamState::Connecting | StreamState::Streaming | StreamState::Reconnecting)
        && stream.profile.as_ref().and_then(|n| list.iter().find(|p| &p.name == n)).is_some_and(|p| {
            matches!(p.capture_mode.as_str(), "virtual" | "output")
                && !default.is_empty()
                && p.audio_device_id == default
        });
    if switched { "" } else { default }
}

/// VB-CABLE's playback side ("CABLE Input"): the 2-channel one, never the
/// 16-channel one (as the UI's setupAudio; issue #86: Windows lists the
/// 16-channel one first for a moment, and the microphone stayed on it)
fn default_cable(devices: &[audio_capture::DeviceInfo]) -> Option<&audio_capture::DeviceInfo> {
    devices.iter().find(|d| profiles::is_vb_cable(&d.name, d.is_virtual) && !d.name.to_lowercase().contains("16ch"))
}

/// Windows' default input for the stream and the profiles: the
/// microphone output's recording side while a device whose profile asks for
/// it (headset mode Auto or On, muted or not, an output chosen) is
/// connected; kept while it connects (again), released once disconnected.
/// The profile's switch counts only in developer mode (`developer_mode`),
/// on for everyone else (issue #86).
fn default_input_want(stream: &StreamStatus, profiles: &[Profile], developer_mode: bool) -> InputWant {
    match stream.state {
        StreamState::Connecting | StreamState::Reconnecting => InputWant::Keep,
        StreamState::Idle | StreamState::Error => InputWant::Release,
        StreamState::Streaming => stream
            .profile
            .as_ref()
            .and_then(|name| profiles.iter().find(|p| &p.name == name))
            .filter(|p| {
                (p.mic_default_input || !developer_mode)
                    && !p.mic_device_id.is_empty()
                    && HeadsetMic::parse(&p.headset_mic).is_on()
            })
            .map_or(InputWant::Release, |p| InputWant::Switch(p.mic_device_id.clone())),
    }
}

/// Windows' endpoints; none in tests, which must not touch the PC's
/// default input
fn new_default_input() -> DefaultInput {
    if cfg!(test) { DefaultInput::new(Arc::new(NoInputs), std::time::Duration::ZERO) } else { DefaultInput::windows() }
}

/// No capture endpoints (tests)
struct NoInputs;

impl bluecaster_core::default_input::InputEndpoints for NoInputs {
    fn default_input(&self, _: bluecaster_core::default_input::Role) -> Option<String> {
        None
    }
    fn capture_side(&self, _: &str) -> Option<String> {
        None
    }
    fn set_default_input(&self, _: &str, _: bluecaster_core::default_input::Role) -> bool {
        false
    }
}

/// The headset mode in force for a profile: its mode with an output
/// chosen; Off otherwise (or while muted). On (the microphone kept open)
/// counts only in developer mode (`developer`, Settings::debug_mode; issue
/// #86): elsewhere it works as Auto, and the saved value stays, so turning
/// developer mode on brings it back. LE Audio takes Auto as On anyway.
fn headset_mic_in_force(p: &CoreProfile, developer: bool) -> HeadsetMic {
    match p.headset_mic_mode() {
        _ if p.mic_device_id.is_empty() => HeadsetMic::Off,
        HeadsetMic::On if !developer => HeadsetMic::Auto,
        mode => mode,
    }
}

/// Headset mode in force into a profile's request, for classic and LE Audio
/// devices alike (not a broadcast; LE Audio takes Auto as On)
fn set_headset_mic(req: &mut StreamRequest, mode: HeadsetMic, mic_device_id: &str) {
    req.headset_mic = mode;
    req.mic_output = mode.is_on().then(|| mic_device_id.to_string());
    if let Some(le) = req.le.as_mut().filter(|le| le.broadcast.is_none()) {
        le.headset_mic = mode;
        le.mic_output = req.mic_output.clone();
    }
}

/// The microphone's codec chosen into a profile's request, with Auto-select
/// (`auto_select`) off: the best call codec (classic), the microphone's rate
/// (LE Audio). Auto-select on: Auto for both, the choice stays saved.
fn set_mic_codec(req: &mut StreamRequest, p: &CoreProfile, auto_select: bool) {
    req.voice_codec = p.voice_codec_in_force(auto_select);
    if let Some(le) = req.le.as_mut().filter(|le| le.broadcast.is_none()) {
        le.mic_rate = p.mic_rate_in_force(auto_select);
    }
}

/// What a save that changed only the headset mode, the microphone output or
/// the microphone's codec of the profile in use does to its stream
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadsetChange {
    /// Nothing the stream uses: the microphone output changed while
    /// headset mode is off
    Unused,
    /// A classic stream switches its voice link on or off itself
    InPlace,
    /// An LE Audio stream sets up its streams again (or a classic one still
    /// connecting starts again)
    Restart,
}

/// Why the headset microphone did not open (the UI's MIC_NOTES): the stream
/// plays on with it as its message
fn is_mic_note(key: &str) -> bool {
    matches!(key, "error.headset_mic_loop" | "status.headset_mic_failed" | "status.headset_mic_output_failed")
}

/// Whether the microphone's codec chosen on a profile matters to its stream:
/// headset mode in force and a call codec (classic) or microphone rate (LE
/// Audio) chosen. Auto-select turned on or off then changes what it uses.
fn mic_choice_matters(p: &CoreProfile) -> bool {
    p.headset_mic_mode().is_on()
        && !p.mic_device_id.is_empty()
        && if p.le_audio { p.mic_rate != 0 } else { !p.voice_codec.is_empty() }
}

/// The save changed only `headset_mic` / `mic_device_*` / the microphone's
/// codec (None: something else changed as well, or nothing)
fn headset_change(stream: &StreamStatus, before: Option<&CoreProfile>, saved: &CoreProfile) -> Option<HeadsetChange> {
    let before = before.filter(|b| b.only_headset_mic_differs(saved))?;
    Some(if !before.headset_mic_mode().is_on() && !saved.headset_mic_mode().is_on() {
        HeadsetChange::Unused
    } else if stream.state == StreamState::Streaming && !saved.le_audio {
        HeadsetChange::InPlace
    } else {
        HeadsetChange::Restart
    })
}

/// The save changed only the bitpools of an A2DP SBC / SBC XQ profile
/// streaming: the encoder takes them in place ([`Service::change_bitpools`]),
/// as long as they stay within the range the link was set up with. Its lowest
/// is the profile's minimum at the time; a lower one needs a new setup.
fn bitpools_change_in_place(stream: &StreamStatus, before: Option<&CoreProfile>, saved: &CoreProfile) -> bool {
    if stream.state != StreamState::Streaming
        || saved.le_audio
        || !before.is_some_and(|b| b.only_bitpools_differ(saved))
    {
        return false;
    }
    let Some(info) = stream.info.as_ref() else { return false };
    let Some(sbc) = info.details.as_ref().and_then(|d| d.sbc.as_ref()) else { return false };
    let Some((lowest, _)) = sbc.config_bitpool else { return false };
    let (max, min) = if info.codec == codec::CodecKind::SbcXq.name() {
        (saved.sbcxq_bitpool, saved.sbcxq_min_bitpool)
    } else {
        (saved.sbc_bitpool, saved.sbc_min_bitpool)
    };
    // Auto's minimum is the device's
    let floor = if min != 0 { u32::from(min) } else { sbc.device.as_ref().map_or(2, |d| d.min_bitpool) };
    floor >= lowest && (max == 0 || u32::from(max) >= lowest)
}

/// Whether turning Auto-select (for every device) on or off may change the
/// codec of the profile in use: it chose one, or its own codec order (a
/// profile left on "auto" in the default order streams as Auto either
/// way). Not LE Audio (LC3).
fn restart_for_auto(stream: &StreamStatus, p: &CoreProfile) -> bool {
    restart_after_save(stream, &p.name) && !p.le_audio && p.codec != "auto"
}

/// A profile streams (or connects) with a codec of developer mode
/// ([`codec::CodecKind::developer_only`]): it starts again when developer
/// mode is turned off (issue #86)
fn streams_developer_codec(stream: &StreamStatus) -> bool {
    let developer = |name: &str| codec::CodecKind::ALL.into_iter().any(|k| k.developer_only() && k.name() == name);
    stream.profile.as_deref().is_some_and(|p| restart_after_save(stream, p))
        && stream.info.as_ref().is_some_and(|i| developer(&i.codec))
}

/// Auto-select just turned `auto_now`: whether the profile streaming would
/// stream the same anyway (issue #86: no reconnect then). The codec streaming
/// is the one the other side takes among those the device offered (`known`),
/// with the same settings: Auto's defaults, or each codec's own under
/// Auto-select in developer mode (`own_settings`). Unsure (no codec list, not
/// streaming yet): not the same.
fn same_after_auto_toggle(
    stream: &StreamStatus,
    p: &CoreProfile,
    auto_now: bool,
    known: Option<&bluecaster_core::stream::KnownCodecs>,
    own_settings: bool,
) -> bool {
    let (Some(info), Some(known)) = (stream.info.as_ref(), known) else {
        return false;
    };
    if stream.state != StreamState::Streaming {
        return false;
    }
    let offered = |k: &codec::CodecKind| known.keys.iter().any(|key| key == k.key());
    let pick = |order: codec::AutoOrder| order.codecs().iter().copied().find(offered);
    let Some(running) = codec::CodecKind::ALL.into_iter().find(|k| k.name() == info.codec) else {
        return false;
    };
    // The profile's own: its codec when offered, else Auto in its order
    let chosen = codec::CodecKind::from_key(&p.codec).filter(offered).or_else(|| pick(p.order()));
    let auto = pick(p.order());
    let next = if auto_now { auto } else { chosen };
    // The settings each side streams that codec with
    let auto_values = own_settings
        .then(|| p.all_codec_values().into_iter().find(|(k, _)| Some(*k) == next).map(|(_, v)| v))
        .flatten()
        .unwrap_or_default();
    next == Some(running) && auto_values == p.codec_values()
}

/// Whether a new codec order of the device (its cards dragged, issue #86
/// round 8) may change the codec of the profile in use (`p` as it streams):
/// it is Auto (`auto_select`: Auto-select is on) or fell back (the fallback
/// takes the first codec in the order). Not LE Audio.
fn restart_for_order(stream: &StreamStatus, p: &CoreProfile, auto_select: bool) -> bool {
    let fell_back = stream.info.as_ref().is_some_and(|i| i.requested_codec.is_some());
    restart_after_save(stream, &p.name) && !p.le_audio && (auto_select || p.codec == "auto" || fell_back)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl CoreState {
    pub fn new(
        service: Arc<Service>,
        tray: Box<dyn Fn(TrayView) + Send + Sync>,
        quit: Box<dyn Fn() + Send + Sync>,
        balloon: Box<dyn Fn(BalloonRequest) + Send + Sync>,
    ) -> Arc<CoreState> {
        let dir = service.config_dir().to_path_buf();
        let mut settings = service.settings();
        // Two earbuds of one set saved as two devices (a set learned in an
        // earlier session, or kept with their bonds): one entry
        let (_, merged_profiles, volumes) = merge_le_set_entries(&dir, &service.known_le_sets(), &mut settings);
        if let Some(list) = merged_profiles
            && let Err(e) = profiles::save(&dir, &list)
        {
            tracing::warn!("devices: saving the profiles of merged entries: {e}");
        }
        if volumes {
            let _ = settings.save(&dir);
            service.set_settings(settings.clone());
        }
        let legacy = settings.legacy_max_media_payload();
        let profiles: Vec<Profile> = profiles::load(&dir).iter().map(|p| to_ipc_profile(p, legacy)).collect();
        let active = Some(settings.last_profile.clone()).filter(|n| profiles.iter().any(|p| &p.name == n));
        let (adapters, hung) = adapter_infos(&dir, &settings);
        warn_invalid_firmware(&dir, &firmware_change(&[], &adapters).invalid);
        let service_mode = paths::service_mode();
        let snapshot = Snapshot {
            adapter: AdapterState {
                status: AdapterStatus::Stopped,
                le_audio: le_audio_now(service.le_audio_supported(), &adapters),
                other_radio: other_radio(&adapters),
                adapters,
                error: None,
                rom_firmware: false,
                hung,
                loader: None,
                active: None,
            },
            devices: saved_devices(&dir, &[], &service.device_codecs()),
            scanning: false,
            le_scanning: false,
            stream: StreamStatus { message: Msg::new("status.ready"), ..Default::default() },
            profiles,
            active_profile: active,
            settings: to_ipc_settings(&settings),
            update: UpdateState { current_version: env!("CARGO_PKG_VERSION").into(), ..Default::default() },
            // Service mode: a protected folder the user cannot open, so
            // none is named (files come through crate::handover)
            config_dir: if service_mode { String::new() } else { dir.display().to_string() },
            // Where main.rs has the log files written
            log_dir: if service_mode { String::new() } else { paths::log_dir(&dir).display().to_string() },
            service_mode,
            capture: None,
            // Set by logging::init, first thing in main
            clock_ms: crate::logging::hub().clock(),
            // Set by the resident core's watch (resident.rs)
            v1_running: false,
            verbose: crate::logging::verbose(),
            crashes: crate::crashes::unseen(&dir, &settings),
            audio_endpoints: 0,
            hid_devices: to_ipc_hid_devices(service.hid_devices()),
            hid_candidates: Vec::new(),
            hid_passkey: to_ipc_hid_passkey(service.hid_passkey()),
            hid_checks: to_ipc_hid_checks(&service),
            hid_rolled_back: to_ipc_hid_rolled_back(&service),
            hid_health: to_ipc_hid_health(&service),
            vendors: bluecaster_core::vendor::VENDORS
                .iter()
                .map(|v| ipc::VendorInfo {
                    id: v.id.into(),
                    name: v.name.into(),
                    options: v.options.iter().map(|o| o.to_string()).collect(),
                })
                .collect(),
        };
        let (events, _) = broadcast::channel(256);
        let state = Arc::new(CoreState {
            snapshot: Mutex::new((0, snapshot)),
            events,
            service,
            link_subscribers: AtomicUsize::new(0),
            receiver_subscribers: AtomicUsize::new(0),
            receiver: ReceiverClient::new(),
            started: Instant::now(),
            rates: Mutex::new(None),
            starting_profile: Mutex::new(None),
            mic_note_over: Mutex::new(None),
            battery_warned: AtomicBool::new(false),
            connect_announce: AtomicBool::new(false),
            battery_deadline: Mutex::new(None),
            quiet_connect: AtomicBool::new(false),
            restarting: AtomicBool::new(false),
            default_input: new_default_input(),
            cables_came: Mutex::new(None),
            tray,
            quit,
            balloon,
        });
        state.update_tray();
        state
    }

    pub fn config_dir(&self) -> PathBuf {
        self.service.config_dir().to_path_buf()
    }

    pub fn snapshot(&self) -> (u64, Snapshot) {
        let (rev, mut snap) = self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // The countdowns as of now, not as of the last event
        if !snap.hid_checks.is_empty() {
            snap.hid_checks = to_ipc_hid_checks(&self.service);
        }
        (rev, snap)
    }

    /// The revision of the state now
    pub fn rev(&self) -> u64 {
        self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    /// Reads part of the snapshot without copying all of it
    pub fn with_snapshot<R>(&self, f: impl FnOnce(&Snapshot) -> R) -> R {
        f(&self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).1)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(u64, Arc<Event>)> {
        self.events.subscribe()
    }

    /// Applies an event to the snapshot and sends it to the clients
    pub fn publish(&self, event: Event) {
        let mut input = None;
        let rev = {
            let mut g = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            let s = &mut g.1;
            if matches!(event, Event::Stream(_) | Event::Profiles { .. } | Event::Settings(_)) {
                let (stream, profiles, settings) = match &event {
                    Event::Stream(st) => (st, &s.profiles, &s.settings),
                    Event::Profiles { profiles, .. } => (&s.stream, profiles, &s.settings),
                    Event::Settings(x) => (&s.stream, &s.profiles, x),
                    _ => unreachable!(),
                };
                input = Some(default_input_want(stream, profiles, settings.developer_mode));
            }
            match &event {
                Event::Adapter(a) => s.adapter = a.clone(),
                Event::Devices(d) => s.devices = d.clone(),
                Event::Scanning(b) => s.scanning = *b,
                Event::LeScanning(b) => s.le_scanning = *b,
                Event::Stream(st) => s.stream = st.clone(),
                Event::Profiles { profiles, active_profile } => {
                    s.profiles = profiles.clone();
                    s.active_profile = active_profile.clone();
                }
                Event::Settings(x) => s.settings = x.clone(),
                Event::Update(u) => s.update = u.clone(),
                Event::Capture(c) => s.capture = c.clone(),
                Event::V1Running(b) => s.v1_running = *b,
                Event::Verbose(v) => s.verbose = *v,
                Event::Crashes(c) => s.crashes = c.clone(),
                Event::AudioEndpoints(n) => s.audio_endpoints = *n,
                Event::HidDevices(d) => s.hid_devices = d.clone(),
                Event::HidCandidates(d) => s.hid_candidates = d.clone(),
                Event::HidPasskey(p) => s.hid_passkey = p.clone(),
                Event::HidCheck { checks, rolled_back } => {
                    s.hid_checks = checks.clone();
                    s.hid_rolled_back = rolled_back.clone();
                }
                Event::HidHealth(h) => s.hid_health = h.clone(),
                // Not state: no revision
                Event::Link(_)
                | Event::Log(_)
                | Event::Receiver(_)
                | Event::Qcy(_)
                | Event::RfcommData(_)
                | Event::RfcommClosed(_)
                | Event::MediaButton(_)
                | Event::UiClose
                | Event::Bye => {
                    let _ = self.events.send((g.0, Arc::new(event)));
                    return;
                }
            }
            g.0 += 1;
            g.0
        };
        if let Some(want) = input {
            self.default_input.follow(want);
        }
        let _ = self.events.send((rev, Arc::new(event)));
        self.update_tray();
    }

    /// What an ABR encoder runs at now into the stream info (the sampler, a
    /// few times a second): published when it changed. Checked and applied
    /// under one lock, so that a state change meanwhile is never undone.
    pub fn apply_live_codec(&self, live: Option<bt_host::LiveCodec>) {
        let Some(live) = live else { return };
        {
            let mut g = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            let stream = &g.1.stream;
            let next = match (&stream.state, &stream.info) {
                (StreamState::Streaming, Some(i)) => live_info(i, live),
                _ => None,
            };
            let Some(next) = next else { return };
            tracing::trace!(
                "stream info: {} kbps, bitpool {:?}",
                next.bitrate_kbps,
                next.details.as_ref().and_then(|d| d.sbc.as_ref()).map(|s| s.bitpool)
            );
            g.1.stream.info = Some(next);
            g.0 += 1;
            let event = Event::Stream(g.1.stream.clone());
            let _ = self.events.send((g.0, Arc::new(event)));
        }
        self.update_tray();
    }

    /// The bit rate lowered for a link that is slow since the microphone
    /// was used (the sampler; None: not lowered): published when it changed
    pub fn apply_slow_link(&self, kbps: Option<u32>) {
        let mut g = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        let kbps = kbps.filter(|_| g.1.stream.state == StreamState::Streaming && g.1.stream.info.is_some());
        if g.1.stream.slow_link == kbps {
            return;
        }
        g.1.stream.slow_link = kbps;
        g.0 += 1;
        let event = Event::Stream(g.1.stream.clone());
        let _ = self.events.send((g.0, Arc::new(event)));
    }

    /// Windows' audio devices changed (resident.rs: watch_audio_endpoints):
    /// a cable that came is taken into use, and the UI reads its lists again
    pub fn audio_endpoints_changed(&self) {
        if let Ok(devices) = session_host::devices::render_devices() {
            self.watch_cables(&devices);
        }
        let n = self.with_snapshot(|s| s.audio_endpoints).wrapping_add(1);
        self.publish(Event::AudioEndpoints(n));
    }

    /// A2DPWB v1 started or exited: published when it changed
    pub fn set_v1_running(&self, on: bool) {
        if self.with_snapshot(|s| s.v1_running) == on {
            return;
        }
        if on {
            tracing::warn!("A2DPWB v1 is running: it may hold the Bluetooth adapter");
        } else {
            tracing::info!("A2DPWB v1 exited");
        }
        self.publish(Event::V1Running(on));
    }

    fn update_tray(&self) {
        let (_, s) = self.snapshot();
        let lang = s.settings.language.clone();
        let streaming = s.stream.state == StreamState::Streaming;
        let base = if streaming {
            bluecaster_core::messages::translate(&lang, "tray.tooltip_streaming", "BlueCaster - Connected")
        } else {
            bluecaster_core::messages::translate(&lang, "tray.tooltip_idle", "BlueCaster - Idle")
        };
        // The streamed profile's device (v1)
        let device = streamed_device(&s);
        let tooltip = if let Some(stop) = s.stream.codec_stop.as_ref().filter(|_| s.stream.state == StreamState::Error)
        {
            // What stopped it, in a few words (issue #86)
            tray_tooltip(&base, device, &[codec_stop_text(&lang, stop)])
        } else if streaming {
            let mut lines = Vec::new();
            if let Some(i) = &s.stream.info {
                lines.push(tray_codec_line(&i.codec, i.bitrate_kbps, i.sample_rate, i.bits, i.abr));
            }
            if let Some(v) = &s.stream.voice {
                lines.push(voice_line(&lang, &v.codec));
            }
            if let Some(b) = s.stream.battery {
                let template = bluecaster_core::messages::translate(&lang, "ui.home.battery", "Battery %d%%");
                lines.push(bluecaster_core::messages::format_printf(&template, &[b.to_string()]));
            }
            tray_tooltip(&base, device, &lines)
        } else {
            base
        };
        let le = bluecaster_core::messages::translate(&lang, "device.tag_le_audio", "LE Audio");
        let shown = tray_profiles(&s.profiles, s.settings.le_audio_experimental && s.settings.developer_mode);
        let labels = shown.iter().map(|p| tray_label(device_name(&s.devices, p), p.le_audio, &le)).collect();
        (self.tray)(TrayView {
            streaming,
            tooltip,
            profiles: shown.iter().map(|p| p.name.clone()).collect(),
            labels,
            active_profile: s.active_profile.clone(),
            language: lang,
        });
    }

    /// A microphone note out of date: the message it replaced comes back
    /// (plain "connected" without one). False when there was none.
    fn drop_mic_note(&self, stream: &mut StreamStatus) -> bool {
        if stream.state != StreamState::Streaming || !is_mic_note(&stream.message.key) {
            return false;
        }
        let over = self.mic_note_over.lock().unwrap_or_else(|e| e.into_inner()).take();
        stream.message = over.unwrap_or_else(|| Msg::new("status.connected"));
        true
    }

    /// A service event (from its thread)
    pub fn on_service_event(self: &Arc<Self>, e: ServiceEvent) {
        let (_, s) = self.snapshot();
        let mut stream = s.stream.clone();
        let mut devices_changed = false;
        match e {
            ServiceEvent::State(state, m) => {
                // The scan's LE Audio part runs while the state says so
                // (Service::scan_devices; the next message ends it)
                let le_scanning = m.key == LE_SCANNING_KEY;
                if le_scanning != s.le_scanning {
                    self.publish(Event::LeScanning(le_scanning));
                }
                stream.state = match state {
                    CoreStreamState::Idle => StreamState::Idle,
                    CoreStreamState::Connecting => StreamState::Connecting,
                    CoreStreamState::Streaming => StreamState::Streaming,
                    CoreStreamState::Reconnecting => StreamState::Reconnecting,
                    CoreStreamState::Error => StreamState::Error,
                };
                let message = msg(&m);
                {
                    let mut over = self.mic_note_over.lock().unwrap_or_else(|e| e.into_inner());
                    if !is_mic_note(&message.key) {
                        *over = None;
                    } else if !is_mic_note(&stream.message.key) {
                        *over = Some(stream.message.clone());
                    }
                }
                stream.message = message;
                // The codec that stopped it, for the status line (issue #86)
                stream.codec_stop = m.codec_stop.filter(|_| state == CoreStreamState::Error).map(|c| ipc::CodecStop {
                    codec: c.codec.key().into(),
                    kind: match c.kind {
                        CodecStopKind::Unsupported => ipc::CodecStopKind::Unsupported,
                        CodecStopKind::SettingsUnsupported => ipc::CodecStopKind::SettingsUnsupported,
                        CodecStopKind::InUse => ipc::CodecStopKind::InUse,
                        CodecStopKind::Refused => ipc::CodecStopKind::Refused,
                    },
                });
                // A new connection may warn about its battery again
                if matches!(state, CoreStreamState::Idle | CoreStreamState::Error | CoreStreamState::Connecting) {
                    self.battery_warned.store(false, Ordering::Release);
                }
                // A connection (also the headphones back on after they went
                // off) is announced; a lost link reconnecting is not, its
                // dropouts would repeat the same balloon; nor is the same
                // device started again with a new codec (issue #86)
                match state {
                    CoreStreamState::Connecting => {
                        if !self.quiet_connect.load(Ordering::Acquire) {
                            self.connect_announce.store(true, Ordering::Release);
                            *self.battery_deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        }
                    }
                    CoreStreamState::Streaming => self.quiet_connect.store(false, Ordering::Release),
                    CoreStreamState::Idle | CoreStreamState::Error => {
                        self.connect_announce.store(false, Ordering::Release)
                    }
                    _ => {}
                }
                // A lost link's battery is stale until the device reports
                // again (issue #86: a battery shown while disconnected)
                // The voice link goes with it (W3 reports it again)
                if state == CoreStreamState::Reconnecting {
                    stream.battery = None;
                    stream.battery_refused = false;
                    stream.voice = None;
                    stream.slow_link = None;
                    stream.voice_codecs.clear();
                    stream.mic_rates.clear();
                }
                if matches!(state, CoreStreamState::Idle | CoreStreamState::Error) {
                    stream.info = None;
                    stream.volume = None;
                    stream.battery = None;
                    stream.battery_refused = false;
                    stream.voice = None;
                    stream.slow_link = None;
                    stream.voice_codecs.clear();
                    stream.mic_rates.clear();
                    if state == CoreStreamState::Idle {
                        stream.profile = None;
                    }
                }
                // The new session starts (after the old one's Idle when
                // switching profiles): its profile is the one asked for
                if state == CoreStreamState::Connecting {
                    stream.profile = self.starting_profile.lock().unwrap_or_else(|e| e.into_inner()).clone();
                }
                // The stack runs once anything connected
                let adapter_status = match state {
                    CoreStreamState::Error if m.key.starts_with("error.btstack") => Some(AdapterStatus::Error),
                    CoreStreamState::Streaming => Some(AdapterStatus::Ready),
                    _ => None,
                };
                if let Some(st) = adapter_status {
                    let mut a = s.adapter.clone();
                    if a.status != st {
                        a.status = st;
                        a.error = (st == AdapterStatus::Error).then(|| msg(&m));
                        self.publish(Event::Adapter(a));
                        // The adapter may be gone or hung (issue #86): the
                        // list anew, so the UI tells which
                        if st == AdapterStatus::Error {
                            self.list_adapters();
                        }
                    }
                }
                // A connection learns the device's codecs, also one that
                // fails afterwards
                devices_changed = state == CoreStreamState::Error;
            }
            ServiceEvent::StreamInfo(i) => {
                // LE Audio (experimental): what the stream was set up with
                let le = self.service.le_stream().map(|d| to_ipc_le_stream(&d));
                stream.info = Some(ipc::StreamInfo {
                    codec: if le.is_some() { LE_CODEC.into() } else { i.codec },
                    bitrate_kbps: i.bitrate_kbps,
                    sample_rate: i.sample_rate,
                    channels: i.channels,
                    bits: i.bits,
                    source_sample_rate: i.source_sample_rate,
                    source_channels: i.source_channels,
                    source_bits: i.source_bits,
                    requested_codec: i.requested_codec,
                    requested_in_use: i.requested_in_use,
                    abr: i.abr,
                    bitrate_capped: i.bitrate_capped,
                    hi_res_set: i.hi_res_set,
                    low_latency: i.low_latency,
                    source: Some(to_ipc_source(&i.source)),
                    le,
                    details: i.details.map(|d| Box::new(to_ipc_codec_details(*d))),
                });
                // The device was saved when it connected
                devices_changed = true;
            }
            ServiceEvent::Volume(v) => stream.volume = Some(v),
            // Headset mode: the voice link carries audio, or no longer
            ServiceEvent::Voice(v) => {
                // The microphone opened: why it did not before is out of date
                if v.is_some() {
                    self.drop_mic_note(&mut stream);
                }
                stream.voice = v;
            }
            // The Adapters screen shows the loader while the adapter starts
            ServiceEvent::Loader(p) => {
                self.publish(Event::Adapter(AdapterState {
                    loader: Some(LoaderStatus {
                        vendor: p.vendor.to_owned(),
                        step: p.step,
                        percent: p.percent,
                        ok: p.done,
                    }),
                    ..s.adapter
                }));
                return;
            }
            ServiceEvent::LeSetLearned => {
                self.merge_le_sets();
                return;
            }
            // The adapter a stack opened, or none any more
            ServiceEvent::Adapter(ids) => {
                let active = ids.map(|(vid, pid)| active_adapter(vid, pid, &s.adapter.adapters));
                if active != s.adapter.active {
                    self.publish(Event::Adapter(AdapterState { active, ..s.adapter }));
                }
                return;
            }
            ServiceEvent::Button(b) => {
                self.publish(Event::MediaButton(b));
                return;
            }
            ServiceEvent::HidDevices => {
                let list = to_ipc_hid_devices(self.service.hid_devices());
                if list != s.hid_devices {
                    self.publish(Event::HidDevices(list));
                }
                return;
            }
            ServiceEvent::HidPasskey => {
                let p = to_ipc_hid_passkey(self.service.hid_passkey());
                if p != s.hid_passkey {
                    self.publish(Event::HidPasskey(p));
                }
                return;
            }
            // Always sent: the time left changes
            ServiceEvent::HidCheck => {
                let checks = to_ipc_hid_checks(&self.service);
                let rolled_back = to_ipc_hid_rolled_back(&self.service);
                self.publish(Event::HidCheck { checks, rolled_back });
                return;
            }
            ServiceEvent::HidHealth => {
                let h = to_ipc_hid_health(&self.service);
                if h != s.hid_health {
                    self.publish(Event::HidHealth(h));
                }
                return;
            }
            // Another saved device connected while the one streamed was off
            // (issue #86): its own profile starts, as a connect on Home
            ServiceEvent::OtherDeviceBack(addr) => {
                let address = bt_host::addr_str(&addr);
                let profile = profiles::load(&self.config_dir())
                    .into_iter()
                    .find(|p| !p.le_audio && p.device_address.eq_ignore_ascii_case(&address));
                match profile {
                    Some(p) => {
                        tracing::info!("{address} connected while the device streamed was off: starting {}", p.name);
                        if let Err(e) = self.start_profile(&p.name) {
                            tracing::warn!("{address}: {}", e.message.key);
                        }
                    }
                    None => tracing::debug!("{address} connected, but has no profile"),
                }
                return;
            }
            // A report that arrives after the device went off (HFP still
            // closing) is not shown on a device no longer connected
            ServiceEvent::Battery(_)
            | ServiceEvent::BatteryRefused
            | ServiceEvent::VoiceCodecs(_)
            | ServiceEvent::MicRates(_)
                if matches!(stream.state, StreamState::Idle | StreamState::Error) =>
            {
                tracing::debug!("battery report while not connected: ignored");
            }
            ServiceEvent::VoiceCodecs(codecs) => stream.voice_codecs = codecs,
            ServiceEvent::MicRates(rates) => stream.mic_rates = rates,
            ServiceEvent::BatteryRefused => {
                stream.battery = None;
                stream.battery_refused = true;
            }
            ServiceEvent::Battery(v) => {
                stream.battery = Some(v);
                stream.battery_refused = false;
                let mut warned = self.battery_warned.load(Ordering::Acquire);
                let low = self.service.settings().battery_low_percent;
                let alert = battery_low_alert(&mut warned, v, low);
                self.battery_warned.store(warned, Ordering::Release);
                if alert {
                    self.battery_low(&s, v, low);
                }
            }
        }
        self.publish(Event::Stream(stream));
        self.announce_connected();
        if devices_changed {
            self.refresh_devices();
        }
        // A stack started and found out the adapter's LE Audio (remembered
        // in adapter_caps.json): the adapters anew
        if le_audio_now(self.service.le_audio_supported(), &s.adapter.adapters) != s.adapter.le_audio {
            self.list_adapters();
        }
        // The stack found its Realtek adapter on the ROM code, or a new
        // stack loaded the firmware
        let rom = self.service.host().is_some_and(|h| h.realtek_rom_firmware());
        let (_, s) = self.snapshot();
        if rom != s.adapter.rom_firmware {
            self.publish(Event::Adapter(AdapterState { rom_firmware: rom, ..s.adapter }));
        }
    }

    /// The device list again (saved file, last scan, codecs); published if
    /// it changed
    pub(crate) fn refresh_devices(&self) {
        let (_, s) = self.snapshot();
        let scanned: Vec<Device> = s.devices.iter().filter(|d| !d.saved).cloned().collect();
        let list = saved_devices(&self.config_dir(), &scanned, &self.service.device_codecs());
        if list != s.devices {
            self.publish(Event::Devices(list));
        }
    }

    /// Service mode, after the one-time move of the user's data (when the
    /// first agent connected): settings, profiles and devices read again
    /// from the folder and published, as if the core had started with them
    pub(crate) fn reload_from_disk(&self) {
        let settings = self.service.reload_saved();
        crate::logging::set_dev(&settings.log());
        self.publish(Event::Settings(to_ipc_settings(&settings)));
        let legacy = settings.legacy_max_media_payload();
        let profiles: Vec<Profile> =
            profiles::load(&self.config_dir()).iter().map(|p| to_ipc_profile(p, legacy)).collect();
        let active = Some(settings.last_profile.clone()).filter(|n| profiles.iter().any(|p| &p.name == n));
        self.publish(Event::Profiles { profiles, active_profile: active });
        self.refresh_devices();
        self.update_tray();
    }

    /// A connection learned a coordinated set: saved entries of it become
    /// one ([`merge_le_set_entries`]), published
    fn merge_le_sets(&self) {
        let dir = self.config_dir();
        let _tx = self.service.settings_tx();
        let mut settings = self.service.settings();
        let (_, list, volumes) = merge_le_set_entries(&dir, &self.service.known_le_sets(), &mut settings);
        if let Some(list) = list {
            let (_, s) = self.snapshot();
            let active = s.active_profile.filter(|a| list.iter().any(|p| &p.name == a));
            if self.save_profiles(&list, active).is_err() {
                tracing::warn!("devices: saving the profiles of merged entries failed");
            }
        }
        if volumes {
            let _ = settings.save(&dir);
            self.service.set_settings(settings);
        }
        // The addresses of the set too
        self.refresh_devices();
    }

    fn save_profiles(&self, list: &[CoreProfile], active: Option<String>) -> Result<(), ErrorInfo> {
        let dir = self.config_dir();
        profiles::save(&dir, list).map_err(|_| err("profile.save_failed", "error.profile_save"))?;
        let legacy = self.service.settings().legacy_max_media_payload();
        self.publish(Event::Profiles {
            profiles: list.iter().map(|p| to_ipc_profile(p, legacy)).collect(),
            active_profile: active,
        });
        Ok(())
    }

    /// Forgets a device: its stream ends, and its entry, profile, volume,
    /// what it offered and its link key go, so nothing can connect it again
    /// (issue #86: the tray still could, with the profile left behind)
    fn forget_device(&self, address: &str, le_audio: bool) -> Result<Value, ErrorInfo> {
        let dir = self.config_dir();
        let mut list = profiles::load(&dir);
        // An LE Audio device of a coordinated set: every member goes (both
        // earbuds of a pair are one device)
        let mut members = vec![address.to_uppercase()];
        if le_audio && let Some(group) = self.service.known_le_sets().group_of(address) {
            members.extend(group.iter().filter(|m| !m.eq_ignore_ascii_case(address)).cloned());
        }
        let is_member = |a: &str| members.iter().any(|m| m.eq_ignore_ascii_case(a));
        let of_it = |p: &CoreProfile| p.le_audio == le_audio && is_member(&p.device_address);
        let (_, s) = self.snapshot();
        let streaming = s.stream.profile.as_ref().is_some_and(|n| list.iter().any(|p| p.name == *n && of_it(p)));
        if streaming && s.stream.state != StreamState::Idle {
            // Waited for: a session ending later would save the device again
            self.service.disconnect_for(self.service.ticket());
        }
        let r = if le_audio { devices::remove_le(&dir, address) } else { devices::remove(&dir, address) };
        r.map_err(|_| err("device.forget_failed", "error.device_forget"))?;
        for m in members.iter().skip(1) {
            if let Err(e) = devices::remove_le(&dir, m) {
                tracing::warn!("devices: removing another member of the set: {e}");
            }
        }
        if members.len() > 1 {
            le_sets::forget(&dir, address);
            tracing::info!("device forgotten with the other {} member(s) of its coordinated set", members.len() - 1);
        }
        let gone: Vec<String> = list.iter().filter(|p| of_it(p)).map(|p| p.name.clone()).collect();
        list.retain(|p| !of_it(p));
        let active = s.active_profile.filter(|a| !gone.contains(a));
        self.save_profiles(&list, active)?;
        // Off every list now (Home, Codecs, the sidebar): what
        // follows may wait for the stack (up to seconds per entry)
        self.refresh_devices();
        for m in &members {
            self.service.forget_device(m, le_audio);
            crate::pairing::forget_services(&dir, m, le_audio);
        }
        let _tx = self.service.settings_tx();
        let mut settings = self.service.settings();
        let mut volume = false;
        for m in &members {
            volume |= settings.saved_volumes.remove(m).is_some();
        }
        let last = gone.contains(&settings.last_profile);
        if last {
            settings.last_profile.clear();
        }
        if volume || last {
            let _ = settings.save(&dir);
            self.service.set_settings(settings);
        }
        // Published again only if a scan changed the list meanwhile
        self.refresh_devices();
        Ok(Value::Null)
    }

    /// The virtual cables taken into use (issue #86): once one came after it
    /// was missing, the profiles set up by the rules alone are set up again
    /// with what Windows has now, as the first-run setup would: both cables,
    /// the Hi-Fi Cable for the sound (less delay) and VB-CABLE for the
    /// microphone; one, the microphone, the sound on a real output
    /// ([`real_output`], [`profiles::reassign_cables`]; what the user chose
    /// stays), and again at each look for [`CABLES_SETTLE`] after (Windows
    /// lists a cable's outputs one after the other). Windows' default
    /// output decides nothing here. At every look, a microphone on without
    /// an output gets one when a cable it can use is there
    /// ([`profiles::fill_mic_outputs`]), System Loopback of the cable
    /// the microphone plays into moves to the sound the rules give
    /// ([`profiles::unloop_sounds`]), and a source or microphone on a cable
    /// that is gone is chosen again ([`profiles::drop_cables`]); the names
    /// saved follow Windows' ([`profiles::refresh_names`]).
    pub fn watch_cables(&self, devices: &[audio_capture::DeviceInfo]) {
        // One look at a time: two would both save the profiles
        static WATCHING: Mutex<()> = Mutex::new(());
        let _one = WATCHING.lock().unwrap_or_else(|e| e.into_inner());
        let s = self.service.settings();
        let now = cables(devices);
        // Missing at the last look; both there before the first one
        let was = profiles::CableSet { vb: !s.awaiting_vb_cable, hifi: !s.awaiting_hifi_cable };
        let (vb_missing, hifi_missing) = (now.vb.is_none(), now.hifi.is_none());
        let came = (!vb_missing && !was.vb) || (!hifi_missing && !was.hifi);
        let flags_changed = (vb_missing, hifi_missing) != (s.awaiting_vb_cable, s.awaiting_hifi_cable);
        let mut list = profiles::load(&self.config_dir());
        let (_, snap) = self.snapshot();
        // The profile streaming now, as it was
        let streamed = match snap.stream.state {
            StreamState::Streaming => snap.stream.profile.as_ref().and_then(|n| list.iter().find(|p| &p.name == n)),
            _ => None,
        }
        .cloned();
        let default_output = default_output_for_cables(devices, &snap.stream, &list);
        // What was there before the cable came, while it settles
        let since = {
            let mut settling = self.cables_came.lock().unwrap_or_else(|e| e.into_inner());
            if came {
                *settling = Some((Instant::now(), was));
            }
            *settling = settling.filter(|(at, _)| at.elapsed() < CABLES_SETTLE);
            settling.map(|(_, was)| was)
        };
        let real = real_output(devices).map(|d| profiles::Cable { id: d.id.clone(), name: d.name.clone() });
        // Not while Windows lists no output at all: nothing is known then
        let as_cable = |d: &audio_capture::DeviceInfo| profiles::Cable { id: d.id.clone(), name: d.name.clone() };
        let outputs: Vec<profiles::Cable> = devices.iter().map(as_cable).collect();
        let displays: Vec<profiles::Cable> =
            devices.iter().filter(|d| d.is_display && !d.is_virtual).map(as_cable).collect();
        let pair = real_output(devices).filter(|d| d.is_display).zip(sound_card(devices));
        let pair = pair.map(|(sound, mic)| (as_cable(sound), as_cable(mic)));
        let bare = pair.as_ref().map(|(sound, mic)| profiles::Bare { sound, mic });
        let dropped = match devices.is_empty() {
            true => 0,
            false => profiles::drop_cables(&mut list, &now, default_output, real.as_ref(), bare, &outputs, &displays),
        };
        let set_up = dropped
            + match since {
                Some(was) => {
                    profiles::reassign_cables(&mut list, was, &now, default_output, real.as_ref(), bare, &displays)
                }
                None => 0,
            }
            + profiles::unloop_sounds(&mut list, &now, default_output, real.as_ref());
        let filled = profiles::fill_mic_outputs(&mut list, &now, default_output);
        let renamed = profiles::refresh_names(&mut list, &outputs);
        if came || set_up + filled > 0 {
            tracing::info!(
                "cables: VB-CABLE {}, Hi-Fi Cable {}; {set_up} profile(s) set up again, {filled} microphone(s) given one",
                if vb_missing { "missing" } else { "there" },
                if hifi_missing { "missing" } else { "there" },
            );
        }
        if set_up + filled + renamed > 0 {
            let active = Some(s.last_profile.clone()).filter(|n| list.iter().any(|p| &p.name == n));
            if let Err(e) = self.save_profiles(&list, active) {
                tracing::warn!("cables: profiles not saved: {}", e.code);
            }
            // The stream follows its profile (issue #86: it went on
            // capturing the cable its microphone had just been given): the
            // source first, the session refuses a microphone that loops
            if let Some(was) = streamed
                && let Some(p) = list.iter().find(|p| p.name == was.name)
            {
                let settings = self.service.settings();
                if p.source() != was.source() {
                    let moved = self.service.change_source(settings.source_in_force(p.source()));
                    tracing::info!("cables: the stream's source follows its profile: {moved}");
                }
                if (p.mic_device_id.as_str(), p.headset_mic.as_str())
                    != (was.mic_device_id.as_str(), was.headset_mic.as_str())
                {
                    let mode = headset_mic_in_force(p, settings.debug_mode);
                    let kept = self.service.set_headset_mic(
                        mode,
                        mode.is_on().then(|| p.mic_device_id.clone()),
                        p.voice_codec_in_force(settings.auto_codec),
                    );
                    tracing::info!("cables: the stream's microphone follows its profile: {kept}");
                }
            }
        }
        if flags_changed {
            // As they are now: the settings may have been saved meanwhile
            let _tx = self.service.settings_tx();
            let mut s = self.service.settings();
            s.awaiting_vb_cable = vb_missing;
            s.awaiting_hifi_cable = hifi_missing;
            let _ = s.save(&self.config_dir());
            self.service.set_settings(s);
        }
    }

    fn save_settings(&self, s: CoreSettings) -> Result<(), ErrorInfo> {
        s.save(&self.config_dir()).map_err(|_| err("settings.save_failed", "error.settings_save"))?;
        crate::logging::set_dev(&s.log());
        let ui = to_ipc_settings(&s);
        let hid_on = s.hid_host();
        self.service.set_settings(s);
        self.publish(Event::Settings(ui));
        // Turned off: the keyboards and mice found are out of date
        if !hid_on && !self.snapshot().1.hid_candidates.is_empty() {
            self.publish(Event::HidCandidates(Vec::new()));
        }
        Ok(())
    }

    /// The Bluetooth LE keyboards and mice of a scan; published if they
    /// changed (each name or signal update of a scan calls this)
    fn publish_hid_candidates(&self, list: Vec<Device>) {
        if list != self.snapshot().1.hid_candidates {
            self.publish(Event::HidCandidates(list));
        }
    }

    /// hid.enable (after the confirmation) and hid.disable: the setting
    /// saved and applied as settings.set does
    fn set_hid_host(&self, on: bool) -> Result<Value, ErrorInfo> {
        let _tx = self.service.settings_tx();
        let mut s = self.service.settings();
        if s.hid_host != on {
            s.hid_host = on;
            self.save_settings(s)?;
        }
        Ok(Value::Null)
    }

    fn set_active_profile(&self, name: Option<String>) {
        let tx = self.service.settings_tx();
        let mut s = self.service.settings();
        if let Some(n) = &name
            && s.last_profile != *n
        {
            s.last_profile = n.clone();
            let _ = s.save(&self.config_dir());
            self.service.set_settings(s);
        }
        drop(tx);
        let (_, snap) = self.snapshot();
        self.publish(Event::Profiles { profiles: snap.profiles, active_profile: name });
    }

    /// The USB device of the adapter with this address
    pub(crate) fn adapter_instance(&self, address: &str) -> Option<String> {
        let (_, s) = self.snapshot();
        s.adapter
            .adapters
            .iter()
            .find(|a| a.address.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(address)))
            .map(|a| a.instance_id.clone())
    }

    pub fn refresh_adapters(&self) -> Vec<AdapterInfo> {
        let (list, fixed) = self.list_adapters();
        if fixed {
            // Files placed or replaced meanwhile: a stack started without
            // them keeps running the ROM code (v1 OnOpenFirmware)
            tracing::info!("firmware files are there now");
            self.restart_for_firmware();
        }
        list
    }

    /// The adapters anew, published; true if an adapter's firmware is there
    /// now that was missing or invalid
    fn list_adapters(&self) -> (Vec<AdapterInfo>, bool) {
        let (_, s) = self.snapshot();
        let (list, hung) = adapter_infos(&self.config_dir(), &self.service.settings());
        let change = firmware_change(&s.adapter.adapters, &list);
        warn_invalid_firmware(&paths::firmware_dir(&self.config_dir()), &change.invalid);
        let le_audio = le_audio_now(self.service.le_audio_supported(), &list);
        let other_radio = other_radio(&list);
        // What the new list tells about the adapter in use
        let active = s.adapter.active.as_ref().map(|a| active_adapter(a.vid, a.pid, &list));
        self.publish(Event::Adapter(AdapterState {
            adapters: list.clone(),
            le_audio,
            other_radio,
            hung,
            active,
            ..s.adapter
        }));
        (list, change.fixed)
    }

    /// The chip or firmware setting changed: the firmware state anew, and a
    /// stack that loads the firmware again (v1 reset_btstack)
    fn reload_adapter(&self) {
        self.list_adapters();
        self.restart_for_firmware();
    }

    /// A stack that loads the firmware again: stopped now when idle, else
    /// at its next use
    fn restart_for_firmware(&self) {
        let (_, s) = self.snapshot();
        let idle = !s.scanning && matches!(s.stream.state, StreamState::Idle | StreamState::Error);
        if self.service.reload_adapter(idle) {
            tracing::info!("adapter settings changed: Bluetooth stopped, it starts again at the next use");
            self.publish(Event::Adapter(AdapterState {
                status: AdapterStatus::Stopped,
                error: None,
                le_audio: le_audio_now(self.service.le_audio_supported(), &s.adapter.adapters),
                rom_firmware: false,
                ..s.adapter
            }));
        } else {
            tracing::info!("adapter settings changed: Bluetooth restarts at its next use");
        }
    }

    /// An adapter's driver changed (and the stack was released for it): the
    /// stack's error from before ("no adapter on WinUSB") is out of date,
    /// the next use tries again
    pub fn driver_changed(&self) {
        let (_, s) = self.snapshot();
        if s.adapter.status == AdapterStatus::Error {
            self.publish(Event::Adapter(AdapterState { status: AdapterStatus::Stopped, error: None, ..s.adapter }));
        }
        if s.stream.state == StreamState::Error && s.stream.message.key.starts_with("error.btstack") {
            self.publish(Event::Stream(StreamStatus {
                message: Msg::new("status.ready"),
                profile: s.stream.profile,
                ..StreamStatus::default()
            }));
        }
    }

    /// Starts streaming a profile (tray and UI)
    pub fn start_profile(&self, name: &str) -> Result<(), ErrorInfo> {
        let dir = self.config_dir();
        let settings = self.service.settings();
        let list = profiles::load(&dir);
        let p =
            list.iter().find(|p| p.name == name).ok_or_else(|| err("profile.not_found", "error.profile_not_found"))?;
        // Auto-select for every device: Auto in the device's order, whatever
        // codec the profile chose
        let auto = (settings.auto_codec && !p.le_audio).then(|| p.as_auto());
        if auto.is_some() && p.codec != "auto" {
            tracing::debug!(
                "profile {}: codec {} chosen, Auto-select streams Auto",
                bluecaster_core::diag::quoted(&p.name),
                p.codec
            );
        }
        let mut req = auto
            .as_ref()
            .unwrap_or(p)
            .to_request(settings.legacy_max_media_payload())
            .map_err(|_| err("profile.invalid_address", "error.invalid_address"))?;
        req.developer_codecs = settings.debug_mode;
        req.source = settings.source_in_force(req.source);
        req.volume_sync = effective_volume_sync(settings.volume_sync, req.volume_sync);
        req.mute_output = settings.mutes_output();
        req.sbc = settings.sbc_frame(req.sbc);
        if auto.is_some() && settings.debug_mode {
            req.codec_values = p.all_codec_values();
        }
        // Headset mode: on only with an output chosen
        let headset_mic = headset_mic_in_force(p, settings.debug_mode);
        if headset_mic.is_on() {
            tracing::debug!(
                "profile {}: headset microphone ({}) to {}",
                bluecaster_core::diag::quoted(&p.name),
                headset_mic.as_str(),
                bluecaster_core::diag::quoted(&p.mic_device_name)
            );
        }
        // Classic devices: the session switches to the voice link once
        // streaming, and refuses (a status message) an output that would
        // feed the capture (voice.rs); LE Audio: the LE session asks for the
        // conversational configuration and renders the microphone, with the
        // same refusal (le_stream.rs)
        set_headset_mic(&mut req, headset_mic, &p.mic_device_id);
        // The microphone's codec chosen, as the codec: only with Auto-select off
        set_mic_codec(&mut req, p, settings.auto_codec);
        self.set_active_profile(Some(name.to_string()));
        self.begin_stream(req, Some(name.to_string()));
        Ok(())
    }

    /// Starts streaming without a profile (stream.start_custom)
    pub fn start_custom(&self, c: &CustomStream) -> Result<(), ErrorInfo> {
        let settings = self.service.settings();
        let mut req = custom_request(c, settings.legacy_max_media_payload())?;
        req.developer_codecs = settings.debug_mode;
        req.mute_output = settings.mutes_output();
        tracing::info!("custom stream to {}: codec {}, source {}", c.device, c.codec, req.source);
        self.begin_stream(req, None);
        Ok(())
    }

    /// Error developer.off while developer mode is off
    fn developer_only(&self) -> Result<(), ErrorInfo> {
        if self.service.settings().debug_mode { Ok(()) } else { Err(err("developer.off", "ipc.error.developer_only")) }
    }

    /// stream.start_broadcast (developer mode)
    pub fn start_broadcast(&self, b: &BroadcastStream) -> Result<(), ErrorInfo> {
        self.developer_only()?;
        let settings = self.service.settings();
        let mut req = broadcast_request(b, settings.legacy_max_media_payload())?;
        req.mute_output = settings.mutes_output();
        tracing::info!("broadcast \"{}\": quality {}, source {}", b.name, b.quality, req.source);
        self.begin_stream(req, None);
        Ok(())
    }

    /// The stream running goes on under its profile's new name (a save that
    /// renamed it and kept the stream)
    fn rename_stream_profile(&self, name: &str) {
        *self.starting_profile.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_string());
        let stream = self.with_snapshot(|s| s.stream.clone());
        if stream.profile.as_deref() != Some(name) {
            self.publish(Event::Stream(StreamStatus { profile: Some(name.to_string()), ..stream }));
        }
    }

    fn begin_stream(&self, req: StreamRequest, profile: Option<String>) {
        *self.starting_profile.lock().unwrap_or_else(|e| e.into_inner()) = profile.clone();
        // The profile's playback EQ (none for a stream without one)
        let eq = profile
            .as_deref()
            .and_then(|name| profiles::load(&self.config_dir()).into_iter().find(|p| p.name == name))
            .map(|p| p.media_eq);
        media_eq::apply(eq.as_ref());
        self.publish(Event::Stream(StreamStatus {
            state: StreamState::Connecting,
            message: Msg::new("status.initializing"),
            profile,
            info: None,
            volume: None,
            battery: None,
            battery_refused: false,
            voice_codecs: Vec::new(),
            mic_rates: Vec::new(),
            voice: None,
            codec_stop: None,
            slow_link: None,
        }));
        let service = self.service.clone();
        // A restart for a save waits a moment: a request right behind it
        // takes over before the first connection is under way (issue #86)
        let restart = self.restarting.swap(false, Ordering::AcqRel);
        self.quiet_connect.store(restart, Ordering::Release);
        // start_streaming joins a running session first: not on a runtime
        // thread. The ticket, taken now, keeps the order of quick switches:
        // only the last one asked for starts (issue #86)
        let ticket = service.ticket();
        std::thread::spawn(move || {
            if restart {
                std::thread::sleep(RESTART_COALESCE);
            }
            service.start_streaming_for(req, ticket)
        });
    }

    /// Starts the profile in use again with what was saved (a codec chosen,
    /// Auto-select turned off): the same device, so no "connected" balloon,
    /// and a second save right behind it (Home turns Auto-select off, then
    /// saves the codec) is one restart, not two (issue #86: a second restart
    /// broke off the first one's connection)
    /// The adapter's state after the stack was shut down to start again
    fn publish_adapter_stopped(&self) {
        let (_, s) = self.snapshot();
        self.publish(Event::Adapter(AdapterState {
            status: AdapterStatus::Stopped,
            error: None,
            rom_firmware: false,
            ..s.adapter
        }));
    }

    /// profile.save: adds or replaces a profile; the stream of the profile
    /// in use takes the change in place or starts again
    fn save_profile(self: &Arc<Self>, profile: &Profile, original_name: Option<String>) -> Result<Value, ErrorInfo> {
        if profile.name.trim().is_empty() || bt_host::parse_addr(&profile.device_address).is_none() {
            return Err(err("profile.invalid", "error.invalid_address_format"));
        }
        let dir = self.config_dir();
        let mut list = profiles::load(&dir);
        let key = original_name.as_deref().unwrap_or(&profile.name).to_string();
        let pos = list.iter().position(|p| p.name == key);
        let mut merged = merge_profile(pos.map(|i| &list[i]), profile);
        if pos.is_none() {
            default_mic_output(&mut merged, &self.snapshot().1.stream, &list);
        }
        let known = if merged.le_audio { devices::load_le(&dir) } else { devices::load(&dir) };
        // Another earbud of a saved set is that saved device: its
        // profile is the saved device's, no second entry
        if merged.le_audio
            && let Some(d) = self.service.known_le_sets().saved_entry(&merged.device_address, &known)
            && !d.address.eq_ignore_ascii_case(&merged.device_address)
        {
            tracing::info!("profile: a member of a saved coordinated set, saved for the set's device");
            merged.device_address = d.address.clone();
        }
        let before = pos.map(|i| list[i].clone());
        // Only the device's codec order changed (its cards dragged):
        // the stream goes on unless the order changes its codec
        let order_only = pos.is_some_and(|i| {
            list[i].codec_order != merged.codec_order
                && CoreProfile { codec_order: merged.codec_order.clone(), ..list[i].clone() } == merged
        });
        let auto_select = self.service.settings().auto_codec;
        // Settings of a codec not in use: nothing the stream uses
        // The playback EQ and the microphone's processing (one or
        // both): the running stream takes them in place
        let live_only = pos.is_some_and(|i| list[i].only_live_processing_differs(&merged));
        let processing_only = live_only && pos.is_some_and(|i| list[i].mic_processing != merged.mic_processing);
        let kept_only = live_only
            || pos.is_some_and(|i| {
                list[i].only_kept_settings_differ(&merged) || list[i].only_default_input_differs(&merged)
            });
        let in_order = merged.clone();
        // The profile's device joins the saved ones of its kind (as
        // a connection saves it)
        let address = merged.device_address.clone();
        if !known.iter().any(|d| d.address == address) {
            let r = if merged.le_audio {
                devices::save_le(&dir, &address, &merged.device_name)
            } else {
                devices::save(&dir, &address, &merged.device_name)
            };
            if let Err(e) = r {
                tracing::warn!("cannot save the device {address}: {e}");
            }
        }
        match pos {
            Some(i) => list[i] = merged,
            None => list.push(merged),
        }
        let (_, s) = self.snapshot();
        let renamed_active = s.active_profile.as_deref() == Some(key.as_str()) && key != profile.name;
        let active = match s.active_profile {
            Some(a) if a == key => Some(profile.name.clone()),
            other => other.or(Some(profile.name.clone())),
        };
        self.save_profiles(&list, active)?;
        // A renamed active profile (its name follows the codec chosen
        // on the Codecs screen) stays the one remembered
        if renamed_active {
            self.set_active_profile(Some(profile.name.clone()));
        }
        self.refresh_devices();
        // The stream of this profile plays its EQ as saved from now
        // on (also when it starts again below)
        if restart_after_save(&s.stream, &key) {
            media_eq::apply(Some(&in_order.media_eq));
        }
        // The profile in use starts again with what was saved (v1),
        // but for another source, which needs nothing of the link
        if processing_only && restart_after_save(&s.stream, &key) {
            self.service.set_mic_processing(in_order.mic_processing);
        }
        let order_keeps_codec = order_only && !restart_for_order(&s.stream, &in_order, auto_select);
        let headset = headset_change(&s.stream, before.as_ref(), &in_order);
        let headset_kept = match headset {
            Some(HeadsetChange::Unused) => true,
            Some(HeadsetChange::InPlace) => {
                tracing::info!(
                    "profile {} saved while in use: headset microphone {} in place",
                    bluecaster_core::diag::quoted(&key),
                    in_order.headset_mic_mode().as_str()
                );
                // The session refuses an output that would feed the
                // capture; false (no stream, or a stack without
                // voice): it starts again instead
                let mode = headset_mic_in_force(&in_order, self.service.settings().debug_mode);
                let kept = self.service.set_headset_mic(
                    mode,
                    mode.is_on().then(|| in_order.mic_device_id.clone()),
                    in_order.voice_codec_in_force(auto_select),
                );
                // Another output or mode: the session tries again
                // and says so anew if it still cannot
                let mut stream = self.with_snapshot(|s| s.stream.clone());
                if kept && self.drop_mic_note(&mut stream) {
                    self.publish(Event::Stream(stream));
                }
                kept
            }
            Some(HeadsetChange::Restart) | None => false,
        };
        if restart_after_save(&s.stream, &key) && !order_keeps_codec && !kept_only && !headset_kept {
            let in_place = source_changes_in_place(&s.stream, before.as_ref(), &in_order)
                && self.service.change_source(self.service.settings().source_in_force(in_order.source()));
            let bitpools_in_place = !self.service.settings().auto_codec
                && bitpools_change_in_place(&s.stream, before.as_ref(), &in_order)
                && self.service.change_bitpools(in_order.bitpools());
            // "Show only options it supports" turned, the stream
            // set up the same either way (issue #86)
            let unlisted_in_place = !bitpools_in_place
                && !in_place
                && before.as_ref().is_some_and(|b| b.only_unlisted_differs(&in_order))
                && self.service.change_unlisted(in_order.unlisted_options);
            let named_in_place = !bitpools_in_place
                && !in_place
                && !unlisted_in_place
                && codec_named_in_place(&s.stream, before.as_ref(), &in_order)
                && (before.as_ref().is_some_and(|b| b.unlisted_options == in_order.unlisted_options)
                    || self.service.change_unlisted(in_order.unlisted_options));
            if named_in_place {
                tracing::info!(
                    "profile {} saved while in use as {}: it names the codec it streams with, the stream stays",
                    bluecaster_core::diag::quoted(&key),
                    bluecaster_core::diag::quoted(&profile.name)
                );
                self.rename_stream_profile(&profile.name);
            } else if unlisted_in_place {
                tracing::info!(
                    "profile {} saved while in use: the stream stays as it is",
                    bluecaster_core::diag::quoted(&key)
                );
            } else if bitpools_in_place {
                tracing::debug!("profile {} saved while in use: new bitpools", bluecaster_core::diag::quoted(&key));
            } else if in_place {
                tracing::info!(
                    "profile {} saved while in use: its source changes in place",
                    bluecaster_core::diag::quoted(&key)
                );
            } else {
                tracing::info!(
                    "profile {} saved while in use: it starts again as {}",
                    bluecaster_core::diag::quoted(&key),
                    bluecaster_core::diag::quoted(&profile.name)
                );
                self.restart_profile(&profile.name)?;
            }
        } else if reconnect_after_codec_stop(&s.stream, &key, before.as_ref(), &in_order) {
            tracing::info!(
                "profile {} saved after its codec stopped the stream: connecting again as {}",
                bluecaster_core::diag::quoted(&key),
                bluecaster_core::diag::quoted(&profile.name)
            );
            self.start_profile(&profile.name)?;
        }
        Ok(Value::Null)
    }

    /// Asks the headphones for this volume (0..127): AVRCP, or VCP for LE
    /// Audio. Nothing without a stream.
    fn request_volume(&self, volume: u8) {
        if let Some(h) = self.service.host() {
            // LE Audio: through VCP, not AVRCP (issue #86)
            if self.service.le_stream().is_none() || !h.le_request_volume(volume.min(127)) {
                h.request_volume(volume.min(127));
            }
        }
    }

    /// A tool's change of one value of a profile (profile.set_headset_mic,
    /// set_mic_muted, set_media_eq): `change` gives the profile `name` (None:
    /// [`tool_profile`]) with it, None when it has it already: nothing
    /// happens then. Saved as the window saves it (profile.save), so the
    /// stream takes it as from the window and every client gets the profiles.
    /// `mic`: a change the stream may start again for, refused while that
    /// stream connects (a tool that repeats it cannot keep it reconnecting).
    fn save_tool_change(
        self: &Arc<Self>,
        name: Option<String>,
        mic: bool,
        change: impl FnOnce(&Profile) -> Result<Option<Profile>, ErrorInfo>,
    ) -> Result<Value, ErrorInfo> {
        let (_, s) = self.snapshot();
        let p = tool_profile(&s, name.as_deref())?;
        let Some(changed) = change(p)? else { return Ok(Value::Null) };
        if mic && connecting(&s.stream, &p.name) {
            return Err(err("stream.busy", "ipc.error.stream_busy"));
        }
        self.save_profile(&changed, None)
    }

    fn restart_profile(&self, name: &str) -> Result<(), ErrorInfo> {
        self.restarting.store(true, Ordering::Release);
        let r = self.start_profile(name);
        self.restarting.store(false, Ordering::Release);
        r
    }

    pub fn stop(&self) {
        let service = self.service.clone();
        let ticket = service.ticket();
        std::thread::spawn(move || service.disconnect_for(ticket));
    }

    /// Settings > the flyout above Quick Settings
    pub fn flyout_with_quick_settings(&self) -> bool {
        self.service.settings().flyout_with_quick_settings
    }

    /// A click on the tray icon opens the flyout, not the main window:
    /// Settings > where the flyout opens is the tray icon
    pub fn tray_click_opens_flyout(&self) -> bool {
        let s = self.service.settings();
        !s.tray_click_opens_window && !s.flyout_with_quick_settings
    }

    pub fn quit(&self) {
        self.default_input.release_now();
        self.publish(Event::Bye);
        (self.quit)();
    }

    /// Start with Windows: at start the Run value of v1 and 2.0.x
    /// ("A2DPWB") is taken over when it names v1 or a core (when
    /// `replaces_v1`), and ours ("BlueCaster") is written when it is missing
    /// or names a copy of the core that is gone (the program moved). One
    /// naming another copy of the core that exists (the installed copy while
    /// a test build runs) is left alone; the settings toggle still writes
    /// it. See autostart_plan. The values are read and written by the
    /// session host (the agent's user's when the agent does the session's
    /// work).
    pub fn refresh_autostart(&self, replaces_v1: bool) {
        use session_host::autostart::get;
        use win_sys::autostart::{OLD_VALUE_NAME, VALUE_NAME};
        // Service mode: the Run value belongs to the per-user core
        // (apply_autostart)
        if bluecaster_core::paths::service_mode() {
            return;
        }
        let s = self.service.settings();
        let (current, old) = match (get(VALUE_NAME), get(OLD_VALUE_NAME)) {
            (Ok(c), Ok(o)) => (c, o),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("cannot read the start with Windows entry: {e}");
                return;
            }
        };
        let plan =
            autostart_plan(current.as_deref(), old.as_deref(), s.start_with_windows, replaces_v1, |p| p.exists());
        if plan.ours == AutostartFix::Keep {
            tracing::debug!("start with Windows entry kept: {}", current.as_deref().unwrap_or_default());
        } else {
            tracing::info!("start with Windows entry {:?}: was {}", plan.ours, current.as_deref().unwrap_or_default());
            if let Err(e) = apply_autostart(plan.ours == AutostartFix::Write, s.start_minimized) {
                tracing::warn!("cannot refresh the start with Windows entry: {e}");
                // The old entry still starts the program: keep it
                return;
            }
        }
        if plan.remove_old {
            tracing::info!("old start with Windows entry {OLD_VALUE_NAME} removed: was {}", old.unwrap_or_default());
            if let Err(e) = session_host::autostart::remove(OLD_VALUE_NAME) {
                tracing::warn!("cannot remove the old start with Windows entry {OLD_VALUE_NAME}: {e}");
            }
            // Disabled in Task Manager's Startup tab stays disabled
            match session_host::autostart::carry_startup_approved(OLD_VALUE_NAME, VALUE_NAME) {
                Ok(true) => {
                    tracing::info!("Task Manager's startup state of {OLD_VALUE_NAME} carried over to {VALUE_NAME}")
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("cannot carry over Task Manager's startup state of {OLD_VALUE_NAME}: {e}"),
            }
        }
    }

    // ---- Tray balloons ------------------------------------------------------

    fn tr(&self, key: &str, default: &str) -> String {
        bluecaster_core::messages::translate(&self.service.settings().language, key, default)
    }

    // ---- Crash reports -------------------------------------------------------

    /// Lists the crash reports not dismissed yet, if they changed
    fn refresh_crashes(&self) {
        let now = crate::crashes::unseen(&self.config_dir(), &self.service.settings());
        if self.with_snapshot(|s| s.crashes != now) {
            self.publish(Event::Crashes(now));
        }
    }

    /// The reports shown were seen: only newer ones are shown from now on
    fn crashes_seen(&self) {
        let Some(newest) = self.with_snapshot(|s| s.crashes.first().map(|c| c.name.clone())) else { return };
        let _tx = self.service.settings_tx();
        let mut s = self.service.settings();
        crate::crashes::mark_seen(&mut s, &newest);
        if let Err(e) = s.save(&self.config_dir()) {
            tracing::warn!("cannot save the settings: {e}");
        }
        self.service.set_settings(s);
        self.publish(Event::Crashes(Vec::new()));
    }

    /// The UI hid its window to the tray (app.hidden_to_tray): v1's "still
    /// running in the tray" balloon, once
    pub fn hidden_to_tray(&self) {
        let _tx = self.service.settings_tx();
        let mut s = self.service.settings();
        if s.other.get(TRAY_HINT_SHOWN).and_then(Value::as_bool) == Some(true) {
            return;
        }
        // No title: Windows heads the notification with the app's name
        // already (issue #86: the name three times)
        (self.balloon)(BalloonRequest {
            title: String::new(),
            text: self.tr("tray.minimized_hint", "BlueCaster is still running in the system tray."),
            url: None,
        });
        s.other.insert(TRAY_HINT_SHOWN.into(), Value::Bool(true));
        if let Err(e) = s.save(&self.config_dir()) {
            tracing::warn!("cannot save the settings: {e}");
        }
        self.service.set_settings(s);
    }

    /// The streamed headphones' battery went low: a balloon, once per
    /// connection ([`battery_low_alert`])
    fn battery_low(&self, s: &Snapshot, level: u8, low: u8) {
        let device = streamed_device(s).unwrap_or("BlueCaster");
        tracing::info!("battery: {device} at {level}%, below {low}%: balloon");
        let template = self.tr("notify.battery_low", "%s: battery low (%d%%)");
        // No title: Windows heads the notification with the app's name
        // already (issue #86: the name three times)
        (self.balloon)(BalloonRequest {
            title: String::new(),
            text: bluecaster_core::messages::format_printf(&template, &[device.to_string(), level.to_string()]),
            url: None,
        });
    }

    /// A new connection streams with its codec settled (the stream info
    /// comes after the Streaming state on a first connection, before it on a
    /// reconnect): the balloon with what it runs ([`connected_balloon`]),
    /// once, when the setting is on. A battery not reported yet is waited
    /// for a moment (issue #86: the balloon came before it on some devices)
    fn announce_connected(self: &Arc<Self>) {
        let (_, s) = self.snapshot();
        if s.stream.state != StreamState::Streaming || s.stream.info.is_none() {
            return;
        }
        if !self.service.settings().notify_connected {
            self.connect_announce.store(false, Ordering::Release);
            return;
        }
        if !self.connect_announce.load(Ordering::Acquire) {
            return;
        }
        if s.stream.battery.is_none() && !s.stream.battery_refused && self.wait_for_battery() {
            return;
        }
        if !self.connect_announce.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(b) = connected_balloon(&s.settings.language, streamed_device(&s), &s.stream) else { return };
        tracing::info!("connected balloon: {} / {}", b.title, b.text.replace('\n', " / "));
        (self.balloon)(b);
    }

    /// Whether the balloon still waits for the battery; the first call
    /// starts the wait, which ends here again without a report. A wait left
    /// by an earlier connection finds this one's later deadline.
    fn wait_for_battery(self: &Arc<Self>) -> bool {
        let now = Instant::now();
        let mut deadline = self.battery_deadline.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = *deadline {
            return now < d;
        }
        *deadline = Some(now + BATTERY_WAIT);
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(BATTERY_WAIT);
            me.announce_connected();
        });
        true
    }

    /// The automatic check found a newer release: a balloon whose click
    /// opens its page (v1)
    fn announce_update(&self, u: &UpdateState) {
        let Some(a) = &u.available else { return };
        let text = self
            .tr("update.balloon", "BlueCaster %s is available. Click here to open the download page.")
            .replacen("%s", &a.version, 1);
        (self.balloon)(BalloonRequest {
            title: self.tr("update.available_title", "Update Available"),
            text,
            url: Some(a.url.clone()),
        });
    }

    // ---- Audio rates (flow) ------------------------------------------------

    pub fn set_rates(&self, r: Option<AudioRates>) {
        *self.rates.lock().unwrap_or_else(|e| e.into_inner()) = r;
    }

    pub fn rates(&self) -> Option<AudioRates> {
        *self.rates.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A chip script read for the adapter `instance_id`: what cannot run,
    /// what may change the chip, what its vendor never takes from a script
    fn check_script(
        &self,
        instance_id: &str,
        text: &str,
    ) -> (Result<bt_host::script::Script, Vec<String>>, ScriptCheck) {
        let parsed = bt_host::script::parse(text);
        let (_, s) = self.snapshot();
        let vendor = s
            .adapter
            .adapters
            .iter()
            .find(|a| a.instance_id == instance_id)
            .map_or(ChipVendor::Unknown, |a| chips::vendor_for_usb(a.vid, a.pid, None));
        let line = |l: &bt_host::script::ScriptLine| format!("line {}: {}", l.line, l.text);
        let check = match &parsed {
            Ok(script) => ScriptCheck {
                errors: Vec::new(),
                writes: script.writes().into_iter().map(line).collect(),
                denied: script.denied(chips::denied_opcodes(vendor)).into_iter().map(line).collect(),
            },
            Err(errors) => ScriptCheck { errors: errors.clone(), writes: Vec::new(), denied: Vec::new() },
        };
        (parsed, check)
    }

    // ---- HCI capture -------------------------------------------------------

    fn start_capture(&self) -> Result<String, ErrorInfo> {
        let capture = self.service.capture();
        if let Some(p) = capture.path() {
            return Ok(capture_shown(&p));
        }
        let path = paths::capture_file(&self.config_dir(), &local_stamp());
        capture.start(&path).map_err(|_| err("diag.capture_failed", "error.capture_start"))?;
        let path = capture_shown(&path);
        self.publish(Event::Capture(Some(CaptureState {
            path: path.clone(),
            bytes: capture.bytes_written(),
            started_ms: unix_ms(),
        })));
        Ok(path)
    }

    /// Stops the capture: its file and final size (None: none ran)
    fn stop_capture(&self) -> Option<CaptureState> {
        let capture = self.service.capture();
        let known = self.with_snapshot(|s| s.capture.clone());
        let path = capture.path();
        capture.stop();
        let done = match path {
            Some(p) => Some(CaptureState {
                path: capture_shown(&p),
                bytes: capture.bytes_written(),
                started_ms: known.as_ref().map_or(0, |k| k.started_ms),
            }),
            None => known.clone(),
        };
        if known.is_some() {
            self.publish(Event::Capture(None));
        }
        done
    }

    /// Verbose logging ran out (the sampler)
    pub fn expire_verbose(&self) {
        if crate::logging::expire_verbose() {
            self.publish(Event::Verbose(None));
        }
    }

    /// The running capture's size (the sampler, every few seconds)
    pub fn refresh_capture(&self) {
        let Some(known) = self.with_snapshot(|s| s.capture.clone()) else { return };
        let capture = self.service.capture();
        match capture.path() {
            None => self.publish(Event::Capture(None)),
            Some(_) => {
                let bytes = capture.bytes_written();
                if bytes != known.bytes {
                    self.publish(Event::Capture(Some(CaptureState { bytes, ..known })));
                }
            }
        }
    }

    // ---- Receiver test -----------------------------------------------------

    pub fn receiver_subscribed(&self) -> bool {
        self.receiver_subscribers.load(Ordering::Acquire) > 0
    }

    pub fn receiver_state(&self) -> ReceiverState {
        let (status, detail) = self.receiver.status();
        let connection = match status {
            receiver::ReceiverStatus::Off => ReceiverConnection::Off,
            receiver::ReceiverStatus::Connecting => ReceiverConnection::Connecting,
            receiver::ReceiverStatus::Connected => ReceiverConnection::Connected,
            receiver::ReceiverStatus::Failed => ReceiverConnection::Failed,
        };
        let stats = self.receiver.latest();
        let (total, start, starts) = link_totals(&self.service.stats());
        ReceiverState {
            connection,
            detail,
            target: if connection == ReceiverConnection::Off { String::new() } else { self.receiver.target() },
            receiver: self.receiver.receiver().as_ref().and_then(convert),
            stats_age_ms: stats.as_ref().and_then(|s| s.received).map(|t| t.elapsed().as_millis() as u64),
            stats: stats.as_ref().and_then(convert),
            sent: (starts > 0).then(|| total.since(&start)),
            stream_starts: starts,
        }
    }

    /// Sends the receiver state to its subscribers
    pub fn publish_receiver(&self) {
        if self.receiver_subscribed() {
            self.publish(Event::Receiver(Box::new(self.receiver_state())));
        }
    }

    // ---- Requests ----------------------------------------------------------

    /// One request; blocking work runs off the runtime's threads
    pub async fn handle(self: &Arc<Self>, method: Method) -> Result<Value, ErrorInfo> {
        let me = self.clone();
        let blocking = |f: BlockingJob| async move {
            tokio::task::spawn_blocking(move || f(&me))
                .await
                .unwrap_or_else(|_| Err(err("internal", "error.btstack_crash_stream")))
        };
        match method {
            Method::StateSnapshot => {
                // A window that opens after a crash (of itself, or of the
                // core while it was closed) finds the report
                self.refresh_crashes();
                Ok(json(&self.snapshot().1))
            }
            Method::StreamStart { profile } => self.start_profile(&profile).map(|()| Value::Null),
            Method::StreamStartCustom {
                device,
                device_name,
                codec,
                quality,
                sample_rate,
                bit_depth,
                source,
                source_id,
                source_name,
                max_media_payload,
                abr,
                lhdcv5_low_latency,
                codec_fallback,
            } => {
                let c = CustomStream {
                    device,
                    device_name,
                    codec,
                    quality,
                    sample_rate,
                    bit_depth,
                    source,
                    source_id,
                    source_name,
                    max_media_payload,
                    abr,
                    lhdcv5_low_latency,
                    codec_fallback,
                };
                self.start_custom(&c).map(|()| Value::Null)
            }
            Method::StreamStartBroadcast {
                name,
                code,
                quality,
                rear_quality,
                mono,
                split,
                layout,
                source,
                source_id,
                source_name,
            } => {
                let b = BroadcastStream {
                    name,
                    code,
                    quality,
                    rear_quality,
                    mono,
                    split,
                    layout,
                    source,
                    source_id,
                    source_name,
                };
                self.start_broadcast(&b).map(|()| Value::Null)
            }
            Method::StreamStop => {
                self.stop();
                Ok(Value::Null)
            }
            Method::StreamSetVolume { volume } => {
                self.request_volume(volume);
                Ok(Value::Null)
            }
            Method::StreamVolumeStep { delta } => {
                let stream = self.with_snapshot(|s| s.stream.clone());
                let volume = volume_step(&stream, delta)?;
                if stream.volume != Some(volume) {
                    tracing::debug!("IPC: stream.volume_step {delta}: volume {volume}");
                    self.request_volume(volume);
                }
                Ok(json(&volume))
            }
            Method::ProfileSetHeadsetMic { name, mode } => {
                let developer = self.service.settings().debug_mode;
                self.save_tool_change(name, true, |p| tool_headset_mic(p, mode, developer))
            }
            Method::ProfileSetMicMuted { name, muted } => {
                self.save_tool_change(name, true, |p| tool_mic_muted(p, muted))
            }
            Method::ProfileSetMediaEq { name, enabled } => {
                self.save_tool_change(name, false, |p| Ok(tool_media_eq(p, enabled)))
            }
            Method::DeviceScan { seconds } => {
                blocking(Box::new(move |me| {
                    me.publish(Event::Scanning(true));
                    // The list shows the devices as they answer: first the
                    // saved ones alone (the last scan's are gone), then each
                    // device found. A scan refused (streaming) keeps the list.
                    let dir = me.config_dir();
                    let codecs = me.service.device_codecs();
                    if !me.service.is_running() {
                        me.publish(Event::Devices(saved_devices(&dir, &[], &codecs)));
                        me.publish(Event::HidCandidates(Vec::new()));
                    }
                    let mut on_found = |found: &[ScannedDevice]| {
                        let (scanned, hid) = split_scan(found);
                        me.publish(Event::Devices(saved_devices(&dir, &scanned, &codecs)));
                        me.publish_hid_candidates(hid);
                    };
                    // Classic, then (LE Audio on) the LE Audio devices
                    let r = me.service.scan_devices(seconds.clamp(3, ipc::MAX_SCAN_SECONDS), &mut on_found);
                    if me.with_snapshot(|s| s.le_scanning) {
                        me.publish(Event::LeScanning(false));
                    }
                    me.publish(Event::Scanning(false));
                    let found = r.map_err(|m| ErrorInfo { code: "adapter.init_failed".into(), message: msg(&m) })?;
                    let (scanned, hid) = split_scan(&found);
                    let list = saved_devices(&me.config_dir(), &scanned, &me.service.device_codecs());
                    me.publish(Event::Devices(list.clone()));
                    me.publish_hid_candidates(hid);
                    Ok(json(&list))
                }))
                .await
            }
            Method::DeviceScanStop => {
                self.service.stop_scan();
                Ok(Value::Null)
            }
            Method::DeviceWindowsPaired => blocking(Box::new(|_| Ok(json(&windows_paired())))).await,
            Method::DeviceWindowsPairedThrough { adapter } => {
                let Some(instance_id) = self.adapter_instance(&adapter) else {
                    return Ok(json(&Vec::<WindowsPairedDevice>::new()));
                };
                // The audio devices: only those are saved by an import
                blocking(Box::new(move |_| {
                    let list: Vec<WindowsPairedDevice> = win_sys::bt_paired::paired_through(&instance_id)
                        .iter()
                        .filter(|d| d.is_a2dp_sink())
                        .map(|d| WindowsPairedDevice {
                            address: bt_host::addr_str(&d.address),
                            name: d.name.clone(),
                            audio: d.is_a2dp_sink(),
                        })
                        .collect();
                    Ok(json(&list))
                }))
                .await
            }
            Method::DevicePairing { address } => crate::pairing::pairing(self, &address),
            Method::DeviceServices { address, le_audio } => {
                blocking(Box::new(move |me| crate::pairing::services(me, &address, le_audio == Some(true)))).await
            }
            Method::DeviceImportWindowsPairings { adapter, addresses } => {
                blocking(Box::new(move |me| crate::pairing::import_windows(me, &adapter, addresses.as_deref()))).await
            }
            Method::DevCompareWindowsKeys { adapter } => {
                blocking(Box::new(move |me| crate::driver::compare_windows_keys(me, &adapter))).await
            }
            Method::DeviceForget { address, le_audio } => {
                blocking(Box::new(move |me| me.forget_device(&address, le_audio == Some(true)))).await
            }
            Method::DeviceForgetCodecs { address } => {
                if bt_host::parse_addr(&address).is_none() {
                    return Err(err("device.invalid_address", "error.invalid_address_format"));
                }
                if self.service.forget_device_codecs(&address) {
                    self.refresh_devices();
                }
                Ok(Value::Null)
            }
            Method::DeviceRename { address, name, le_audio } => {
                let name = name.trim();
                let Some(addr) = bt_host::parse_addr(&address) else {
                    return Err(err("device.invalid_address", "error.invalid_address_format"));
                };
                if name.is_empty() || name.contains(['\n', '\r', '|']) {
                    return Err(bad_request("device.invalid_name", "device.rename", format!("name \"{name}\"")));
                }
                let (dir, address) = (self.config_dir(), bt_host::addr_str(&addr));
                let r = if le_audio == Some(true) {
                    devices::save_le(&dir, &address, name)
                } else {
                    devices::save(&dir, &address, name)
                };
                r.map_err(|_| err("device.rename_failed", "error.device_rename"))?;
                self.refresh_devices();
                Ok(Value::Null)
            }
            Method::ProfileSave { profile, original_name } => self.save_profile(&profile, original_name),
            Method::AuracastSetSource { source } => {
                self.developer_only()?;
                let source = match source.trim().to_ascii_lowercase().as_str() {
                    "tone" => bluecaster_core::stream::SourceRequest::TestTone,
                    "" | "loopback" => bluecaster_core::stream::SourceRequest::SystemLoopback,
                    other => {
                        return Err(bad_request(
                            "stream.bad_source",
                            "auracast.set_source",
                            format!("unknown source \"{other}\""),
                        ));
                    }
                };
                Ok(Value::Bool(self.service.change_source(source)))
            }
            Method::AuracastMixGet { channels, rear } => {
                self.developer_only()?;
                Ok(json(&bluecaster_core::auracast_mix::state(&self.config_dir(), channels, rear)))
            }
            Method::AuracastMixSet { mix, channels, rear } => {
                self.developer_only()?;
                let dir = self.config_dir();
                bluecaster_core::auracast_mix::set(&dir, &mix).map_err(|e| {
                    tracing::warn!("Auracast mixer: not saved: {e}");
                    err("auracast.mix_save_failed", "error.settings_save")
                })?;
                Ok(json(&bluecaster_core::auracast_mix::state(&dir, channels, rear)))
            }
            Method::AuracastMixPresetSave { name, mix } => {
                self.developer_only()?;
                let dir = self.config_dir();
                bluecaster_core::auracast_mix::save_preset(&dir, &name, &mix).map_err(|e| {
                    tracing::warn!("Auracast mixer: preset not saved: {e}");
                    err("auracast.mix_save_failed", "error.settings_save")
                })?;
                Ok(json(&bluecaster_core::auracast_mix::state(&dir, None, None)))
            }
            Method::AuracastMixPresetDelete { name } => {
                self.developer_only()?;
                let dir = self.config_dir();
                bluecaster_core::auracast_mix::delete_preset(&dir, &name).map_err(|e| {
                    tracing::warn!("Auracast mixer: preset not deleted: {e}");
                    err("auracast.mix_save_failed", "error.settings_save")
                })?;
                Ok(json(&bluecaster_core::auracast_mix::state(&dir, None, None)))
            }
            Method::MediaEqPreview { name, eq } => {
                let (_, s) = self.snapshot();
                if restart_after_save(&s.stream, &name) {
                    media_eq::apply(Some(&MediaEq::from_ipc(&eq)));
                }
                Ok(Value::Null)
            }
            Method::ProfileDelete { name } => {
                let mut list = profiles::load(&self.config_dir());
                list.retain(|p| p.name != name);
                let (_, s) = self.snapshot();
                let active = s.active_profile.filter(|a| *a != name);
                self.save_profiles(&list, active)?;
                Ok(Value::Null)
            }
            Method::ProfileSetActive { name } => {
                self.set_active_profile(Some(name));
                Ok(Value::Null)
            }
            Method::SettingsSet { settings } => {
                blocking(Box::new(move |me| {
                    let base = me.service.settings();
                    let merged = merge_settings(&base, &settings);
                    if autostart_changed(&base, &merged) {
                        apply_autostart(merged.start_with_windows, merged.start_minimized).map_err(|e| {
                            tracing::warn!("start with Windows: {e}");
                            err("settings.autostart_failed", "error.autostart")
                        })?;
                    }
                    let chip = chip_changed(&base, &merged);
                    let le_toggled = base.le_audio() != merged.le_audio();
                    let auto_toggled = base.auto_codec != merged.auto_codec;
                    let developer_off = base.debug_mode && !merged.debug_mode;
                    let developer_toggled = base.debug_mode != merged.debug_mode;
                    let sync_toggled =
                        base.volume_sync != merged.volume_sync || base.mutes_output() != merged.mutes_output();
                    {
                        // Merged again into the settings as they are now:
                        // another change may have been saved since `base`
                        let _tx = me.service.settings_tx();
                        let merged = merge_settings(&me.service.settings(), &settings);
                        me.save_settings(merged)?;
                    }
                    if chip {
                        me.reload_adapter();
                    }
                    // Developer mode on / off: a classic stream of a profile
                    // saved as On switches between On and Auto in place
                    if developer_toggled {
                        let (_, s) = me.snapshot();
                        let list = profiles::load(&me.config_dir());
                        let now = me.service.settings();
                        let saved_on = |p: &&CoreProfile| {
                            !p.le_audio && p.headset_mic_mode() == HeadsetMic::On && !p.mic_device_id.is_empty()
                        };
                        if let Some(p) =
                            s.stream.profile.as_deref().and_then(|n| list.iter().find(|p| p.name == n)).filter(saved_on)
                        {
                            let mode = headset_mic_in_force(p, now.debug_mode);
                            let kept = me.service.set_headset_mic(
                                mode,
                                Some(p.mic_device_id.clone()),
                                p.voice_codec_in_force(now.auto_codec),
                            );
                            tracing::info!(
                                "developer mode {}: headset microphone {} in place: {kept}",
                                if now.debug_mode { "on" } else { "off" },
                                mode.as_str()
                            );
                        }
                    }
                    // Auto-select on / off: a profile in use whose codec it
                    // changes starts again (as a profile save does); Windows
                    // Volume Sync or the output mute on / off: the profile in
                    // use starts again (as when it was the profile's own setting)
                    if auto_toggled || sync_toggled {
                        let (_, s) = me.snapshot();
                        let list = profiles::load(&me.config_dir());
                        let in_use = s.stream.profile.as_deref().and_then(|n| list.iter().find(|p| p.name == n));
                        let known = me.service.device_codecs();
                        let own_settings = settings.developer_mode && settings.dev_overrides.auto_codec_settings;
                        let auto_changes = |p: &CoreProfile| {
                            let codecs = known.get(&p.device_address.to_uppercase());
                            let same = same_after_auto_toggle(&s.stream, p, settings.auto_codec, codecs, own_settings);
                            if same {
                                tracing::info!(
                                    "Auto-select {}: profile {} streams the same, no restart",
                                    if settings.auto_codec { "on" } else { "off" },
                                    bluecaster_core::diag::quoted(&p.name)
                                );
                            }
                            restart_for_auto(&s.stream, p) && !same
                        };
                        // An LE Audio microphone's rate chosen: set up again
                        let mic_changes = |p: &CoreProfile| {
                            p.le_audio && mic_choice_matters(p) && restart_after_save(&s.stream, &p.name)
                        };
                        let restart = |p: &&CoreProfile| {
                            (auto_toggled && (auto_changes(p) || mic_changes(p)))
                                || (sync_toggled && restart_after_save(&s.stream, &p.name))
                        };
                        // Auto-select turned on after the codec stopped the
                        // stream: the device connects again with Auto's
                        // codec (issue #86)
                        let after_stop = in_use
                            .filter(|p| auto_toggled && settings.auto_codec && stopped_by_codec(&s.stream, &p.name));
                        if let Some(p) = after_stop {
                            tracing::info!(
                                "Auto-select on after the codec stopped profile {}: connecting again",
                                bluecaster_core::diag::quoted(&p.name)
                            );
                            me.start_profile(&p.name.clone())?;
                        } else if let Some(p) = in_use.filter(restart) {
                            tracing::info!(
                                "Auto-select {}, volume sync {}, output mute {}: profile {} starts again",
                                if settings.auto_codec { "on" } else { "off" },
                                if settings.volume_sync { "on" } else { "off" },
                                if settings.mute_output { "on" } else { "off" },
                                bluecaster_core::diag::quoted(&p.name)
                            );
                            me.restart_profile(&p.name.clone())?;
                        } else if let Some(p) = in_use.filter(|p| auto_toggled && !p.le_audio && mic_choice_matters(p))
                        {
                            // A call codec chosen: the classic stream's
                            // voice link takes Auto's or the chosen one in
                            // place, or the profile starts again
                            let mode = headset_mic_in_force(p, me.service.settings().debug_mode);
                            let codec = p.voice_codec_in_force(settings.auto_codec);
                            if !me.service.set_headset_mic(mode, Some(p.mic_device_id.clone()), codec)
                                && restart_after_save(&s.stream, &p.name)
                            {
                                tracing::info!(
                                    "Auto-select {}: the call codec of profile {} changes, it starts again",
                                    if settings.auto_codec { "on" } else { "off" },
                                    bluecaster_core::diag::quoted(&p.name)
                                );
                                me.restart_profile(&p.name.clone())?;
                            }
                        }
                    }
                    // Developer mode off while one of its codecs streams
                    // (issue #86): the profile starts again without it
                    if developer_off {
                        let (_, s) = me.snapshot();
                        if let Some(name) = s.stream.profile.clone().filter(|_| streams_developer_codec(&s.stream)) {
                            tracing::info!(
                                "developer mode off: profile {} streams a codec of developer mode, starts again",
                                bluecaster_core::diag::quoted(&name)
                            );
                            me.restart_profile(&name)?;
                        }
                    }
                    // LE Audio on / off while a device is connected (issue
                    // #86): the stack starts again at once, and the device
                    // connects again over what is now on (its LE Audio
                    // profile, or its classic one)
                    if le_toggled {
                        // LE Audio off: a device paired over LE only, whose
                        // pairing gave a classic link key, gets its classic
                        // side, listed and connecting over classic
                        if !me.service.settings().le_audio() {
                            let keys = bluecaster_core::paths::link_keys_file(&me.config_dir());
                            let keys = bt_host::link_keys::FileLinkKeyStore::open(&keys);
                            if bluecaster_core::devices::save_classic_sides(&me.config_dir(), keys.entries()) > 0 {
                                me.refresh_devices();
                            }
                        }
                        let (_, s) = me.snapshot();
                        let list = profiles::load(&me.config_dir());
                        let in_use = s
                            .stream
                            .profile
                            .as_deref()
                            .filter(|n| restart_after_save(&s.stream, n))
                            .and_then(|n| list.iter().find(|p| p.name == n));
                        if let Some(p) = in_use {
                            let le_on = me.service.settings().le_audio();
                            let mut next = le_switched_profile(&list, p, le_on);
                            // An LE Audio side saved (a scan found it) without
                            // its profile yet: one from the classic one
                            let le_saved = bluecaster_core::devices::load_le(&me.config_dir())
                                .iter()
                                .any(|d| d.address.eq_ignore_ascii_case(&p.device_address));
                            if le_on && next == p.name && !p.le_audio && le_saved {
                                let made = le_side_profile(&list, p);
                                next = made.name.clone();
                                let mut all = list.clone();
                                all.push(made);
                                let active = s.active_profile.clone();
                                me.save_profiles(&all, active)?;
                            }
                            tracing::info!(
                                "LE Audio {}: Bluetooth starts again, profile {} connects",
                                if le_on { "on" } else { "off" },
                                bluecaster_core::diag::quoted(&next)
                            );
                            me.service.restart_adapter();
                            me.publish_adapter_stopped();
                            me.restart_profile(&next)?;
                        }
                    }
                    // LE Audio on / off waits for the next scan or connection
                    // while the stack is in use otherwise (v1's deferred text)
                    let le_audio_deferred = le_toggled && me.service.le_audio_deferred();
                    Ok(json(&SettingsApplied { le_audio_deferred }))
                }))
                .await
            }
            Method::AdapterList => blocking(Box::new(|me| Ok(json(&me.refresh_adapters())))).await,
            Method::AdapterAssignWinusb { instance_id } => {
                blocking(Box::new(move |me| crate::driver::assign(me, &instance_id))).await
            }
            Method::AdapterRestoreDriver { instance_id } => {
                blocking(Box::new(move |me| crate::driver::restore(me, &instance_id))).await
            }
            Method::AdapterWindowsDevices { instance_id } => {
                // Only the adapters listed: the ID names a device node to walk
                if !self.with_snapshot(|s| s.adapter.adapters.iter().any(|a| a.instance_id == instance_id)) {
                    return Ok(json(&Vec::<WindowsPairedDevice>::new()));
                }
                blocking(Box::new(move |_| {
                    let list: Vec<WindowsPairedDevice> = win_sys::bt_paired::devices_through(&instance_id)
                        .iter()
                        .map(|d| WindowsPairedDevice {
                            address: bt_host::addr_str(&d.address),
                            name: d.name.clone(),
                            audio: d.is_a2dp_sink(),
                        })
                        .collect();
                    tracing::info!("adapter: Windows paired {} device(s) through it", list.len());
                    Ok(json(&list))
                }))
                .await
            }
            Method::AdapterRestart => {
                blocking(Box::new(|me| {
                    let again = me.service.restart_adapter();
                    me.publish_adapter_stopped();
                    // The device streamed before connects again on the new
                    // stack, with the same profile (issue #86)
                    if let Some(mut req) = again {
                        let settings = me.service.settings();
                        req.developer_codecs = settings.debug_mode;
                        let profile = me.starting_profile.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        me.begin_stream(req, profile);
                    }
                    Ok(Value::Null)
                }))
                .await
            }
            Method::AdapterProbe { instance_id } => {
                blocking(Box::new(move |me| {
                    let report = me
                        .service
                        .probe_adapter(Some(instance_id.clone()))
                        .map_err(|m| ErrorInfo { code: "adapter.probe_failed".into(), message: msg(&m) })?;
                    // The stack was stopped for it
                    let (_, s) = me.snapshot();
                    me.publish(Event::Adapter(AdapterState {
                        status: AdapterStatus::Stopped,
                        error: None,
                        ..s.adapter
                    }));
                    let summary = bluecaster_core::probe_report::summary_line(&report);
                    // The service's log names no device
                    if bluecaster_core::paths::service_mode() {
                        tracing::info!("chip probe: {summary}");
                    } else {
                        tracing::info!("chip probe of {instance_id}: {summary}");
                    }
                    let version = env!("CARGO_PKG_VERSION");
                    let path =
                        bluecaster_core::probe_report::save(&me.config_dir(), &report, Some(&instance_id), version)
                            .map_err(|e| {
                                tracing::warn!("chip probe: the report could not be saved: {e}");
                                err("adapter.probe_failed", "error.probe_save")
                            })?;
                    let report =
                        bluecaster_core::probe_report::to_json(&report, Some(&instance_id), version).to_string();
                    // Service mode: the user's copy (the core's folder is
                    // protected)
                    let saved = std::fs::read(&path).unwrap_or_default();
                    let path =
                        crate::handover::for_user(session_host::handover::Kind::ProbeReport, &path, &saved, || {
                            err("adapter.probe_failed", "error.probe_save")
                        })?;
                    Ok(json(&ProbeResult { path, summary, report }))
                }))
                .await
            }
            Method::AdapterCheckScript { instance_id, script } => Ok(json(&self.check_script(&instance_id, &script).1)),
            Method::AdapterRunScript { instance_id, script, allow_writes } => {
                blocking(Box::new(move |me| {
                    let (parsed, check) = me.check_script(&instance_id, &script);
                    let invalid = |key: &str, lines: &[String]| ErrorInfo {
                        code: "adapter.script_refused".into(),
                        message: Msg { key: key.into(), args: vec![lines.join("\n")] },
                    };
                    let parsed = match parsed {
                        Ok(s) if check.denied.is_empty() && (allow_writes || check.writes.is_empty()) => s,
                        Ok(_) if !check.denied.is_empty() => return Err(invalid("error.script_denied", &check.denied)),
                        Ok(_) => return Err(invalid("error.script_writes", &check.writes)),
                        Err(_) => return Err(invalid("error.script_invalid", &check.errors)),
                    };
                    let total = parsed.lines.len();
                    let on = if bluecaster_core::paths::service_mode() {
                        String::new()
                    } else {
                        format!(" on {instance_id}")
                    };
                    tracing::info!("chip script{on}: {total} lines, {} that may change the chip", check.writes.len());
                    let report = me
                        .service
                        .run_script(Some(instance_id.clone()), parsed)
                        .map_err(|m| ErrorInfo { code: "adapter.script_failed".into(), message: msg(&m) })?;
                    let (_, s) = me.snapshot();
                    me.publish(Event::Adapter(AdapterState {
                        status: AdapterStatus::Stopped,
                        error: None,
                        ..s.adapter
                    }));
                    let summary = match &report.error {
                        Some(e) => e.clone(),
                        None => format!("{} of {total} lines done", report.steps.len()),
                    };
                    let version = env!("CARGO_PKG_VERSION");
                    let path = bluecaster_core::probe_report::save_script(
                        &me.config_dir(),
                        &report,
                        &script,
                        Some(&instance_id),
                        version,
                    )
                    .map_err(|e| {
                        tracing::warn!("chip script: the report could not be saved: {e}");
                        err("adapter.script_failed", "error.probe_save")
                    })?;
                    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
                    let saved = std::fs::read(&path).unwrap_or_default();
                    let path =
                        crate::handover::for_user(session_host::handover::Kind::ScriptReport, &path, &saved, || {
                            err("adapter.script_failed", "error.probe_save")
                        })?;
                    Ok(json(&ScriptResult {
                        path,
                        summary,
                        lines: bt_host::script::report_text(&report),
                        other_events: report.other_events.iter().map(|e| hex(e)).collect(),
                    }))
                }))
                .await
            }
            Method::AdapterFirmwareFiles => {
                blocking(Box::new(|me| {
                    let list: Vec<FirmwareFile> = chips::scan_firmware_files(&paths::firmware_dir(&me.config_dir()))
                        .into_iter()
                        .map(|f| FirmwareFile {
                            stem: f.stem,
                            display_name: f.display_name,
                            pid: f.pid,
                            has_config: f.has_config,
                        })
                        .collect();
                    Ok(json(&list))
                }))
                .await
            }
            Method::AdapterDownloadFirmware { file } => {
                blocking(Box::new(move |me| {
                    if !bluecaster_core::firmware::available(&file) {
                        return Err(bad_request(
                            "firmware.no_download",
                            "adapter.download_firmware",
                            format!("no download for {file}"),
                        ));
                    }
                    if let Err(e) = bluecaster_core::firmware::download(&paths::firmware_dir(&me.config_dir()), &file) {
                        tracing::warn!("firmware download failed: {e}");
                        return Err(err("firmware.download_failed", "firmware.download_failed"));
                    }
                    // The files are there now: a stack started without them
                    // loads them anew
                    Ok(json(&me.refresh_adapters()))
                }))
                .await
            }
            Method::AudioDevices => {
                blocking(Box::new(|me| {
                    let devices = session_host::devices::render_devices();
                    if let Ok(d) = &devices {
                        me.watch_cables(d);
                    }
                    let list: Vec<AudioDevice> = devices
                        .unwrap_or_default()
                        .into_iter()
                        .map(|d| {
                            let mix = session_host::devices::mix_format(Some(&d.id));
                            AudioDevice {
                                mix_rate: mix.map(|m| m.rate),
                                mix_channels: mix.map(|m| m.channels),
                                mix_bits: mix.map(|m| m.bits),
                                id: d.id,
                                name: d.name,
                                short_name: d.short_name,
                                adapter_name: d.adapter_name,
                                is_default: d.is_default,
                                is_virtual: d.is_virtual,
                                is_display: d.is_display,
                                capture_name: d.capture_name,
                                capture_rate: d.capture_rate,
                            }
                        })
                        .collect();
                    Ok(json(&list))
                }))
                .await
            }
            Method::AudioInstallVbCable => {
                blocking(Box::new(|me| {
                    let found = crate::vb_cable::install()?;
                    // Taken into use as any VB-CABLE that comes (watch_cables)
                    if let Ok(d) = session_host::devices::render_devices() {
                        me.watch_cables(&d);
                    }
                    Ok(json(&found))
                }))
                .await
            }
            Method::AudioUninstallCable { hifi } => {
                blocking(Box::new(move |me| {
                    let gone = crate::vb_cable::uninstall(&me.config_dir(), hifi)?;
                    Ok(json(&gone))
                }))
                .await
            }
            Method::AudioApps => {
                blocking(Box::new(|_| {
                    let list: Vec<AudioApp> = session_host::endpoints::audio_apps()
                        .into_iter()
                        .map(|a| AudioApp { exe: a.exe, name: a.name })
                        .collect();
                    Ok(json(&list))
                }))
                .await
            }
            Method::AudioMixer => {
                blocking(Box::new(|_| {
                    let list: Vec<MixerApp> = session_host::volume::mixer_apps()
                        .into_iter()
                        .map(|a| MixerApp { key: a.key, name: a.name, volume: a.volume, muted: a.muted })
                        .collect();
                    Ok(json(&list))
                }))
                .await
            }
            Method::AudioMixerSet { app, volume, muted } => {
                blocking(Box::new(move |_| {
                    session_host::volume::set_mixer_app(&app, volume, muted);
                    Ok(Value::Null)
                }))
                .await
            }
            // Handled per client by the server
            Method::DiagSubscribe { .. } => Ok(Value::Null),
            Method::RfcommOpen { .. } | Method::RfcommSend { .. } | Method::RfcommClose { .. } => {
                Err(err("rfcomm.no_channel", "ipc.error.rfcomm_no_channel"))
            }
            Method::DiagCaptureStart => self.start_capture().map(Value::String),
            Method::DiagCaptureStop => Ok(json(&self.stop_capture())),
            Method::DiagInfo(params) => {
                let mask = params.and_then(|p| p.mask).unwrap_or(true);
                // Lists the USB adapters: off the runtime
                blocking(Box::new(move |me| Ok(json(&DiagInfo { text: crate::diag_text(me, mask) })))).await
            }
            Method::DiagWifiScan => {
                // Waits for the scan: off the runtime
                blocking(Box::new(|_| {
                    use win_sys::wlan::{ScanError, scan};
                    match scan(std::time::Duration::from_secs(4)) {
                        Ok(aps) => {
                            let list: Vec<WifiAccessPoint> = aps
                                .into_iter()
                                .map(|a| WifiAccessPoint { ssid: a.ssid, channel: a.channel, mhz: a.mhz, rssi: a.rssi })
                                .collect();
                            Ok(json(&list))
                        }
                        Err(ScanError::NoWifi) => Err(err("wifi.none", "error.wifi_none")),
                        Err(ScanError::LocationDenied) => Err(err("wifi.location", "error.wifi_location")),
                        Err(ScanError::RadioOff) => Err(err("wifi.off", "error.wifi_off")),
                        Err(ScanError::Failed(code)) => {
                            tracing::warn!("Wi-Fi scan failed (0x{code:08X})");
                            Err(err("wifi.failed", "error.wifi_failed"))
                        }
                    }
                }))
                .await
            }
            Method::DiagFlow => Ok(json(&crate::flow_view(self))),
            Method::DiagRecentLogs { level } => {
                let mut lines = crate::logging::hub().recent();
                lines.retain(|l| l.level <= level);
                Ok(json(&lines))
            }
            Method::DiagCrashesSeen => {
                self.crashes_seen();
                Ok(Value::Null)
            }
            Method::DiagMark { note } => Ok(json(&crate::logging::mark(&note))),
            Method::DiagSetVerbose { level } => {
                let v = crate::logging::set_verbose(level);
                self.publish(Event::Verbose(v));
                Ok(json(&v))
            }
            Method::DiagBundle(params) => {
                let p = params.unwrap_or_default();
                let (mask, hci) = (p.mask.unwrap_or(true), p.hci.unwrap_or(true));
                blocking(Box::new(move |me| {
                    crate::bundle::create(me, mask, hci).map(|b| json(&b)).map_err(|e| {
                        tracing::error!("diagnostic bundle: {e}");
                        ErrorInfo {
                            code: "diag.bundle_failed".into(),
                            message: Msg { key: "error.bundle_failed".into(), args: vec![e] },
                        }
                    })
                }))
                .await
            }
            Method::ReceiverDiscover => {
                blocking(Box::new(|_| {
                    let found: Vec<ipc::ReceiverInfo> =
                        receiver::discover(None, receiver::DEFAULT_PORT, receiver::DISCOVER_TIMEOUT)
                            .iter()
                            .filter_map(convert)
                            .collect();
                    Ok(json(&found))
                }))
                .await
            }
            Method::ReceiverConnect { target } => {
                let target = if target.trim().is_empty() { receiver::AUTO.to_string() } else { target.trim().into() };
                if receiver::split_target(&target).is_none() {
                    return Err(bad_request("receiver.invalid_target", "receiver.connect", format!("\"{target}\"")));
                }
                blocking(Box::new(move |me| {
                    // Ends a previous connection first (bounded by its
                    // connect and discovery timeouts)
                    me.receiver.start(&target);
                    let _tx = me.service.settings_tx();
                    let mut s = me.service.settings();
                    if s.remote_sink != target {
                        s.remote_sink = target;
                        me.save_settings(s)?;
                    }
                    me.publish_receiver();
                    Ok(json(&me.receiver_state()))
                }))
                .await
            }
            Method::ReceiverDisconnect => {
                blocking(Box::new(|me| {
                    me.receiver.stop();
                    me.publish_receiver();
                    Ok(Value::Null)
                }))
                .await
            }
            Method::ReceiverConfigure { mtu, buffer_ms } => {
                let (mtu, buffer_ms) = (mtu.unwrap_or(0), buffer_ms.unwrap_or(0));
                if mtu > u16::MAX as u32 || (mtu != 0 && mtu < 48) || buffer_ms > 10_000 {
                    return Err(bad_request(
                        "receiver.bad_settings",
                        "receiver.configure",
                        format!("mtu {mtu}, buffer {buffer_ms} ms"),
                    ));
                }
                if !self.receiver.configure(mtu, buffer_ms) {
                    return Err(err("receiver.not_connected", "receiver.off"));
                }
                Ok(Value::Null)
            }
            Method::ReceiverStatus => Ok(json(&self.receiver_state())),
            Method::QcyState => Ok(crate::qcy_live::current()),
            Method::QcyRefresh | Method::QcyReconnect => {
                blocking(Box::new(move |me| {
                    let v = crate::qcy_live::refresh(&me.service);
                    me.publish(Event::Qcy(v.clone()));
                    Ok(v)
                }))
                .await
            }
            Method::QcySet { kind, enabled, value } => {
                // Exactly kind + the one value given (allowed_change refuses
                // a toggle with a level, a level with on/off, or both)
                let mut change = serde_json::json!({ "kind": kind });
                if let Some(e) = enabled {
                    change["enabled"] = serde_json::json!(e);
                }
                if let Some(v) = value {
                    change["value"] = serde_json::json!(v);
                }
                if crate::qcy_live::allowed_change(&change).is_none() {
                    return Err(err("qcy.read_only", "fones.error.read_only"));
                }
                blocking(Box::new(move |me| {
                    let v = crate::qcy_live::set(&me.service, &change)
                        .map_err(|()| err("qcy.read_only", "fones.error.read_only"))?;
                    me.publish(Event::Qcy(v.clone()));
                    Ok(v)
                }))
                .await
            }
            Method::MicTestRecord => {
                blocking(Box::new(|me| {
                    let restart = me.service.mic_test_record().map_err(|code| {
                        let key = if code == "mic_test.le_audio" {
                            "error.mic_test_le_audio"
                        } else {
                            "error.mic_test_not_streaming"
                        };
                        err(code, key)
                    })?;
                    if restart {
                        // A stack with voice: the profile in use starts again
                        let (_, s) = me.snapshot();
                        let Some(name) = s.stream.profile else {
                            me.service.mic_test_end();
                            return Err(err("mic_test.not_streaming", "error.mic_test_not_streaming"));
                        };
                        tracing::info!("mic test: {} starts again with voice", bluecaster_core::diag::quoted(&name));
                        if let Err(e) = me.restart_profile(&name) {
                            me.service.mic_test_end();
                            return Err(e);
                        }
                    }
                    Ok(json(&me.service.mic_test_status()))
                }))
                .await
            }
            Method::MicTestStop => {
                self.service.mic_test_stop();
                Ok(json(&self.service.mic_test_status()))
            }
            Method::MicTestPlay => {
                self.service.mic_test_play();
                Ok(json(&self.service.mic_test_status()))
            }
            Method::MicTestStatus => Ok(json(&self.service.mic_test_status())),
            Method::MicTestEnd => {
                self.service.mic_test_end();
                Ok(Value::Null)
            }
            Method::HidCode => {
                developer(self)?;
                Ok(json(&ipc::HidCode { code: self.service.hid_code() }))
            }
            Method::HidEnable { code, other_input_at_hand, risk_understood } => {
                developer(self)?;
                blocking(Box::new(move |me| {
                    me.service.hid_confirm(&code, other_input_at_hand, risk_understood).map_err(|m| hid_err(&m))?;
                    me.set_hid_host(true)?;
                    // The path to Windows tested at once: on failure the
                    // feature goes off again, as hid.disable turns it off
                    if let Err(m) = me.service.hid_self_test_enable() {
                        let _ = me.set_hid_host(false);
                        return Err(hid_err(&m));
                    }
                    Ok(Value::Null)
                }))
                .await
            }
            Method::HidDisable => blocking(Box::new(|me| me.set_hid_host(false))).await,
            Method::HidStatus => Ok(json(&ipc::HidStatus {
                devices: to_ipc_hid_devices(self.service.hid_devices()),
                counts: self.service.hid_counts().map(|c| ipc::HidCounts {
                    reports: c.reports,
                    undecoded: c.undecoded,
                    events: c.events,
                    injected: c.injected,
                    blocked: c.blocked,
                    refused: c.refused,
                }),
            })),
            Method::HidAdd { address, name, le } => {
                developer(self)?;
                blocking(Box::new(move |me| {
                    me.service.hid_add(&address, &name, le).map_err(|m| hid_err(&m))?;
                    Ok(Value::Null)
                }))
                .await
            }
            Method::HidConnect { address } => {
                developer(self)?;
                blocking(Box::new(move |me| {
                    me.service.hid_connect(&address).map_err(|m| hid_err(&m))?;
                    Ok(Value::Null)
                }))
                .await
            }
            Method::HidRemove { address } => {
                blocking(Box::new(move |me| {
                    me.service.hid_remove(&address);
                    Ok(Value::Null)
                }))
                .await
            }
            Method::HidCheckCancel { address } => {
                blocking(Box::new(move |me| {
                    me.service.hid_check_cancel(&address);
                    Ok(Value::Null)
                }))
                .await
            }
            Method::HidCheckDismiss => {
                self.service.hid_check_dismiss();
                Ok(Value::Null)
            }
            Method::AppCheckUpdate => blocking(Box::new(|me| me.check_update().map(|u| json(&u)))).await,
            Method::AppHiddenToTray => {
                self.hidden_to_tray();
                Ok(Value::Null)
            }
            Method::AppQuit => {
                self.quit();
                Ok(Value::Null)
            }
            // Service mode: the core stays (crate::headless)
            Method::AppClose => {
                if paths::service_mode() {
                    crate::headless::close_for_user(self);
                } else {
                    self.quit();
                }
                Ok(Value::Null)
            }
            Method::AppRunAsAdmin { call } => {
                let service = bluecaster_core::paths::service_mode();
                blocking(Box::new(move |me| crate::rules::run_as_admin(&call, &**me, service))).await
            }
            Method::AppQuitForUpdate => match self.service.state() {
                CoreStreamState::Connecting | CoreStreamState::Streaming | CoreStreamState::Reconnecting => {
                    Err(err("app.streaming", "ui.update.streaming"))
                }
                CoreStreamState::Idle | CoreStreamState::Error => {
                    tracing::info!("exiting for an update");
                    self.quit();
                    Ok(Value::Null)
                }
            },
        }
    }

    /// Asks GitHub for a newer release (blocking) and publishes the result;
    /// Err (update.failed) when it could not ask, and the state says so too
    pub fn check_update(&self) -> Result<UpdateState, ErrorInfo> {
        use bluecaster_core::update;
        let (_, s) = self.snapshot();
        self.publish(Event::Update(UpdateState { checking: true, ..s.update.clone() }));
        let channel = self.service.settings().update_channel();
        let current_version = update::current_version().to_string();
        match update::check(update::current_version(), channel) {
            Ok(r) => {
                let available = r.map(|r| ipc::UpdateInfo {
                    manifest_url: update::manifest_url(&r.tag),
                    version: r.version,
                    url: r.url,
                    prerelease: r.prerelease,
                    notes: r.notes,
                });
                let u = UpdateState { checking: false, available, current_version, error: None };
                self.publish(Event::Update(u.clone()));
                Ok(u)
            }
            Err(e) => {
                tracing::warn!("update check failed: {e}");
                let error = Msg::new("update.error");
                self.publish(Event::Update(UpdateState {
                    checking: false,
                    available: s.update.available.clone(),
                    current_version,
                    error: Some(error.clone()),
                }));
                Err(ErrorInfo { code: "update.failed".into(), message: error })
            }
        }
    }

    /// The automatic check: at most once a day (recorded before asking, as v1)
    pub fn check_update_if_due(self: &Arc<Self>) {
        use bluecaster_core::update;
        let tx = self.service.settings_tx();
        let mut s = self.service.settings();
        let today = update::today();
        if !update::automatic_check_due(&s, &today) {
            return;
        }
        s.last_update_check = today;
        let _ = s.save(&self.config_dir());
        self.service.set_settings(s);
        drop(tx);
        let me = self.clone();
        std::thread::spawn(move || {
            // A failure is in the update state
            if let Ok(u) = me.check_update() {
                me.announce_update(&u);
            }
        });
    }

    pub fn link_subscribed(&self) -> bool {
        self.link_subscribers.load(Ordering::Acquire) > 0
    }
}

/// A capture's file as the UI shows it: its path, or in service mode its
/// name only (the folder is protected; the capture reaches the user in a
/// diagnostic bundle, crate::handover)
fn capture_shown(path: &Path) -> String {
    if paths::service_mode() {
        return path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    path.display().to_string()
}

/// yyyyMMdd_HHmmss of the local time, for file names (as v1)
fn local_stamp() -> String {
    let t = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!("{:04}{:02}{:02}_{:02}{:02}{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute(), t.second())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(source: &str) -> CustomStream {
        CustomStream {
            device: "aa:bb:cc:dd:ee:ff".into(),
            codec: "ldac".into(),
            quality: "sq".into(),
            source: source.into(),
            ..CustomStream::default()
        }
    }

    fn rtl(id: &str, present: Option<bool>, invalid: Option<&str>) -> AdapterInfo {
        AdapterInfo {
            instance_id: id.into(),
            vid: 0x0BDA,
            pid: 0x8771,
            display_name: "RTL8761BU".into(),
            device_name: String::new(),
            vendor: "Realtek".into(),
            driver: DriverKind::Winusb,
            driver_service: "WinUSB".into(),
            realtek_chip: Some("RTL8761BU".into()),
            firmware_present: present,
            firmware_files: vec!["rtl8761bu_fw.bin".into(), "rtl8761bu_config.bin".into()],
            firmware_invalid: invalid.map(str::to_owned),
            firmware_download: true,
            firmware_from_driver: false,
            firmware_driver: false,
            vendor_note: None,
            restart_needed: false,
            le_audio: None,
            address: None,
            bt_version: None,
            windows_name: None,
        }
    }

    #[test]
    fn a_broadcast_is_mono_or_split_not_both() {
        let b = |mono, split| BroadcastStream {
            name: "Speakers".into(),
            quality: "hq".into(),
            mono,
            split,
            source: "tone".into(),
            ..BroadcastStream::default()
        };
        let e = broadcast_request(&b(Some(true), Some(true)), 0).expect_err("refused");
        assert_eq!(e.code, "stream.bad_channels");
        assert_eq!(e.message.key, "ipc.error.bad_request");
        assert_eq!(e.message.args, ["stream.start_broadcast", "mono and split cannot be used together"]);
        for (mono, split) in [(Some(true), Some(false)), (Some(false), Some(true)), (None, None), (Some(true), None)] {
            let req = broadcast_request(&b(mono, split), 0).expect("accepted");
            let le = req.le.and_then(|l| l.broadcast).expect("a broadcast");
            let layout =
                if split == Some(true) { bt_host::BroadcastLayout::Split } else { bt_host::BroadcastLayout::Stereo };
            assert_eq!((le.mono, le.layout), (mono.unwrap_or(false), layout));
        }
    }

    #[test]
    fn a_new_profile_gets_the_two_channel_cable_for_its_microphone() {
        let dev = |id: &str, name: &str, virt: bool| audio_capture::DeviceInfo {
            id: id.into(),
            name: name.into(),
            short_name: None,
            adapter_name: None,
            is_default: false,
            is_virtual: virt,
            is_display: false,
            capture_name: None,
            capture_rate: None,
        };
        let list = [
            dev("spk", "Speakers (Realtek(R) Audio)", false),
            dev("c16", "CABLE In 16ch (VB-Audio Virtual Cable)", true),
            dev("vm", "Voicemeeter Input (VB-Audio Voicemeeter VAIO)", true),
            dev("cable", "CABLE Input (VB-Audio Virtual Cable)", true),
        ];
        assert_eq!(default_cable(&list).map(|d| d.id.as_str()), Some("cable"));
        assert!(default_cable(&list[..3]).is_none());
        assert!(default_cable(&list[..1]).is_none());
    }

    #[test]
    fn a_chosen_realtek_chip_is_not_checked_on_other_vendors_adapters() {
        // RTL8761BU chosen by hand, a Realtek and a MediaTek adapter (issue #86)
        let s = CoreSettings { bt_chip_pid: 0x8771, ..CoreSettings::default() };
        assert_eq!(adapter_chip(ChipVendor::Realtek, Some(0x8771), &s), (0x8771, ""));
        assert_eq!(adapter_chip(ChipVendor::MediaTek, None, &s), (0, ""));
        assert_eq!(adapter_chip(ChipVendor::Intel, None, &s), (0, ""));
        // A chip known only by its files, on an adapter not recognised as another vendor's
        let stem = CoreSettings { bt_chip_fw_stem: "rtl8723bu".into(), ..s.clone() };
        assert_eq!(adapter_chip(ChipVendor::Unknown, None, &stem).1, "rtl8723bu");
        assert_eq!(adapter_chip(ChipVendor::Qualcomm, None, &stem), (0, ""));
    }

    /// This PC's adapters as the Adapters screen lists them, with nothing
    /// remembered yet (a fresh config folder; read-only otherwise): cargo
    /// test -p bluecaster-core-exe list_adapter_infos -- --ignored --nocapture
    #[test]
    #[ignore = "reads this PC's USB devices and Bluetooth radios"]
    fn list_adapter_infos() {
        let dir = std::env::temp_dir().join(format!("bluecaster-adapter-infos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (list, hung) = adapter_infos(&dir, &CoreSettings::default());
        println!("hung adapter: {hung}");
        for a in list {
            println!(
                "{} {:04X}:{:04X} {:?} ({}) address {:?} version {:?} name {:?}",
                a.instance_id, a.vid, a.pid, a.driver, a.driver_service, a.address, a.bt_version, a.windows_name
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn firmware_fixed_and_invalid_files_are_noticed() {
        let missing = rtl("a", Some(false), None);
        let page = rtl("a", Some(false), Some("rtl8761bu_fw.bin"));
        let ok = rtl("a", Some(true), None);
        // Placed, or a web page replaced by the file: the stack restarts
        assert!(firmware_change(std::slice::from_ref(&missing), std::slice::from_ref(&ok)).fixed);
        assert!(firmware_change(std::slice::from_ref(&page), std::slice::from_ref(&ok)).fixed);
        // Present all along, another adapter, or a newly plugged one
        assert!(!firmware_change(std::slice::from_ref(&ok), std::slice::from_ref(&ok)).fixed);
        assert!(!firmware_change(&[rtl("b", Some(false), None)], std::slice::from_ref(&ok)).fixed);
        assert!(!firmware_change(&[], std::slice::from_ref(&ok)).fixed);
        // An invalid file is told once, not at every refresh
        let c = firmware_change(std::slice::from_ref(&missing), std::slice::from_ref(&page));
        assert_eq!(c.invalid, [("RTL8761BU".to_string(), "rtl8761bu_fw.bin".to_string())]);
        assert!(!c.fixed);
        assert!(firmware_change(std::slice::from_ref(&page), std::slice::from_ref(&page)).invalid.is_empty());
        assert_eq!(firmware_change(&[], std::slice::from_ref(&page)).invalid.len(), 1);
    }

    #[test]
    fn the_tray_tooltip_names_the_device_and_the_codec() {
        let ldac = tray_codec_line("LDAC", 990, 96000, 24, true);
        assert_eq!(ldac, "LDAC 990 kbps | 96 kHz | 24-bit | ABR");
        assert_eq!(tray_codec_line("SBC", 328, 44100, 16, false), "SBC 328 kbps | 44.1 kHz | 16-bit");
        assert_eq!(tray_codec_line("LC3", 320, 48000, 0, false), "LC3 320 kbps | 48 kHz");
        assert_eq!(
            tray_tooltip("BlueCaster - Connected", Some("WH-1000XM4"), &[ldac.clone(), "Battery 80%".into()]),
            "BlueCaster - Connected (WH-1000XM4)\nLDAC 990 kbps | 96 kHz | 24-bit | ABR\nBattery 80%"
        );
        assert_eq!(
            tray_tooltip("BlueCaster - Connected", None, &[ldac]),
            "BlueCaster - Connected\nLDAC 990 kbps | 96 kHz | 24-bit | ABR"
        );
        assert_eq!(tray_tooltip("BlueCaster - Connected", Some(""), &[]), "BlueCaster - Connected");
        assert_eq!(
            tray_tooltip("BlueCaster - Connected", Some("AA:BB:CC:DD:EE:FF"), &[]),
            "BlueCaster - Connected (AA:BB:CC:DD:EE:FF)"
        );
        // AAC's variable bit rate, as the UI calls it
        assert_eq!(tray_codec_line("AAC", 320, 48000, 16, true), "AAC 320 kbps | 48 kHz | 16-bit | VBR");
    }

    /// A stream as the core reports it once the codec is set up
    fn streaming_with(codec: &str, kbps: u32, rate: u32, bits: u32, abr: bool, battery: Option<u8>) -> StreamStatus {
        StreamStatus {
            state: StreamState::Streaming,
            message: Msg::new("status.connected"),
            info: Some(ipc::StreamInfo {
                codec: codec.into(),
                bitrate_kbps: kbps,
                sample_rate: rate,
                channels: 2,
                bits,
                source_sample_rate: rate,
                source_channels: 2,
                source_bits: 32,
                requested_codec: None,
                requested_in_use: false,
                abr,
                bitrate_capped: false,
                hi_res_set: false,
                low_latency: false,
                source: None,
                le: None,
                details: None,
            }),
            battery,
            ..StreamStatus::default()
        }
    }

    #[test]
    fn the_connected_balloon_shows_the_device_codec_and_battery() {
        let ldac = streaming_with("LDAC", 990, 96000, 24, true, Some(80));
        let b = connected_balloon("en", Some("WH-1000XM4"), &ldac).unwrap();
        assert_eq!(b.title, "WH-1000XM4 connected");
        assert_eq!(b.text, "Battery 80%\nLDAC\n990 kbps | 96 kHz | 24-bit | ABR");
        assert_eq!(b.url, None);

        // Battery not known yet; an address when the device has no name
        let aptx = streaming_with("aptX Adaptive", 420, 48000, 24, true, None);
        let b = connected_balloon("en", Some("C0:11:22:33:44:55"), &aptx).unwrap();
        assert_eq!(b.title, "C0:11:22:33:44:55 connected");
        assert_eq!(b.text, "aptX Adaptive\n420 kbps | 48 kHz | 24-bit | ABR");

        // Another codec than the one asked for: the reason, as the UI's note
        let mut aac = streaming_with("AAC", 320, 44100, 16, true, Some(55));
        aac.info.as_mut().unwrap().requested_codec = Some("LDAC".into());
        let b = connected_balloon("en", None, &aac).unwrap();
        assert_eq!(b.title, "Connected");
        assert_eq!(b.text, "Battery 55%\nAAC\n320 kbps | 44.1 kHz | 16-bit | VBR\n(LDAC is unsupported)");
        aac.info.as_mut().unwrap().requested_in_use = true;
        assert!(connected_balloon("en", None, &aac).unwrap().text.contains("(LDAC is in use)"));

        // Translated; nothing before the codec is set up
        let b = connected_balloon("ja", Some("WH-1000XM4"), &ldac).unwrap();
        assert_eq!(b.text, "バッテリー 80%\nLDAC\n990 kbps | 96 kHz | 24-bit | ABR");
        assert!(connected_balloon("en", Some("WH"), &StreamStatus::default()).is_none());
    }

    #[test]
    fn a_connection_is_announced_once_with_its_negotiated_codec() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-announce-{}", std::process::id()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let shown = Arc::new(Mutex::new(Vec::<BalloonRequest>::new()));
        let sink = shown.clone();
        let state = CoreState::new(
            Arc::new(Service::new(options, tx)),
            Box::new(|_| {}),
            Box::new(|| {}),
            Box::new(move |b| sink.lock().unwrap().push(b)),
        );
        let event = |st| state.on_service_event(ServiceEvent::State(st, bluecaster_core::Msg::new("status.ready")));
        let info = |kbps| {
            state.on_service_event(ServiceEvent::StreamInfo(bluecaster_core::StreamInfo {
                codec: "LDAC".into(),
                bitrate_kbps: kbps,
                sample_rate: 96000,
                channels: 2,
                bits: 24,
                source_sample_rate: 96000,
                source_channels: 2,
                source_bits: 32,
                requested_codec: None,
                requested_in_use: false,
                abr: false,
                bitrate_capped: false,
                hi_res_set: false,
                low_latency: false,
                source: bluecaster_core::SourceInfo { kind: "tone", name: String::new(), waiting: false },
                details: None,
            }))
        };
        let texts = || shown.lock().unwrap().iter().map(|b| b.text.clone()).collect::<Vec<_>>();
        // A first connection: Streaming, the negotiated codec, then a moment
        // for the battery
        event(CoreStreamState::Connecting);
        event(CoreStreamState::Streaming);
        assert!(texts().is_empty());
        info(660);
        assert!(texts().is_empty());
        state.on_service_event(ServiceEvent::Battery(80));
        assert_eq!(texts().len(), 1);
        assert!(texts()[0].ends_with(" 80%\nLDAC\n660 kbps | 96 kHz | 24-bit"));
        // A lost link reconnecting (its info again, then Streaming): quiet
        event(CoreStreamState::Reconnecting);
        info(660);
        event(CoreStreamState::Streaming);
        assert_eq!(texts().len(), 1);
        // The headphones off and back on: a connection again
        event(CoreStreamState::Idle);
        event(CoreStreamState::Connecting);
        info(990);
        assert_eq!(texts().len(), 1);
        event(CoreStreamState::Streaming);
        assert_eq!(texts().len(), 1);
        // The battery refused: nothing to wait for
        state.on_service_event(ServiceEvent::BatteryRefused);
        assert_eq!(texts().len(), 2);
        assert_eq!(texts()[1], "LDAC\n990 kbps | 96 kHz | 24-bit");
        // Started again for a codec chosen (restart_profile): quiet, and
        // the next connection is announced again (issue #86)
        state.quiet_connect.store(true, Ordering::Release);
        event(CoreStreamState::Idle);
        event(CoreStreamState::Connecting);
        info(660);
        event(CoreStreamState::Streaming);
        assert_eq!(texts().len(), 2);
        event(CoreStreamState::Idle);
        event(CoreStreamState::Connecting);
        event(CoreStreamState::Streaming);
        info(660);
        assert_eq!(texts().len(), 2);
        // No report: without the battery once the wait is over
        *state.battery_deadline.lock().unwrap() = Some(Instant::now());
        state.announce_connected();
        assert_eq!(texts().len(), 3);
        assert_eq!(texts()[2], "LDAC\n660 kbps | 96 kHz | 24-bit");
        // The setting off: nothing
        let mut s = state.service.settings();
        s.notify_connected = false;
        state.service.set_settings(s);
        event(CoreStreamState::Idle);
        event(CoreStreamState::Connecting);
        event(CoreStreamState::Streaming);
        info(990);
        assert_eq!(texts().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tray_menu_names_devices_not_profiles() {
        let saved = |name: &str, le_audio| Device {
            address: "aa:bb:cc:dd:ee:ff".into(),
            name: name.into(),
            saved: true,
            audio: true,
            rssi: None,
            codecs: Vec::new(),
            sbc: None,
            codec_caps: Vec::new(),
            codecs_on_request: Vec::new(),
            le_audio,
            le_audio_set: false,
            hid_le: None,
            set_addresses: Vec::new(),
        };
        let core = CoreProfile {
            name: "EarFun Air Pro 4+ | aptX Adaptive".into(),
            device_address: "AA:BB:CC:DD:EE:FF".into(),
            device_name: "EarFun".into(),
            ..CoreProfile::default()
        };
        let p = to_ipc_profile(&core, 679);
        // The saved device's name (renamed since), else the profile's, else the address
        assert_eq!(
            device_name(&[saved("Air Pro 4+", true), saved("Air Pro 4+ (renamed)", false)], &p),
            "Air Pro 4+ (renamed)"
        );
        assert_eq!(device_name(&[], &p), "EarFun");
        let unnamed = to_ipc_profile(&CoreProfile { device_name: String::new(), ..core }, 679);
        assert_eq!(device_name(&[], &unnamed), "AA:BB:CC:DD:EE:FF");
        assert_eq!(tray_label("Air Pro 4+", true, "LE Audio"), "Air Pro 4+ (LE Audio)");
        assert_eq!(tray_label("Air Pro 4+", false, "LE Audio"), "Air Pro 4+");
        // One per device (issue #86)
        let p = |name: &str, addr: &str, le_audio| Profile {
            name: name.into(),
            device_address: addr.into(),
            le_audio,
            ..to_ipc_profile(&CoreProfile::default(), 0)
        };
        let list = [p("Buds", "AA:01", false), p("Buds LE Audio", "aa:01", true), p("XM4", "AA:02", false)];
        let names = |le| tray_profiles(&list, le).iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(true), ["Buds LE Audio", "XM4"]);
        assert_eq!(names(false), ["Buds", "XM4"]);
    }

    #[test]
    fn saving_the_profile_in_use_restarts_it() {
        let status = |state, profile: Option<&str>| StreamStatus {
            state,
            profile: profile.map(str::to_owned),
            ..StreamStatus::default()
        };
        for state in [StreamState::Connecting, StreamState::Streaming, StreamState::Reconnecting] {
            assert!(restart_after_save(&status(state, Some("WH LDAC")), "WH LDAC"));
            assert!(!restart_after_save(&status(state, Some("WH LDAC")), "Buds AAC"));
            // A custom stream (receiver test, test tone) has no profile
            assert!(!restart_after_save(&status(state, None), "WH LDAC"));
        }
        for state in [StreamState::Idle, StreamState::Error] {
            assert!(!restart_after_save(&status(state, Some("WH LDAC")), "WH LDAC"));
        }
    }

    #[test]
    fn headset_mode_switches_in_place_for_classic_and_restarts_le_audio() {
        let before = CoreProfile { name: "WH".into(), ..CoreProfile::default() };
        let on = CoreProfile {
            headset_mic: "on".into(),
            mic_device_id: "{cable}".into(),
            mic_device_name: "CABLE Input".into(),
            ..before.clone()
        };
        let stream = |state| StreamStatus { state, profile: Some("WH".into()), ..StreamStatus::default() };
        let streaming = stream(StreamState::Streaming);
        assert_eq!(headset_change(&streaming, Some(&before), &on), Some(HeadsetChange::InPlace));
        assert_eq!(headset_change(&streaming, Some(&on), &before), Some(HeadsetChange::InPlace));
        // Another output while on: the classic stream takes it in place too
        let other = CoreProfile { mic_device_id: "{cable 2}".into(), ..on.clone() };
        assert_eq!(headset_change(&streaming, Some(&on), &other), Some(HeadsetChange::InPlace));
        // Nothing the stream uses: an output chosen while headset mode is off
        let output_only = CoreProfile { headset_mic: "off".into(), ..on.clone() };
        assert_eq!(headset_change(&streaming, Some(&before), &output_only), Some(HeadsetChange::Unused));
        // Auto: on to auto and auto to off switch in place as well
        let auto = CoreProfile { headset_mic: "auto".into(), ..on.clone() };
        assert_eq!(headset_change(&streaming, Some(&on), &auto), Some(HeadsetChange::InPlace));
        assert_eq!(headset_change(&streaming, Some(&auto), &output_only), Some(HeadsetChange::InPlace));
        // LE Audio sets its streams up again, as does a classic one connecting
        let le = CoreProfile { le_audio: true, ..before.clone() };
        let le_on = CoreProfile { le_audio: true, ..on.clone() };
        assert_eq!(headset_change(&streaming, Some(&le), &le_on), Some(HeadsetChange::Restart));
        assert_eq!(headset_change(&stream(StreamState::Connecting), Some(&before), &on), Some(HeadsetChange::Restart));
        // Saved with something else, or nothing changed
        let codec = CoreProfile { codec: "aac".into(), ..on.clone() };
        assert_eq!(headset_change(&streaming, Some(&before), &codec), None);
        assert_eq!(headset_change(&streaming, Some(&before), &before), None);
        assert_eq!(headset_change(&streaming, None, &on), None);
    }

    #[test]
    fn le_audio_requests_carry_headset_mode() {
        let le_profile = CoreProfile {
            name: "Buds".into(),
            device_address: "00:A2:D0:00:00:12".into(),
            le_audio: true,
            ..CoreProfile::default()
        };
        let mut req = le_profile.to_request(1000).unwrap();
        assert_eq!(req.headset_mic, HeadsetMic::Off);
        set_headset_mic(&mut req, HeadsetMic::On, "{cable}");
        let le = req.le.as_ref().unwrap();
        assert_eq!((req.headset_mic, le.headset_mic), (HeadsetMic::On, HeadsetMic::On));
        assert_eq!(le.mic_output.as_deref(), Some("{cable}"));
        set_headset_mic(&mut req, HeadsetMic::Auto, "{cable}");
        let le = req.le.as_ref().unwrap();
        assert_eq!((req.headset_mic, le.headset_mic), (HeadsetMic::Auto, HeadsetMic::Auto));
        assert_eq!(le.mic_output.as_deref(), Some("{cable}"));
        set_headset_mic(&mut req, HeadsetMic::Off, "{cable}");
        let le = req.le.as_ref().unwrap();
        assert_eq!((req.headset_mic, le.headset_mic), (HeadsetMic::Off, HeadsetMic::Off));
        assert!(le.mic_output.is_none());
        // Classic: the request only
        let mut classic = CoreProfile { le_audio: false, ..le_profile.clone() }.to_request(1000).unwrap();
        set_headset_mic(&mut classic, HeadsetMic::Auto, "{cable}");
        assert!(classic.le.is_none() && classic.headset_mic == HeadsetMic::Auto);
        assert_eq!(classic.mic_output.as_deref(), Some("{cable}"));
        // In force: an output chosen and not muted
        let auto = CoreProfile { headset_mic: "auto".into(), mic_device_id: "{cable}".into(), ..le_profile };
        assert_eq!(headset_mic_in_force(&auto, false), HeadsetMic::Auto);
        let muted = CoreProfile { mic_muted: true, ..auto.clone() };
        assert_eq!(headset_mic_in_force(&muted, true), HeadsetMic::Off);
        // On only in developer mode; Auto otherwise, saved as it was
        let on = CoreProfile { headset_mic: "on".into(), ..auto.clone() };
        assert_eq!(headset_mic_in_force(&on, true), HeadsetMic::On);
        assert_eq!(headset_mic_in_force(&on, false), HeadsetMic::Auto);
        assert_eq!(on.headset_mic, "on");
        assert_eq!(headset_mic_in_force(&CoreProfile { mic_muted: true, ..on }, false), HeadsetMic::Off);
        let no_output = CoreProfile { mic_device_id: String::new(), ..auto };
        assert_eq!(headset_mic_in_force(&no_output, true), HeadsetMic::Off);
    }

    #[test]
    fn mic_processing_round_trips_and_is_clamped() {
        let core =
            CoreProfile { name: "WH".into(), device_address: "00:11:22:33:44:55".into(), ..CoreProfile::default() };
        let mut p = to_ipc_profile(&core, 679);
        assert_eq!(p.mic_processing, ipc::MicProcessing::default());
        p.mic_processing = ipc::MicProcessing { enabled: true, gain_db: 40, suppression: 120, gate_db: -127 };
        let merged = merge_profile(Some(&core), &p);
        assert_eq!(
            merged.mic_processing,
            ipc::MicProcessing { enabled: true, gain_db: 24, suppression: 100, gate_db: -80 }
        );
        let unchanged = merge_profile(Some(&core), &to_ipc_profile(&core, 679));
        assert!(unchanged.only_mic_processing_differs(&merged));
        assert_eq!(to_ipc_profile(&merged, 679).mic_processing, merged.mic_processing);
        // An older UI without the field leaves it off
        let old: Profile = serde_json::from_value(serde_json::json!({
            "name": "x", "device_address": "", "device_name": "", "codec": "auto", "quality": "hq",
            "abr": false, "codec_fallback": true, "sample_rate": 0, "bit_depth": 0, "capture_mode": "loopback",
            "audio_device_id": "", "audio_device_name": "", "auto_switch_device": true, "app_exe": "",
            "app_name": "", "volume_sync": "auto", "volume_device_id": "", "volume_device_name": "",
            "max_media_payload": 679
        }))
        .unwrap();
        assert_eq!(old.mic_processing, ipc::MicProcessing::default());
    }

    #[test]
    fn the_microphones_cable_is_the_default_input_while_connected() {
        let core = CoreProfile {
            name: "WH".into(),
            device_address: "00:11:22:33:44:55".into(),
            headset_mic: "auto".into(),
            mic_device_id: "{cable}".into(),
            ..CoreProfile::default()
        };
        // On by default, also for a profile saved before the setting
        assert!(core.mic_default_input);
        let p = to_ipc_profile(&core, 679);
        assert!(p.mic_default_input);
        let saved: Profile = serde_json::from_value(serde_json::json!({
            "name": "x", "device_address": "", "device_name": "", "codec": "auto", "quality": "hq",
            "abr": false, "codec_fallback": true, "sample_rate": 0, "bit_depth": 0, "capture_mode": "loopback",
            "audio_device_id": "", "audio_device_name": "", "auto_switch_device": true, "app_exe": "",
            "app_name": "", "volume_sync": "auto", "volume_device_id": "", "volume_device_name": "",
            "max_media_payload": 679
        }))
        .unwrap();
        assert!(saved.mic_default_input);
        let stream = |state| StreamStatus { state, profile: Some("WH".into()), ..StreamStatus::default() };
        let want = |state, p: &Profile| default_input_want(&stream(state), std::slice::from_ref(p), true);
        assert_eq!(want(StreamState::Streaming, &p), InputWant::Switch("{cable}".into()));
        // Muted from the card: still the device's microphone, kept
        let muted = Profile { mic_muted: true, ..p.clone() };
        assert_eq!(want(StreamState::Streaming, &muted), InputWant::Switch("{cable}".into()));
        assert_eq!(want(StreamState::Connecting, &p), InputWant::Keep);
        assert_eq!(want(StreamState::Reconnecting, &p), InputWant::Keep);
        assert_eq!(want(StreamState::Idle, &p), InputWant::Release);
        assert_eq!(want(StreamState::Error, &p), InputWant::Release);
        // Turned off, headset mode off, no output: not switched
        for off in [
            Profile { mic_default_input: false, ..p.clone() },
            Profile { headset_mic: "off".into(), ..p.clone() },
            Profile { mic_device_id: String::new(), ..p.clone() },
            Profile { name: "other".into(), ..p.clone() },
        ] {
            assert_eq!(want(StreamState::Streaming, &off), InputWant::Release);
        }
        // Outside developer mode the switch is on whatever the profile says
        let off = Profile { mic_default_input: false, ..p.clone() };
        let streaming = stream(StreamState::Streaming);
        assert_eq!(
            default_input_want(&streaming, std::slice::from_ref(&off), false),
            InputWant::Switch("{cable}".into())
        );
        // Turning it alone keeps the stream going
        let off = CoreProfile { mic_default_input: false, ..core.clone() };
        assert!(core.only_default_input_differs(&off));
        assert!(!core.only_default_input_differs(&core));
        assert!(!merge_profile(Some(&core), &to_ipc_profile(&off, 679)).mic_default_input);
        // Written only when off
        assert!(serde_json::to_value(&core).unwrap().get("mic_default_input").is_none());
        assert_eq!(serde_json::to_value(&off).unwrap()["mic_default_input"], false);
    }

    #[test]
    fn the_microphone_codec_chosen_reaches_the_request_with_auto_select_off() {
        let classic = CoreProfile {
            name: "WH".into(),
            device_address: "00:A2:D0:00:00:12".into(),
            headset_mic: "on".into(),
            mic_device_id: "{cable}".into(),
            voice_codec: "cvsd".into(),
            ..CoreProfile::default()
        };
        let mut req = classic.to_request(1000).unwrap();
        set_mic_codec(&mut req, &classic, false);
        assert_eq!(req.voice_codec, Some(bluecaster_core::VoiceCodec::Cvsd));
        set_mic_codec(&mut req, &classic, true);
        assert_eq!(req.voice_codec, None);
        assert!(mic_choice_matters(&classic));
        assert!(!mic_choice_matters(&CoreProfile { voice_codec: String::new(), ..classic.clone() }));
        assert!(!mic_choice_matters(&CoreProfile { headset_mic: "off".into(), ..classic.clone() }));
        // LE Audio: the microphone's rate
        let le = CoreProfile { le_audio: true, mic_rate: 16000, ..classic };
        let mut req = le.to_request(1000).unwrap();
        set_mic_codec(&mut req, &le, false);
        assert_eq!((req.voice_codec, req.le.as_ref().unwrap().mic_rate), (None, Some(16000)));
        set_mic_codec(&mut req, &le, true);
        assert_eq!(req.le.as_ref().unwrap().mic_rate, None);
        assert!(mic_choice_matters(&le));
        // The UI's value as saved: a call codec not known is none chosen
        let saved = merge_profile(None, &Profile { voice_codec: "lc3".into(), ..to_ipc_profile(&le, 1000) });
        assert_eq!(saved.voice_codec, "");
    }

    #[test]
    fn le_audio_switched_connects_the_same_device_over_the_other_stack() {
        let mk = |name: &str, addr: &str, le: bool| CoreProfile {
            name: name.into(),
            device_address: addr.into(),
            le_audio: le,
            ..CoreProfile::default()
        };
        let list = vec![
            mk("Buds", "AA:BB:CC:DD:EE:01", false),
            mk("Buds LE Audio", "aa:bb:cc:dd:ee:01", true),
            mk("Other", "AA:BB:CC:DD:EE:02", false),
        ];
        assert_eq!(le_switched_profile(&list, &list[0], true), "Buds LE Audio");
        assert_eq!(le_switched_profile(&list, &list[1], false), "Buds");
        // Nothing to switch to: the same one
        assert_eq!(le_switched_profile(&list, &list[2], true), "Other");
        assert_eq!(le_switched_profile(&list, &list[0], false), "Buds");
        // The LE Audio side's profile made from the classic one
        let other = CoreProfile { device_name: "Other".into(), ..list[2].clone() };
        let made = le_side_profile(&list, &other);
        assert_eq!((made.name.as_str(), made.le_audio), ("Other LE Audio", true));
        assert_eq!(made.device_address, "AA:BB:CC:DD:EE:02");
    }

    #[test]
    fn the_tray_names_the_voice_codec() {
        assert_eq!(voice_line("en", "msbc"), "Voice: mSBC");
        assert_eq!(voice_line("en", "cvsd"), "Voice: CVSD");
        assert_eq!(voice_line("en", "lc3_swb"), "Voice: LC3");
        assert_eq!(voice_line("en", "lc3"), "Voice: LC3");
    }

    #[test]
    fn a_codec_stop_is_named_in_a_few_words() {
        let stop = |codec: &str, kind| ipc::CodecStop { codec: codec.into(), kind };
        assert_eq!(codec_stop_text("en", &stop("ldac", ipc::CodecStopKind::Unsupported)), "LDAC is unsupported");
        assert_eq!(
            codec_stop_text("en", &stop("sbcxq", ipc::CodecStopKind::SettingsUnsupported)),
            "Unsupported SBC XQ settings"
        );
        assert_eq!(codec_stop_text("en", &stop("aptxhd", ipc::CodecStopKind::InUse)), "aptX HD is in use");
        assert_eq!(codec_stop_text("en", &stop("opus05", ipc::CodecStopKind::Refused)), "Opus (PipeWire) was refused");
        assert_eq!(codec_stop_text("ja", &stop("ldac", ipc::CodecStopKind::Unsupported)), "LDAC 非対応");
        // Every language has all four
        for lang in ["ja", "de", "fr", "es", "ko", "zh-CN", "zh-TW", "uk", "ru"] {
            for key in [
                "status.codec_stop_unsupported",
                "status.codec_stop_settings_unsupported",
                "status.codec_stop_in_use",
                "status.codec_stop_refused",
            ] {
                let text = bluecaster_core::messages::translate(lang, key, "");
                assert!(
                    text.contains("%s") && text != bluecaster_core::messages::translate("en", key, ""),
                    "{lang} {key}"
                );
            }
        }
    }

    #[test]
    fn the_codec_that_stopped_the_stream_reaches_its_status() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-codec-stop-{}", std::process::id()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let tooltip = Arc::new(Mutex::new(String::new()));
        let shown = tooltip.clone();
        let state = CoreState::new(
            Arc::new(Service::new(options, tx)),
            Box::new(move |v: TrayView| *shown.lock().unwrap() = v.tooltip),
            Box::new(|| {}),
            Box::new(|_| {}),
        );
        let refused = bluecaster_core::Msg::codec_stop(
            "error.codec_unsupported_refused",
            codec::CodecKind::Ldac,
            CodecStopKind::Unsupported,
        );
        *state.starting_profile.lock().unwrap() = Some("EarFun | LDAC".into());
        state.on_service_event(ServiceEvent::State(
            CoreStreamState::Connecting,
            bluecaster_core::Msg::new("status.ready"),
        ));
        state.on_service_event(ServiceEvent::State(CoreStreamState::Error, refused.clone()));
        let s = state.snapshot().1.stream;
        assert_eq!(s.codec_stop, Some(ipc::CodecStop { codec: "ldac".into(), kind: ipc::CodecStopKind::Unsupported }));
        assert_eq!(s.message.key, "error.codec_unsupported_refused");
        assert!(stopped_by_codec(&s, "EarFun | LDAC"));
        // In the settings' language (the system's by default)
        let lang = state.snapshot().1.settings.language;
        let line = codec_stop_text(&lang, s.codec_stop.as_ref().unwrap());
        assert!(tooltip.lock().unwrap().ends_with(&format!("\n{line}")), "{}", tooltip.lock().unwrap());
        // The next connection clears it; other errors carry none
        state.on_service_event(ServiceEvent::State(
            CoreStreamState::Connecting,
            bluecaster_core::Msg::new("status.ready"),
        ));
        assert_eq!(state.snapshot().1.stream.codec_stop, None);
        state.on_service_event(ServiceEvent::State(
            CoreStreamState::Error,
            bluecaster_core::Msg::new("error.reconnect_failed"),
        ));
        let s = state.snapshot().1.stream;
        assert_eq!(s.codec_stop, None);
        assert!(!stopped_by_codec(&s, "EarFun | LDAC"));
        assert!(!tooltip.lock().unwrap().contains('\n'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choosing_a_codec_after_a_codec_stop_connects_again() {
        let ldac = CoreProfile {
            name: "EarFun | LDAC".into(),
            codec: "ldac".into(),
            codec_forced: true,
            ..CoreProfile::default()
        };
        let aac = CoreProfile { name: "EarFun | AAC".into(), codec: "aac".into(), ..CoreProfile::default() };
        let stop = Some(ipc::CodecStop { codec: "ldac".into(), kind: ipc::CodecStopKind::Unsupported });
        let stopped = StreamStatus {
            state: StreamState::Error,
            profile: Some("EarFun | LDAC".into()),
            codec_stop: stop.clone(),
            ..StreamStatus::default()
        };
        assert!(reconnect_after_codec_stop(&stopped, "EarFun | LDAC", Some(&ldac), &aac));
        // Its settings changed: "Show only options it supports" on again
        let listed = CoreProfile { unlisted_options: false, ..CoreProfile { unlisted_options: true, ..ldac.clone() } };
        assert!(reconnect_after_codec_stop(
            &stopped,
            "EarFun | LDAC",
            Some(&CoreProfile { unlisted_options: true, ..ldac.clone() }),
            &listed
        ));
        // Not for another device, nor for what the stream does not use
        assert!(!reconnect_after_codec_stop(&stopped, "WH | AAC", Some(&ldac), &aac));
        let order = CoreProfile { codec_order: vec!["aac".into()], ..ldac.clone() };
        assert!(!reconnect_after_codec_stop(&stopped, "EarFun | LDAC", Some(&ldac), &order));
        // Another error (the device out of reach) or the user's own
        // disconnect: nothing connects by itself
        let other = StreamStatus { codec_stop: None, ..stopped.clone() };
        assert!(!reconnect_after_codec_stop(&other, "EarFun | LDAC", Some(&ldac), &aac));
        let idle = StreamStatus { state: StreamState::Idle, ..stopped.clone() };
        assert!(!reconnect_after_codec_stop(&idle, "EarFun | LDAC", Some(&ldac), &aac));
        let streaming = StreamStatus { state: StreamState::Streaming, ..stopped };
        assert!(!reconnect_after_codec_stop(&streaming, "EarFun | LDAC", Some(&ldac), &aac));
    }

    #[test]
    fn another_source_changes_in_place_only_alone_and_while_streaming() {
        let before = CoreProfile { name: "WH".into(), ..CoreProfile::default() };
        let app = CoreProfile { capture_mode: "app".into(), app_exe: "Spotify.exe".into(), ..before.clone() };
        let stream = |state| StreamStatus { state, profile: Some("WH".into()), ..StreamStatus::default() };
        let streaming = stream(StreamState::Streaming);
        assert!(source_changes_in_place(&streaming, Some(&before), &app));
        // Saved with something else, or nothing that changes the source
        let codec = CoreProfile { codec: "aac".into(), ..app.clone() };
        assert!(!source_changes_in_place(&streaming, Some(&before), &codec));
        let names_only = CoreProfile { app_exe: "Spotify.exe".into(), ..before.clone() };
        assert!(!source_changes_in_place(&streaming, Some(&before), &names_only));
        // Still connecting (a restart may be on its way), LE Audio, a new profile
        for state in [StreamState::Connecting, StreamState::Reconnecting] {
            assert!(!source_changes_in_place(&stream(state), Some(&before), &app));
        }
        let le = CoreProfile { le_audio: true, ..before.clone() };
        assert!(!source_changes_in_place(&streaming, Some(&le), &CoreProfile { le_audio: true, ..app.clone() }));
        assert!(!source_changes_in_place(&streaming, None, &app));
    }

    #[test]
    fn a_new_codec_order_restarts_a_profile_on_auto_or_fallen_back() {
        let profile = |codec: &str| CoreProfile { name: "WH".into(), codec: codec.into(), ..CoreProfile::default() };
        let streaming = |requested: Option<&str>| StreamStatus {
            state: StreamState::Streaming,
            profile: Some("WH".into()),
            info: Some(ipc::StreamInfo {
                codec: "AAC".into(),
                bitrate_kbps: 0,
                sample_rate: 48000,
                channels: 2,
                bits: 16,
                source_sample_rate: 48000,
                source_channels: 2,
                source_bits: 32,
                requested_codec: requested.map(str::to_owned),
                requested_in_use: false,
                abr: false,
                bitrate_capped: false,
                hi_res_set: false,
                low_latency: false,
                source: None,
                le: None,
                details: None,
            }),
            ..StreamStatus::default()
        };
        assert!(restart_for_order(&streaming(None), &profile("auto"), false));
        assert!(restart_for_order(&streaming(Some("aptX HD")), &profile("aptxhd"), false));
        // Auto-select streams Auto in the device's order, whatever codec chosen
        assert!(restart_for_order(&streaming(None), &profile("aac"), true));
        // Its own codec in use, LE Audio
        assert!(!restart_for_order(&streaming(None), &profile("aac"), false));
        assert!(!restart_for_order(&streaming(None), &CoreProfile { le_audio: true, ..profile("auto") }, true));
    }

    #[test]
    fn auto_select_on_or_off_restarts_a_profile_that_chose_its_codec() {
        let profile = |codec: &str| CoreProfile { name: "WH".into(), codec: codec.into(), ..CoreProfile::default() };
        let streaming =
            StreamStatus { state: StreamState::Streaming, profile: Some("WH".into()), ..StreamStatus::default() };
        assert!(restart_for_auto(&streaming, &profile("aac")));
        // Auto either way, another profile, nothing running, LE Audio
        assert!(!restart_for_auto(&streaming, &profile("auto")));
        // Auto in its own order: Auto-select takes the same order
        assert!(!restart_for_auto(&streaming, &CoreProfile { codec_order: vec!["sbc".into()], ..profile("auto") }));
        assert!(!restart_for_auto(&streaming, &CoreProfile { name: "Buds".into(), ..profile("aac") }));
        assert!(!restart_for_auto(&StreamStatus::default(), &profile("aac")));
        assert!(!restart_for_auto(&streaming, &CoreProfile { le_audio: true, ..profile("aac") }));
    }

    #[test]
    fn developer_mode_off_restarts_a_stream_on_opus05() {
        let streaming = |codec: &str| StreamStatus {
            state: StreamState::Streaming,
            profile: Some("WH".into()),
            info: Some(ipc::StreamInfo { codec: codec.into(), requested_codec: None, ..sbc_abr_info() }),
            ..StreamStatus::default()
        };
        assert!(streams_developer_codec(&streaming("Opus 05")));
        assert!(!streams_developer_codec(&streaming("Opus")));
        assert!(!streams_developer_codec(&StreamStatus { state: StreamState::Idle, ..streaming("Opus 05") }));
        assert!(!streams_developer_codec(&StreamStatus { profile: None, ..streaming("Opus 05") }));
    }

    #[test]
    fn auto_select_on_or_off_keeps_a_stream_that_would_stay_the_same() {
        use bluecaster_core::stream::KnownCodecs;
        let profile = |codec: &str| CoreProfile { name: "WH".into(), codec: codec.into(), ..CoreProfile::default() };
        let streaming = |codec: &str| StreamStatus {
            state: StreamState::Streaming,
            profile: Some("WH".into()),
            info: Some(ipc::StreamInfo { codec: codec.into(), requested_codec: None, ..sbc_abr_info() }),
            ..StreamStatus::default()
        };
        let known = KnownCodecs { keys: vec!["sbc".into(), "aac".into(), "ldac".into()], ..KnownCodecs::default() };
        let same = |s: &StreamStatus, p: &CoreProfile, on: bool, own: bool| {
            same_after_auto_toggle(s, p, on, Some(&known), own)
        };

        // LDAC chosen, which is what Auto takes, with Auto's settings
        assert!(same(&streaming("LDAC"), &profile("ldac"), true, false));
        assert!(same(&streaming("LDAC"), &profile("ldac"), false, false));
        // Another codec, or other settings
        assert!(!same(&streaming("AAC"), &profile("aac"), true, false));
        let sq = CoreProfile { quality: "sq".into(), ..profile("ldac") };
        assert!(!same(&streaming("LDAC"), &sq, true, false));
        // ... which developer mode keeps under Auto-select
        assert!(same(&streaming("LDAC"), &sq, true, true));
        // Turned off: AAC chosen while Auto streams LDAC
        assert!(!same(&streaming("LDAC"), &profile("aac"), false, false));
        // A codec not offered falls back to the profile's order
        let order = CoreProfile { codec_order: vec!["aac".into()], ..profile("aptxhd") };
        assert!(same(&streaming("AAC"), &order, false, false));
        // ... and Auto-select takes the same order
        assert!(same(&streaming("AAC"), &order, true, false));
        // Unknown codecs, or not streaming yet: restart
        assert!(!same_after_auto_toggle(&streaming("LDAC"), &profile("ldac"), true, None, false));
        let connecting = StreamStatus { state: StreamState::Connecting, ..streaming("LDAC") };
        assert!(!same(&connecting, &profile("ldac"), true, false));
    }

    fn render_device(id: &str, name: &str, is_default: bool) -> audio_capture::DeviceInfo {
        audio_capture::DeviceInfo {
            id: id.into(),
            name: name.into(),
            short_name: None,
            adapter_name: None,
            is_default,
            is_virtual: name.contains("VB-Audio"),
            is_display: name.contains("NVIDIA"),
            capture_name: None,
            capture_rate: None,
        }
    }

    #[test]
    fn vb_cable_installed_after_the_hifi_cable_takes_the_microphone() {
        // issue #86: the Hi-Fi Cable alone at first, VB-CABLE installed later
        // (its setup makes it the Windows default output): the sound ended
        // up on VB-CABLE and the microphone on the Hi-Fi Cable
        let dir = std::env::temp_dir().join(format!("bluecaster-state-cables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let hifi = render_device("{hifi}", "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)", false);
        let before = [render_device("{spk}", "Speakers (Realtek(R) Audio)", true), hifi.clone()];
        state.watch_cables(&before);
        assert!(state.service.settings().awaiting_vb_cable);
        // A device added on Audio Devices: Windows default, and the Hi-Fi
        // Cable for its microphone as the rules gave it then
        let mut added = CoreProfile {
            name: "Buds".into(),
            device_address: "C0:11:22:33:44:55".into(),
            headset_mic: "auto".into(),
            ..CoreProfile::default()
        };
        profiles::fill_mic_outputs(std::slice::from_mut(&mut added), &cables(&before), "{spk}");
        assert_eq!(added.mic_device_id, "{hifi}");
        profiles::save(&dir, &[added]).unwrap();
        let after = [
            render_device("{spk}", "Speakers (Realtek(R) Audio)", false),
            hifi,
            render_device("{vb}", "CABLE Input (VB-Audio Virtual Cable)", true),
        ];
        state.watch_cables(&after);
        let p = &profiles::load(&dir)[0];
        assert_eq!((p.capture_mode.as_str(), p.audio_device_id.as_str()), ("virtual", "{hifi}"));
        assert_eq!(
            (p.mic_device_id.as_str(), p.mic_device_name.as_str()),
            ("{vb}", "CABLE Input (VB-Audio Virtual Cable)")
        );
        assert!(!state.service.settings().awaiting_vb_cable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cable_installed_after_the_setup_takes_the_microphone() {
        // issue #86: set up with a monitor's HDMI alone; VB-CABLE installed
        // later became the sound, with no microphone
        let dir = std::env::temp_dir().join(format!("bluecaster-state-cable-later-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let hdmi = |default| render_device("{hdmi}", "LG TV (NVIDIA High Definition Audio)", default);
        let vb = |default| render_device("{vb}", "CABLE Input (VB-Audio Virtual Cable)", default);
        state.watch_cables(&[hdmi(true)]);
        let added = CoreProfile {
            name: "Buds".into(),
            device_address: "C0:11:22:33:44:55".into(),
            headset_mic: "auto".into(),
            ..CoreProfile::default()
        };
        profiles::save(&dir, &[added]).unwrap();
        let set_up = || {
            let p = profiles::load(&dir).remove(0);
            (p.capture_mode, p.audio_device_id, p.mic_device_id)
        };
        // The microphone, the sound from the HDMI output: before and after
        // the cable's setup makes it Windows' default output
        state.watch_cables(&[hdmi(true), vb(false)]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{vb}".into()));
        state.watch_cables(&[hdmi(false), vb(true)]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{vb}".into()));
        // Windows lists nothing for a moment: nothing is known
        state.watch_cables(&[]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{vb}".into()));
        // The cable uninstalled: as before it came, nothing left unchosen
        state.watch_cables(&[hdmi(true)]);
        assert_eq!(set_up(), ("loopback".into(), String::new(), String::new()));
        // And installed again
        state.watch_cables(&[hdmi(true), vb(false)]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{vb}".into()));
        // Uninstalled where there is a sound card too: the microphone into
        // it, Off (Auto would open for the card's own microphone)
        let speakers = render_device("{spk}", "Speakers (Realtek(R) Audio)", true);
        state.watch_cables(&[speakers.clone(), hdmi(false)]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{spk}".into()));
        assert_eq!(profiles::load(&dir)[0].headset_mic, "off");
        // A cable again: the microphone on it, on Auto
        state.watch_cables(&[speakers, hdmi(false), vb(false)]);
        assert_eq!(set_up(), ("output".into(), "{hdmi}".into(), "{vb}".into()));
        assert_eq!(profiles::load(&dir)[0].headset_mic, "auto");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a new device gets with `devices` (the UI's setupAudio): the
    /// capture mode, the source, the microphone output and its mode
    fn fresh_setup(devices: &[audio_capture::DeviceInfo]) -> (String, String, String, String) {
        let c = cables(devices);
        let real = real_output(devices).map(|d| d.id.clone());
        let auto = "auto".to_string();
        match (c.vb, c.hifi) {
            (Some(vb), Some(hifi)) => ("virtual".into(), hifi.id, vb.id, auto),
            (Some(c), None) | (None, Some(c)) => match real {
                Some(r) => ("output".into(), r, c.id, auto),
                None => ("virtual".into(), c.id, String::new(), auto),
            },
            (None, None) => match real_output(devices).filter(|d| d.is_display).zip(sound_card(devices)) {
                Some((sound, mic)) => ("output".into(), sound.id.clone(), mic.id.clone(), "off".into()),
                None => ("loopback".into(), String::new(), String::new(), auto),
            },
        }
    }

    #[test]
    fn every_cable_coming_or_going_ends_as_a_new_device_would_start() {
        // issue #86: each set of outputs, each cable installed or removed,
        // whichever output is Windows' default after it. The table is
        // printed (--nocapture)
        let real_sets: [(&str, &[(&str, &str)]); 5] = [
            ("no output", &[]),
            ("sound card", &[("{spk}", "Speakers (Realtek(R) Audio)")]),
            ("HDMI", &[("{tv}", "LG TV (NVIDIA High Definition Audio)")]),
            (
                "sound card + HDMI",
                &[("{spk}", "Speakers (Realtek(R) Audio)"), ("{tv}", "LG TV (NVIDIA High Definition Audio)")],
            ),
            (
                "sound card + 2 HDMI",
                &[
                    ("{spk}", "Speakers (Realtek(R) Audio)"),
                    ("{tv}", "LG TV (NVIDIA High Definition Audio)"),
                    ("{mon}", "M28U (NVIDIA High Definition Audio)"),
                ],
            ),
        ];
        let vb = ("{vb}", "CABLE Input (VB-Audio Virtual Cable)");
        let c16 = ("{c16}", "CABLE In 16ch (VB-Audio Virtual Cable)");
        let hifi = ("{hifi}", "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)");
        // (VB-CABLE, Hi-Fi Cable) before and after
        let steps: [((bool, bool), (bool, bool)); 8] = [
            ((false, false), (true, false)),
            ((false, false), (false, true)),
            ((true, false), (true, true)),
            ((false, true), (true, true)),
            ((true, true), (true, false)),
            ((true, true), (false, true)),
            ((true, false), (false, false)),
            ((false, true), (false, false)),
        ];
        let list = |real: &[(&str, &str)], (with_vb, with_hifi): (bool, bool), default: &str| {
            let mut all: Vec<(&str, &str)> = real.to_vec();
            if with_vb {
                all.extend([c16, vb]);
            }
            if with_hifi {
                all.push(hifi);
            }
            all.iter().map(|(id, name)| render_device(id, name, *id == default)).collect::<Vec<_>>()
        };
        let cables_named = |(v, h): (bool, bool)| match (v, h) {
            (true, true) => "VB + Hi-Fi",
            (true, false) => "VB",
            (false, true) => "Hi-Fi",
            (false, false) => "none",
        };
        let mut failed = Vec::new();
        let mut n = 0;
        println!("| outputs | cables before | cables after | default after | sound | microphone | mode | as new |");
        println!("|---|---|---|---|---|---|---|---|");
        for (real_name, real) in real_sets {
            for (from, to) in steps {
                // Windows' default output before: each output there
                let ids = |set: (bool, bool)| list(real, set, "").iter().map(|d| d.id.clone()).collect::<Vec<_>>();
                for default_before in ids(from).iter().map(String::as_str).chain([""]) {
                    for default_after in ids(to).iter().map(String::as_str).chain([""]) {
                        // The default moves only to a cable that came, or
                        // off one that went
                        let came_or_same = default_after == default_before
                            || (!ids(from).iter().any(|i| i == default_after) && !default_after.is_empty())
                            || !ids(to).iter().any(|i| i == default_before);
                        if !came_or_same || default_before == "{c16}" || default_after == "{c16}" {
                            continue;
                        }
                        n += 1;
                        let dir = std::env::temp_dir().join(format!("bluecaster-matrix-{}-{n}", std::process::id()));
                        let _ = std::fs::remove_dir_all(&dir);
                        std::fs::create_dir_all(&dir).unwrap();
                        let (tx, _rx) = std::sync::mpsc::channel();
                        let options = bluecaster_core::ServiceOptions {
                            config_dir: dir.clone(),
                            hci_tcp: None,
                            capture: bt_host::HciCapture::new(),
                            afh: None,
                            usb: Default::default(),
                        };
                        let state = CoreState::new(
                            Arc::new(Service::new(options, tx)),
                            Box::new(|_| {}),
                            Box::new(|| {}),
                            Box::new(|_| {}),
                        );
                        let before = list(real, from, default_before);
                        let after = list(real, to, default_after);
                        let name = |id: &str| {
                            before.iter().chain(&after).find(|d| d.id == id).map_or(String::new(), |d| d.name.clone())
                        };
                        let (mode, source, mic, headset) = fresh_setup(&before);
                        let p = CoreProfile {
                            name: "Buds".into(),
                            device_address: "C0:11:22:33:44:55".into(),
                            capture_mode: mode,
                            audio_device_name: name(&source),
                            audio_device_id: source,
                            mic_device_name: name(&mic),
                            mic_device_id: mic,
                            headset_mic: headset,
                            auto_switch_device: true,
                            ..CoreProfile::default()
                        };
                        profiles::save(&dir, std::slice::from_ref(&p)).unwrap();
                        let read = || {
                            let p = profiles::load(&dir).remove(0);
                            let source = if p.capture_mode == "loopback" { String::new() } else { p.audio_device_id };
                            (p.capture_mode, source, p.mic_device_id, p.headset_mic)
                        };
                        // Nothing moves while nothing came or went
                        state.watch_cables(&before);
                        let settled = read();
                        state.watch_cables(&after);
                        let got = read();
                        let want = fresh_setup(&after);
                        // The display it had (Windows' default then) stays:
                        // any display is the rules' choice
                        let display = |s: &(String, String, String, String)| {
                            let mut s = s.clone();
                            if s.1 == "{mon}" {
                                s.1 = "{tv}".into();
                            }
                            s
                        };
                        // No output at all: nothing is looked at (Windows
                        // may only not have listed them yet)
                        let same =
                            after.is_empty() || (display(&got) == display(&want) && settled == fresh_setup(&before));
                        let short = |id: &str| if id.is_empty() { "-".to_string() } else { id.to_string() };
                        println!(
                            "| {real_name} | {} | {} | {} | {} {} | {} | {} | {} |",
                            cables_named(from),
                            cables_named(to),
                            short(default_after),
                            got.0,
                            short(&got.1),
                            short(&got.2),
                            got.3,
                            if same { "yes" } else { "NO" },
                        );
                        if !same {
                            failed.push(format!(
                                "{real_name}: {} -> {}, default {default_before:?} -> {default_after:?}: {got:?}, a new device {want:?} (before: {settled:?})",
                                cables_named(from),
                                cables_named(to)
                            ));
                        }
                        let _ = std::fs::remove_dir_all(&dir);
                    }
                }
            }
        }
        println!("{n} cases, {} differ", failed.len());
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }

    #[test]
    fn the_real_output_is_a_display_the_default_one_of_several() {
        let tv = |default| render_device("{tv}", "LG TV (NVIDIA High Definition Audio)", default);
        let monitor = |default| render_device("{mon}", "M28U (NVIDIA High Definition Audio)", default);
        let speakers = |default| render_device("{spk}", "Speakers (Realtek(R) Audio)", default);
        let vb = render_device("{vb}", "CABLE Input (VB-Audio Virtual Cable)", false);
        let id = |d: &[audio_capture::DeviceInfo]| real_output(d).map(|d| d.id.clone());
        assert_eq!(id(&[speakers(true), tv(false), monitor(false), vb.clone()]).as_deref(), Some("{tv}"));
        assert_eq!(id(&[speakers(false), tv(false), monitor(true), vb.clone()]).as_deref(), Some("{mon}"));
        assert_eq!(id(&[speakers(true), vb.clone()]).as_deref(), Some("{spk}"));
        assert_eq!(id(&[vb]), None);
    }

    #[test]
    fn another_devices_output_source_made_the_default_does_not_count() {
        // issue #86: one device's Output Source, made the Windows default
        // while it streams, took that cable from the other devices
        let devices = [
            render_device("{spk}", "Speakers (Realtek(R) Audio)", false),
            render_device("{hifi}", "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)", true),
        ];
        let earfun = CoreProfile {
            name: "EarFun".into(),
            capture_mode: "virtual".into(),
            audio_device_id: "{hifi}".into(),
            ..CoreProfile::default()
        };
        let list = [earfun];
        let stream = |state| StreamStatus { state, profile: Some("EarFun".into()), ..StreamStatus::default() };
        assert_eq!(default_output_for_cables(&devices, &stream(StreamState::Streaming), &list), "");
        // Not streaming: the default is the user's
        assert_eq!(default_output_for_cables(&devices, &stream(StreamState::Idle), &list), "{hifi}");
        let loopback = CoreProfile { capture_mode: "loopback".into(), ..list[0].clone() };
        assert_eq!(default_output_for_cables(&devices, &stream(StreamState::Streaming), &[loopback]), "{hifi}");
        // An Output Device source is made the default as well (issue #86)
        let output = CoreProfile { capture_mode: "output".into(), ..list[0].clone() };
        assert_eq!(default_output_for_cables(&devices, &stream(StreamState::Streaming), &[output]), "");
    }

    #[test]
    fn switching_profiles_keeps_the_new_profile() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-switch-{}", std::process::id()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let event = |st| state.on_service_event(ServiceEvent::State(st, bluecaster_core::Msg::new("status.ready")));
        // stream.start of B while A streams: the core says Connecting (B),
        // then A's session ends (Idle) and B's starts
        *state.starting_profile.lock().unwrap() = Some("B".into());
        event(CoreStreamState::Idle);
        assert_eq!(state.snapshot().1.stream.profile, None);
        event(CoreStreamState::Connecting);
        assert_eq!(state.snapshot().1.stream.profile.as_deref(), Some("B"));
        event(CoreStreamState::Streaming);
        assert_eq!(state.snapshot().1.stream.profile.as_deref(), Some("B"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_microphone_that_opens_drops_why_it_did_not_before() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-micnote-{}", std::process::id()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let say = |key| {
            state.on_service_event(ServiceEvent::State(CoreStreamState::Streaming, bluecaster_core::Msg::new(key)))
        };
        let key = || state.snapshot().1.stream.message.key;
        let voice = |on: bool| {
            state.on_service_event(ServiceEvent::Voice(on.then(|| ipc::VoiceStatus {
                codec: "msbc".into(),
                link: "hfp".into(),
                rate: 16000,
                mic_device_id: "{other}".into(),
            })))
        };
        say("status.connected_fallback_developer");
        say("error.headset_mic_loop");
        assert_eq!(key(), "error.headset_mic_loop");
        // Closing changes nothing; another output that opens does
        voice(false);
        assert_eq!(key(), "error.headset_mic_loop");
        voice(true);
        assert_eq!(key(), "status.connected_fallback_developer");
        // A note over a note keeps the message from before both
        voice(false);
        say("error.headset_mic_loop");
        say("status.headset_mic_failed");
        voice(true);
        assert_eq!(key(), "status.connected_fallback_developer");
        // Another message in between: nothing of the old one comes back
        say("error.headset_mic_loop");
        say("status.reconnected");
        say("status.headset_mic_failed");
        voice(true);
        assert_eq!(key(), "status.reconnected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SBC at 48 kHz under ABR (bitpool 19..53), 1011-byte payloads
    fn sbc_abr_info() -> ipc::StreamInfo {
        serde_json::from_value(serde_json::json!({
            "codec": "SBC", "bitrate_kbps": 357, "sample_rate": 48000, "channels": 2, "bits": 16,
            "source_sample_rate": 48000, "source_channels": 2, "source_bits": 32,
            "requested_codec": null, "requested_in_use": false, "abr": true,
            "details": {
                "mtu": 1011, "rtp": true, "media_header": 1, "frame_samples": 128,
                "frame_bytes": 119, "frames_per_packet": 8,
                "sbc": {
                    "channel_mode": "joint_stereo", "blocks": 16, "subbands": 8, "allocation": "loudness",
                    "bitpool": 53, "config_bitpool": [2, 53], "device": null, "abr_bitpool": [19, 53]
                },
                "aac": null
            }
        }))
        .unwrap()
    }

    #[test]
    fn naming_the_codec_an_auto_profile_streams_keeps_the_stream() {
        let before = CoreProfile { name: "WH | Auto".into(), codec: "auto".into(), ..CoreProfile::default() };
        let streaming =
            StreamStatus { state: StreamState::Streaming, info: Some(sbc_abr_info()), ..StreamStatus::default() };
        let named =
            CoreProfile { name: "WH | SBC".into(), codec: "sbc".into(), unlisted_options: true, ..before.clone() };
        assert!(codec_named_in_place(&streaming, Some(&before), &named));
        // Another codec than the one streaming, or other values: connect again
        let aac = CoreProfile { codec: "aac".into(), ..named.clone() };
        assert!(!codec_named_in_place(&streaming, Some(&before), &aac));
        let other = CoreProfile { quality: "sq".into(), ..named.clone() };
        assert!(!codec_named_in_place(&streaming, Some(&before), &other));
        // A codec already named, a fallback, no stream yet
        assert!(!codec_named_in_place(&streaming, Some(&named), &named));
        let mut fell_back = streaming.clone();
        fell_back.info.as_mut().unwrap().requested_codec = Some("LDAC".into());
        assert!(!codec_named_in_place(&fell_back, Some(&before), &named));
        let connecting = StreamStatus { state: StreamState::Connecting, ..streaming.clone() };
        assert!(!codec_named_in_place(&connecting, Some(&before), &named));
        assert!(!codec_named_in_place(&streaming, None, &named));
    }

    #[test]
    fn bitpools_change_in_place_within_the_range_of_the_link() {
        let before =
            CoreProfile { name: "WH".into(), codec: "sbc".into(), sbc_min_bitpool: 20, ..CoreProfile::default() };
        let mut info = sbc_abr_info();
        info.details.as_mut().unwrap().sbc.as_mut().unwrap().config_bitpool = Some((20, 53));
        let stream = |state| StreamStatus {
            state,
            profile: Some("WH".into()),
            info: Some(info.clone()),
            ..StreamStatus::default()
        };
        let streaming = stream(StreamState::Streaming);
        let with = |max, min| CoreProfile { sbc_bitpool: max, sbc_min_bitpool: min, ..before.clone() };
        assert!(bitpools_change_in_place(&streaming, Some(&before), &with(30, 20)));
        assert!(bitpools_change_in_place(&streaming, Some(&before), &with(0, 25)));
        // Below what the link was set up with
        assert!(!bitpools_change_in_place(&streaming, Some(&before), &with(30, 10)));
        assert!(!bitpools_change_in_place(&streaming, Some(&before), &with(15, 20)));
        assert!(!bitpools_change_in_place(&streaming, Some(&before), &with(30, 0)));
        // Anything else changes the link
        let rate = CoreProfile { sample_rate: 44100, ..with(30, 20) };
        assert!(!bitpools_change_in_place(&streaming, Some(&before), &rate));
        assert!(!bitpools_change_in_place(&stream(StreamState::Connecting), Some(&before), &with(30, 20)));
        assert!(!bitpools_change_in_place(&streaming, None, &with(30, 20)));
    }

    #[test]
    fn live_codec_values_go_into_the_stream_info() {
        let info = sbc_abr_info();
        let live = bt_host::LiveCodec { bitrate_kbps: 249, bitpool: 35, frame_bytes: 83 };
        let next = live_info(&info, live).unwrap();
        assert_eq!(next.bitrate_kbps, 249);
        let d = next.details.as_deref().unwrap();
        assert_eq!((d.frame_bytes, d.frames_per_packet), (Some(83), Some(12)));
        let s = d.sbc.as_ref().unwrap();
        assert_eq!((s.bitpool, s.abr_bitpool, s.config_bitpool), (35, Some((19, 53)), Some((2, 53))));
        // Nothing changed: nothing to send
        assert_eq!(live_info(&next, live), None);
        // At most 15 frames (the 4-bit count)
        let small = bt_host::LiveCodec { bitrate_kbps: 100, bitpool: 12, frame_bytes: 40 };
        assert_eq!(live_info(&info, small).unwrap().details.unwrap().frames_per_packet, Some(15));
        // Not under ABR, or LE Audio: left alone
        let fixed = ipc::StreamInfo { abr: false, ..info.clone() };
        assert_eq!(live_info(&fixed, live), None);
        // LDAC under ABR: the bit rate only
        let ldac = ipc::StreamInfo { codec: "LDAC".into(), details: None, ..info };
        assert_eq!(live_info(&ldac, live).map(|i| (i.bitrate_kbps, i.details)), Some((249, None)));
    }

    #[test]
    fn live_codec_updates_only_a_streaming_snapshot() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-live-{}", std::process::id()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        let live = bt_host::LiveCodec { bitrate_kbps: 249, bitpool: 35, frame_bytes: 83 };
        let mut events = state.subscribe();
        let set = |st: StreamState| {
            state.publish(Event::Stream(StreamStatus { state: st, info: Some(sbc_abr_info()), ..Default::default() }))
        };
        set(StreamState::Reconnecting);
        let _ = events.try_recv();
        state.apply_live_codec(Some(live));
        assert!(events.try_recv().is_err());
        set(StreamState::Streaming);
        let _ = events.try_recv();
        state.apply_live_codec(None);
        assert!(events.try_recv().is_err());
        let rev = state.snapshot().0;
        state.apply_live_codec(Some(live));
        let (r, e) = events.try_recv().unwrap();
        assert_eq!(r, rev + 1);
        let Event::Stream(st) = &*e else { panic!("{e:?}") };
        assert_eq!(st.info.as_ref().map(|i| i.bitrate_kbps), Some(249));
        assert_eq!(state.snapshot().1.stream.info.unwrap().details.unwrap().sbc.unwrap().bitpool, 35);
        // The same again: no event
        state.apply_live_codec(Some(live));
        assert!(events.try_recv().is_err());
        // The bit rate lowered for a slow link: beside the one asked for
        state.apply_slow_link(Some(330));
        let (_, e) = events.try_recv().unwrap();
        let Event::Stream(st) = &*e else { panic!("{e:?}") };
        assert_eq!(st.slow_link, Some(330));
        state.apply_slow_link(Some(330));
        assert!(events.try_recv().is_err());
        state.apply_slow_link(None);
        assert_eq!(state.snapshot().1.stream.slow_link, None);
        let _ = events.try_recv();
        // Not while the link is down
        set(StreamState::Reconnecting);
        let _ = events.try_recv();
        state.apply_slow_link(Some(330));
        assert!(events.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_streams_are_built_as_profiles() {
        let r = custom_request(&custom("tone"), 679).unwrap();
        assert_eq!(r.device, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(r.codec, Some(codec::CodecKind::Ldac));
        assert_eq!(r.quality, codec::Quality::Standard);
        assert_eq!(r.source, SourceRequest::TestTone);
        assert_eq!(r.volume_sync, bluecaster_core::VolumeSync::Off);
        assert!(!r.codec_fallback);
        assert_eq!(r.max_media_payload, 679);
        assert_eq!((r.sample_rate, r.bit_depth), (None, None));

        let mut c = custom("app");
        c.source_id = Some("Spotify.exe".into());
        c.sample_rate = Some(96000);
        c.bit_depth = Some(24);
        c.max_media_payload = Some(1023);
        c.codec = "auto".into();
        let r = custom_request(&c, 679).unwrap();
        assert_eq!(r.source, SourceRequest::Application { exe: "Spotify.exe".into() });
        assert_eq!((r.sample_rate, r.bit_depth, r.codec, r.max_media_payload), (Some(96000), Some(24), None, 1023));

        let mut v = custom("virtual");
        v.source_id = Some("{dev}".into());
        v.source_name = Some("CABLE Input".into());
        let r = custom_request(&v, 679).unwrap();
        assert_eq!(
            r.source,
            SourceRequest::VirtualDevice { id: "{dev}".into(), name: "CABLE Input".into(), auto_switch: false }
        );
        assert_eq!(custom_request(&custom("loopback"), 679).unwrap().source, SourceRequest::SystemLoopback);
    }

    #[test]
    fn bad_custom_streams_are_refused() {
        assert_eq!(custom_request(&custom("radio"), 679).unwrap_err().code, "stream.bad_source");
        let mut c = custom("tone");
        c.codec = "mp3".into();
        assert_eq!(custom_request(&c, 679).unwrap_err().code, "stream.bad_codec");
        c.codec = "sbc".into();
        c.device = "not an address".into();
        assert_eq!(custom_request(&c, 679).unwrap_err().code, "profile.invalid_address");
    }

    #[test]
    fn receiver_types_convert_field_for_field() {
        let line = concat!(
            r#"{"type":"stats","version":1,"connected":true,"streaming":true,"device":"AA:BB:CC:DD:EE:FF","#,
            r#""buffer_target_ms":200,"mtu":1005,"codecs":["SBC","LDAC"],"rssi":-3,"tx_power":4,"rssi_note":"","#,
            r#""afh_channels":40,"stream":{"id":2,"codec":"LDAC","config":"LDAC 96 kHz","decoding":true,"#,
            r#""packets":100,"bytes":100000,"frames":400,"lost":1,"late":2,"ts_errors":3,"frame_errors":4,"#,
            r#""decode_errors":5,"underruns":6,"overflows":7,"pauses":8,"idle":false,"audio_ms":1000.5,"#,
            r#""underrun_ms":12.5,"overflow_ms":1.5,"max_gap_ms":30.0,"jitter_ms":2.5,"buffer_ms":180.0,"#,
            r#""interval":{"packets":120,"bytes":120000,"max_gap_ms":20.0}}}"#
        );
        let core = receiver::parse_stats(line).expect("a stats line");
        let ipc: ipc::ReceiverStats = convert(&core).expect("converts");
        // Every field arrives: both give the same JSON
        assert_eq!(json(&ipc), json(&core));
        assert_eq!(ipc.stream.as_ref().map(|s| (s.decode_errors, s.interval_bytes)), Some((5, 120000)));

        let info = receiver::ReceiverInfo {
            ip: "192.168.1.5".into(),
            port: 51201,
            host: "sink".into(),
            bt_address: "00:11:22:33:44:55".into(),
            bt_name: "hci0".into(),
            codecs: vec!["SBC".into()],
            mtu: 895,
            buffer_ms: 200,
        };
        let i: ipc::ReceiverInfo = convert(&info).expect("converts");
        assert_eq!(json(&i), json(&info));
    }

    #[test]
    fn link_details_to_the_protocol() {
        let mut d = bt_host::LinkDetails::default();
        assert_eq!((remote_chip(&d), link_details(&d)), (None, None));
        d.remote_version = Some(bt_host::RemoteVersion { lmp_version: 12, company_id: 0x7777, lmp_subversion: 3 });
        let chip = remote_chip(&d).unwrap();
        // An unlisted maker shows as its ID
        assert_eq!((chip.company.as_deref(), chip.version.as_deref()), (Some("0x7777"), Some("5.3")));
        assert!(chip.features.is_none());
        d.remote_features = Some(bt_host::RemoteFeatures([0x03, 0, 0, 0x02, 0x80, 0x01, 0, 0]));
        assert_eq!(remote_chip(&d).unwrap().features.unwrap().largest_packet, "2-DH5");
        d.central = Some(true);
        d.mode = Some(bt_host::LinkMode::Sniff);
        d.sniff_interval_slots = Some(80);
        d.supervision_timeout_slots = Some(8000);
        let l = link_details(&d).unwrap();
        assert_eq!((l.role, l.mode), (Some(ipc::LinkRole::Central), Some(ipc::LinkMode::Sniff)));
        assert_eq!((l.sniff_interval_ms, l.supervision_timeout_ms), (Some(50.0), Some(5000.0)));
        // The controller does not answer: no counts either
        assert_eq!((l.failed_contacts, l.failed_contacts_stream, l.link_quality), (None, None, None));
    }

    #[test]
    fn link_totals_since_the_stream_started() {
        let s = LinkStats::default();
        let r = Ordering::Relaxed;
        s.packets_sent.store(10, r);
        s.queue_drops.store(1, r);
        s.send_errors.store(2, r);
        s.start_packets.store(4, r);
        s.start_dropped.store(1, r);
        s.stream_starts.store(1, r);
        let (total, start, starts) = link_totals(&s);
        assert_eq!(starts, 1);
        assert_eq!(total.dropped, 3);
        let since = total.since(&start);
        assert_eq!((since.packets, since.dropped), (6, 2));
    }

    #[test]
    fn autostart_follows_both_settings() {
        let off = CoreSettings::default();
        let on = CoreSettings { start_with_windows: true, ..CoreSettings::default() };
        let on_min = CoreSettings { start_minimized: true, ..on.clone() };
        assert!(autostart_changed(&off, &on));
        assert!(autostart_changed(&on, &on_min));
        assert!(!autostart_changed(&on, &on));
        // Minimized alone does not matter while autostart is off
        assert!(!autostart_changed(&off, &CoreSettings { start_minimized: true, ..off.clone() }));
        assert_eq!(autostart_args(true), "--background");
        assert_eq!(autostart_args(false), "");
    }

    #[test]
    fn autostart_fix_takes_over_v1_and_leaves_other_programs_alone() {
        use AutostartFix::{Keep, Remove, Write};
        let present = |p: &std::path::Path| {
            [r"C:\A2DP-Windows-Bridge\build\dist\A2DPWB.exe", r"C:\Program Files\BlueCaster\bluecaster-core.exe"]
                .contains(&p.to_str().unwrap())
        };
        let fix = |current, enabled| autostart_fix(current, enabled, true, present);
        // Missing or empty: written when on
        assert_eq!(fix(None, true), Write);
        assert_eq!(fix(Some(""), true), Write);
        assert_eq!(fix(None, false), Keep);
        // A copy of the core that is gone (moved or uninstalled): written
        assert_eq!(fix(Some(r#""D:\old\bluecaster-core.exe" --background"#), true), Write);
        assert_eq!(fix(Some(r#""D:\old\BlueCaster-Core.EXE""#), true), Write);
        // Another existing copy of v2: kept (a test build must not take it)
        assert_eq!(fix(Some(r#""C:\Program Files\BlueCaster\bluecaster-core.exe""#), true), Keep);
        assert_eq!(fix(Some(r#""C:\Program Files\BlueCaster\bluecaster-core.exe""#), false), Keep);
        // v1, existing or not: taken over as by an update
        let v1 = r#""C:\A2DP-Windows-Bridge\build\dist\A2DPWB.exe" --minimized"#;
        assert_eq!(fix(Some(v1), true), Write);
        assert_eq!(fix(Some(v1), false), Remove);
        assert_eq!(fix(Some(r#""E:\gone\A2DPWB.exe""#), true), Write);
        assert_eq!(fix(Some(r"E:\v1\a2dpwb.EXE --minimized"), false), Remove);
        // ... but not by a development instance
        assert_eq!(autostart_fix(Some(v1), true, false, present), Keep);
        assert_eq!(autostart_fix(Some(v1), false, false, present), Keep);
        // Any other program: kept
        assert_eq!(fix(Some(r"C:\Tools\other.exe /x"), true), Keep);
        assert_eq!(fix(Some(r"C:\Tools\other.exe /x"), false), Keep);
    }

    #[test]
    fn autostart_takes_over_the_old_value_of_a_core_or_v1() {
        use AutostartFix::{Keep, Remove, Write};
        let installed = r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --background"#;
        let present = |p: &std::path::Path| p.to_str() == Some(r"C:\Program Files\BlueCaster\bluecaster-core.exe");
        let plan = |ours, old, enabled| autostart_plan(ours, old, enabled, true, present);
        let p = |ours, remove_old| AutostartPlan { ours, remove_old };
        let old_core = r#""C:\Users\u\AppData\Local\Programs\A2DP Windows Bridge\a2dpwb-core.exe" --background"#;
        // 2.0.x's entry: ours written, the old one removed
        assert_eq!(plan(None, Some(old_core), true), p(Write, true));
        assert_eq!(plan(Some(""), Some(r"D:\x\A2DPWB-Core.EXE"), true), p(Write, true));
        // ... also over a gone copy of ours
        assert_eq!(plan(Some(r#""D:\gone\bluecaster-core.exe""#), Some(old_core), true), p(Write, true));
        // ... ours already names an existing core: kept, the old one removed
        assert_eq!(plan(Some(installed), Some(old_core), true), p(Keep, true));
        // An old value naming a bluecaster-core.exe (any folder)
        assert_eq!(plan(None, Some(r#""E:\bc\bluecaster-core.exe""#), true), p(Write, true));
        // Off: the old one removed, ours as the setting says
        assert_eq!(plan(None, Some(old_core), false), p(Keep, true));
        assert_eq!(plan(Some(installed), Some(old_core), false), p(Keep, true));
        // v1 under the old name: taken over as before the rename
        let v1 = r#""C:\A2DP-Windows-Bridge\build\dist\A2DPWB.exe" --minimized"#;
        assert_eq!(plan(None, Some(v1), true), p(Write, true));
        assert_eq!(plan(None, Some(v1), false), p(Keep, true));
        assert_eq!(plan(Some(v1), None, false), p(Remove, false));
        // Any other program under the old name: never removed
        assert_eq!(plan(None, Some(r"C:\Tools\other.exe /x"), true), p(Write, false));
        assert_eq!(plan(None, Some(r"C:\Tools\other.exe /x"), false), p(Keep, false));
        assert_eq!(plan(Some(installed), Some(r"C:\Tools\a2dpwb-core.exe.bak"), true), p(Keep, false));
        // Another program under our name: neither touched
        assert_eq!(plan(Some(r"C:\Tools\other.exe"), Some(old_core), true), p(Keep, false));
        // No old value
        assert_eq!(plan(Some(installed), None, true), p(Keep, false));
        assert_eq!(plan(None, None, true), p(Write, false));
        // A development instance leaves the old value alone
        assert_eq!(autostart_plan(None, Some(old_core), true, false, present), p(Write, false));
        assert_eq!(autostart_plan(None, Some(v1), false, false, present), p(Keep, false));
    }

    #[test]
    fn chip_changes_need_new_firmware() {
        let a = CoreSettings::default();
        assert!(!chip_changed(&a, &a.clone()));
        assert!(chip_changed(&a, &CoreSettings { bt_chip_pid: 0x8771, ..a.clone() }));
        assert!(chip_changed(&a, &CoreSettings { bt_chip_fw_stem: "rtl8761bu".into(), ..a.clone() }));
        assert!(chip_changed(&a, &CoreSettings { realtek_custom_firmware: true, ..a.clone() }));
        assert!(chip_changed(&a, &CoreSettings { adapter_id: "USB\\VID_8087&PID_0033\\1".into(), ..a.clone() }));
    }

    #[test]
    fn vendors_get_their_notes() {
        assert_eq!(vendor_note(ChipVendor::Intel, 0x8087, 0x0033).unwrap().key, "firmware.vendor_intel");
        // Qualcomm: ROME and WCN6855 have a loader, the others none (QCA6290)
        assert_eq!(vendor_note(ChipVendor::Qualcomm, 0x0CF3, 0xE300).unwrap().key, "firmware.vendor_qualcomm");
        assert_eq!(vendor_note(ChipVendor::Qualcomm, 0x0489, 0xE0C7).unwrap().key, "firmware.vendor_qualcomm");
        assert_eq!(vendor_note(ChipVendor::Qualcomm, 0x0CF3, 0xE400).unwrap().key, "firmware.vendor_unsupported");
        // MediaTek: the MT79xx, the MT7927 and the MT7663 have a loader, the
        // others none (MT7630)
        assert_eq!(vendor_note(ChipVendor::MediaTek, 0x0E8D, 0x7925).unwrap().key, "firmware.vendor_mediatek");
        assert_eq!(vendor_note(ChipVendor::MediaTek, 0x0E8D, 0x0608).unwrap().key, "firmware.vendor_mediatek");
        assert_eq!(vendor_note(ChipVendor::MediaTek, 0x0E8D, 0x7927).unwrap().key, "firmware.vendor_mediatek");
        assert_eq!(vendor_note(ChipVendor::MediaTek, 0x0E8D, 0x7630).unwrap().key, "firmware.vendor_unsupported");
        assert!(vendor_note(ChipVendor::Realtek, 0x0BDA, 0x8771).is_none());
        // Realtek chips whose firmware comes from Realtek's Windows driver
        assert_eq!(vendor_note(ChipVendor::Realtek, 0x0BDA, 0xB853).unwrap().key, "firmware.vendor_realtek_driver");
        assert_eq!(
            vendor_note(ChipVendor::Realtek, 0x0BDA, 0xB892).unwrap().key,
            "firmware.vendor_realtek_driver_other"
        );
        assert_eq!(
            vendor_note(ChipVendor::Realtek, 0x13D3, 0x3618).unwrap().key,
            "firmware.vendor_realtek_driver_other"
        );
        // ROM dongles and Wi-Fi-loaded combos: a note, never "unsupported"
        assert_eq!(vendor_note(ChipVendor::Barrot, 0x33FA, 0x0010).unwrap().key, "firmware.vendor_barrot");
        assert_eq!(vendor_note(ChipVendor::Marvell, 0x1286, 0x204C).unwrap().key, "firmware.vendor_marvell");
        assert_eq!(vendor_note(ChipVendor::Actions, 0x10D7, 0xB012).unwrap().key, "firmware.vendor_actions");
    }

    #[test]
    fn an_earbud_lists_the_other_addresses_of_its_set() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        devices::save(&dir, "C0:00:00:00:00:01", "Buds").unwrap();
        devices::save_le(&dir, "C0:00:00:00:00:01", "Buds").unwrap();
        le_sets::remember(&dir, &["C0:00:00:00:00:01".into(), "C0:00:00:00:00:02".into()]).unwrap();
        let list = saved_devices(&dir, &[], &BTreeMap::new());
        // Its classic side has the one address
        assert!(!list[0].le_audio && list[0].set_addresses.is_empty());
        assert!(list[1].le_audio);
        assert_eq!(list[1].set_addresses, ["C0:00:00:00:00:02"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devices_carry_their_codecs() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-dev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        devices::save(&dir, "AA:BB:CC:DD:EE:FF", "Phones").unwrap();
        let scanned = vec![Device {
            address: "11:22:33:44:55:66".into(),
            name: "Speaker".into(),
            saved: false,
            audio: true,
            rssi: Some(-60),
            codecs: Vec::new(),
            sbc: None,
            codec_caps: Vec::new(),
            codecs_on_request: Vec::new(),
            le_audio: false,
            le_audio_set: false,
            hid_le: None,
            set_addresses: Vec::new(),
        }];
        let mut codecs = BTreeMap::new();
        let sbc = bluecaster_core::SbcDeviceCaps {
            sample_rates: vec![44100, 48000],
            channel_modes: vec!["joint_stereo".into()],
            blocks: vec![8, 16],
            subbands: vec![8],
            allocations: vec!["loudness".into()],
            min_bitpool: 2,
            max_bitpool: 53,
        };
        codecs.insert(
            "AA:BB:CC:DD:EE:FF".to_string(),
            bluecaster_core::KnownCodecs {
                keys: vec!["sbc".to_string(), "ldac".to_string()],
                sbc: Some(sbc),
                caps: vec![bluecaster_core::CodecDeviceCaps {
                    codec: "ldac".into(),
                    sample_rates: vec![44100, 48000],
                    ..Default::default()
                }],
                hi_res_asked: false,
            },
        );
        let list = saved_devices(&dir, &scanned, &codecs);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].codecs, ["sbc", "ldac"]);
        // What its SBC endpoint offered, for the profile editor
        assert_eq!(list[0].sbc.as_ref().map(|s| s.blocks.clone()), Some(vec![8, 16]));
        // And what each codec lists, for the codec settings (issue #86)
        assert_eq!(list[0].codec_caps.len(), 1);
        assert_eq!(
            (list[0].codec_caps[0].codec.as_str(), list[0].codec_caps[0].sample_rates.clone()),
            ("ldac", vec![44100, 48000])
        );
        assert!(list[1].sbc.is_none());
        assert!(list[0].saved && !list[1].saved && list[1].codecs.is_empty());
        assert!(list[0].codecs_on_request.is_empty());
        // Huawei earbuds list LDAC only once it is asked for (issue #30)
        devices::save(&dir, "AA:BB:CC:DD:EE:FF", "HUAWEI FreeBuds Pro 4").unwrap();
        let list = saved_devices(&dir, &scanned, &codecs);
        assert_eq!(list[0].codecs_on_request, ["ldac"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn battery_low_balloon_once_per_connection() {
        let mut warned = false;
        let alerts: Vec<bool> = [50, 30, 20, 19, 15, 10, 22, 18, 25, 19, 5]
            .into_iter()
            .map(|v| battery_low_alert(&mut warned, v, 20))
            .collect();
        // Below 20 once; back at 22 is not enough, 25 rearms it
        assert_eq!(alerts, [false, false, false, true, false, false, false, false, false, true, false]);
        // A connection that starts low warns at its first report
        let mut warned = false;
        assert!(battery_low_alert(&mut warned, 12, 20));
        assert!(!battery_low_alert(&mut warned, 11, 20));
        // Below 5 %: 10 is not low; 0 never warns
        let mut warned = false;
        assert!(!battery_low_alert(&mut warned, 10, 5));
        assert!(battery_low_alert(&mut warned, 4, 5));
        let mut warned = false;
        assert!(!battery_low_alert(&mut warned, 0, 0));
    }

    #[test]
    fn codec_order_is_stored_with_known_keys_once() {
        let ui =
            to_ipc_profile(&CoreProfile { device_address: "C0:11:22:33:44:55".into(), ..CoreProfile::default() }, 679);
        assert!(ui.codec_order.is_empty());
        let order = ["SBCXQ", "nope", "faststream", "sbcxq", "aac"].map(String::from).to_vec();
        let merged = merge_profile(None, &Profile { codec_order: order, ..ui });
        assert_eq!(merged.codec_order, ["sbcxq", "faststream", "aac"]);
        assert_eq!(to_ipc_profile(&merged, 679).codec_order, merged.codec_order);
        // aptX Adaptive LL's card keeps its place (issue #86)
        let ui = to_ipc_profile(&merged, 679);
        let order = ["aac", "AptxAdLL", "aptxad"].map(String::from).to_vec();
        assert_eq!(
            merge_profile(None, &Profile { codec_order: order, ..ui }).codec_order,
            ["aac", "aptxadll", "aptxad"]
        );
    }

    /// The playback EQ goes through the UI and back as saved, and a save
    /// that changes only it keeps the stream going
    #[test]
    fn media_eq_is_saved_and_changes_in_place() {
        let base =
            CoreProfile { name: "WH".into(), device_address: "C0:11:22:33:44:55".into(), ..CoreProfile::default() };
        let ui = to_ipc_profile(&base, 679);
        assert_eq!(ui.media_eq, ipc::MediaEq::default());
        let eq = ipc::MediaEq {
            enabled: true,
            bands: vec![ipc::EqPoint { frequency: 120.0, gain: 3.5, q: 0.9, band_type: ipc::EqBandType::LowShelf }],
        };
        let merged = merge_profile(Some(&base), &Profile { media_eq: eq.clone(), ..ui });
        assert_eq!(to_ipc_profile(&merged, 679).media_eq, eq);
        // As saved before from the UI: the EQ alone differs
        let before = merge_profile(Some(&base), &to_ipc_profile(&base, 679));
        assert!(before.only_media_eq_differs(&merged));
    }

    #[test]
    fn aptx_adaptive_and_its_ll_card_keep_their_own_settings() {
        // issue #86: aptX Adaptive's settings were dropped while its LL card
        // was chosen (both are codec "aptxad"), and came back as LL's
        let ad = profiles::CodecValues { sample_rate: 96000, abr: true, ..profiles::CodecValues::default() };
        let ll =
            profiles::CodecValues { quality: "ll".into(), unlisted_options: true, ..profiles::CodecValues::default() };
        let core = CoreProfile {
            device_address: "C0:11:22:33:44:55".into(),
            codec: "aptxad".into(),
            quality: "ll".into(),
            sample_rate: 48000,
            codec_settings: BTreeMap::from([("aptxad".to_string(), ad.clone())]),
            ..CoreProfile::default()
        };
        // LL chosen: aptX Adaptive's own stay
        let saved = merge_profile(None, &to_ipc_profile(&core, 679));
        assert_eq!(saved.codec_settings.get("aptxad"), Some(&ad));
        assert_eq!((saved.quality.as_str(), saved.sample_rate), ("ll", 48000));
        // aptX Adaptive chosen: LL's stay, a stale copy of aptX Adaptive's goes
        let back = CoreProfile {
            quality: "hq".into(),
            sample_rate: 96000,
            abr: true,
            codec_settings: BTreeMap::from([("aptxad".to_string(), ad.clone()), ("aptxadll".to_string(), ll.clone())]),
            ..core
        };
        let saved = merge_profile(None, &to_ipc_profile(&back, 679));
        assert_eq!(saved.codec_settings.get("aptxadll"), Some(&ll));
        assert!(!saved.codec_settings.contains_key("aptxad"));
        // Each streams its own
        let r = saved.to_request(679).unwrap();
        assert_eq!((r.quality, r.sample_rate, r.abr), (codec::Quality::High, Some(96000), true));
    }

    #[test]
    fn le_audio_profiles_and_settings_map_both_ways() {
        let core = CoreProfile {
            name: "Buds LE Audio".into(),
            device_address: "C0:11:22:33:44:55".into(),
            le_audio: true,
            ..CoreProfile::default()
        };
        let ui = to_ipc_profile(&core, 679);
        assert!(ui.le_audio);
        assert!(merge_profile(None, &ui).le_audio);
        // The UI turns it off: the stored profile follows
        let classic = Profile { le_audio: false, ..ui };
        assert!(!merge_profile(Some(&core), &classic).le_audio);
        // Headset mode and its output, both ways
        assert!(classic.headset_mic == "off" && classic.mic_device_id.is_empty());
        let mic = Profile {
            headset_mic: "auto".into(),
            mic_device_id: "{cable}".into(),
            mic_device_name: "CABLE Input".into(),
            ..classic
        };
        let stored = merge_profile(Some(&core), &mic);
        assert_eq!(stored.headset_mic_mode(), HeadsetMic::Auto);
        // Anything else from the UI is stored as off
        let odd = Profile { headset_mic: "maybe".into(), ..mic.clone() };
        assert_eq!(merge_profile(Some(&core), &odd).headset_mic, "off");
        assert_eq!((stored.mic_device_id.as_str(), stored.mic_device_name.as_str()), ("{cable}", "CABLE Input"));
        assert_eq!(to_ipc_profile(&stored, 679), mic);

        let base = CoreSettings::default();
        let ui = to_ipc_settings(&base);
        // LE Audio off by default (issue #86: Adapters' switch), GMAP low latency off
        assert!(!ui.le_audio_experimental && !ui.le_audio_low_latency);
        let on = Settings { le_audio_experimental: true, le_audio_low_latency: true, ..ui };
        let merged = merge_settings(&base, &on);
        assert!(merged.le_audio_experimental && merged.le_audio_low_latency);
        assert_eq!(to_ipc_settings(&merged), on);

        // The output mute: off by default
        assert!(!to_ipc_settings(&base).mute_output);
        let mute = Settings { mute_output: true, ..on.clone() };
        assert!(merge_settings(&base, &mute).mute_output);

        // Windows Volume Sync for every device: on by default
        assert!(to_ipc_settings(&base).volume_sync);
        let no_sync = Settings { volume_sync: false, ..on };
        let merged = merge_settings(&base, &no_sync);
        assert!(!merged.volume_sync);
        assert_eq!(to_ipc_settings(&merged), no_sync);
    }

    #[test]
    fn volume_sync_for_every_device_overrides_the_profiles() {
        let dev = || VolumeSync::Device("{0.0.0.00000000}.{x}".into());
        // Off for every device: no profile syncs
        for p in [VolumeSync::Auto, VolumeSync::Off, dev()] {
            assert_eq!(effective_volume_sync(false, p), VolumeSync::Off);
        }
        // On: neither a profile's old "off" nor a device it followed counts
        for p in [VolumeSync::Auto, VolumeSync::Off, dev()] {
            assert_eq!(effective_volume_sync(true, p), VolumeSync::Auto);
        }
    }

    #[test]
    fn le_audio_devices_are_their_own_entries() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-le-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A dual-mode headset saved as both kinds
        devices::save(&dir, "AA:BB:CC:DD:EE:FF", "Phones").unwrap();
        devices::save_le(&dir, "AA:BB:CC:DD:EE:FF", "Phones LE").unwrap();
        let scan = [
            ScannedDevice {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Phones".into(),
                rssi: -50,
                class_of_device: 0x240404,
                audio: true,
                le_audio: false,
                le_audio_set: false,
                hid_le: None,
            },
            ScannedDevice {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Phones".into(),
                rssi: -55,
                class_of_device: 0,
                audio: true,
                le_audio: true,
                le_audio_set: false,
                hid_le: None,
            },
            ScannedDevice {
                address: "C0:11:22:33:44:55".into(),
                name: "Earbuds".into(),
                rssi: -60,
                class_of_device: 0,
                audio: true,
                le_audio: true,
                le_audio_set: true,
                hid_le: None,
            },
        ];
        let scanned: Vec<Device> = scan.iter().map(scanned_device).collect();
        assert!(scanned[2].le_audio && scanned[2].le_audio_set && !scanned[2].saved);
        assert_eq!(scanned[2].rssi, Some(-60));
        let mut codecs = BTreeMap::new();
        codecs.insert(
            "AA:BB:CC:DD:EE:FF".to_string(),
            bluecaster_core::KnownCodecs { keys: vec!["sbc".to_string()], ..Default::default() },
        );
        let list = saved_devices(&dir, &scanned, &codecs);
        let kinds: Vec<(&str, bool, bool)> = list.iter().map(|d| (d.name.as_str(), d.le_audio, d.saved)).collect();
        // Merged by address AND kind: each saved entry got its own scan result
        assert_eq!(kinds, [("Phones", false, true), ("Phones LE", true, true), ("Earbuds", true, false)]);
        assert_eq!((list[0].rssi, list[1].rssi), (Some(-50), Some(-55)));
        assert!(list[2].le_audio_set);
        // Codecs are the classic entry's
        assert_eq!(list[0].codecs, ["sbc"]);
        assert!(list[1].codecs.is_empty());
        // Forgetting the LE Audio entry keeps the classic one
        devices::remove_le(&dir, "AA:BB:CC:DD:EE:FF").unwrap();
        let list = saved_devices(&dir, &[], &codecs);
        assert_eq!(list.len(), 1);
        assert!(!list[0].le_audio);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bluetooth LE keyboards and mice (issue #413) stay out of the device
    /// list the audio screens use, also when a saved device has their address
    #[test]
    fn le_keyboards_and_mice_stay_out_of_the_audio_devices() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-hid-le-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        devices::save(&dir, "AA:BB:CC:DD:EE:FF", "Phones").unwrap();
        let entry = |address: &str, name: &str, audio: bool, hid_le: Option<u8>| ScannedDevice {
            address: address.into(),
            name: name.into(),
            rssi: -50,
            class_of_device: 0,
            audio,
            le_audio: false,
            le_audio_set: false,
            hid_le,
        };
        let scan = [
            entry("AA:BB:CC:DD:EE:FF", "Phones", true, None),
            entry("11:22:33:44:55:66", "Classic keyboard", false, None),
            entry("C0:11:22:33:44:55", "LE mouse", false, Some(1)),
            // The saved headphones' address: still not that device
            entry("AA:BB:CC:DD:EE:FF", "LE keyboard", false, Some(0)),
        ];
        let (scanned, hid) = split_scan(&scan);
        let names = |l: &[Device]| l.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&scanned), ["Phones", "Classic keyboard"]);
        assert_eq!(names(&hid), ["LE mouse", "LE keyboard"]);
        assert_eq!(hid.iter().map(|d| d.hid_le).collect::<Vec<_>>(), [Some(1), Some(0)]);
        assert!(hid.iter().all(|d| !d.audio && !d.le_audio && !d.saved));
        let list = saved_devices(&dir, &scanned, &BTreeMap::new());
        assert_eq!(names(&list), ["Phones", "Classic keyboard"]);
        assert_eq!(list[0].rssi, Some(-50));
        // Even when handed one, the device list leaves it out, and nothing is saved
        let all: Vec<Device> = scan.iter().map(scanned_device).collect();
        let list = saved_devices(&dir, &all, &BTreeMap::new());
        assert_eq!(names(&list), ["Phones", "Classic keyboard"]);
        assert!(list.iter().all(|d| d.hid_le.is_none()));
        assert_eq!(devices::load_all(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A removed device stayed on Home and Codecs for a while
    #[test]
    fn forgetting_takes_a_device_off_the_lists_at_once() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A dual-mode headset (both kinds), LE Audio earbuds alone, another device
        let (dual, set, kept) = ("00:A2:D0:00:00:31", "00:A2:D0:00:00:32", "00:A2:D0:00:00:33");
        devices::save(&dir, dual, "Phones").unwrap();
        devices::save_le(&dir, dual, "Phones").unwrap();
        devices::save_le(&dir, set, "Buds").unwrap();
        devices::save(&dir, kept, "Speaker").unwrap();
        let profile = |name: &str, address: &str, le_audio: bool| CoreProfile {
            name: name.into(),
            device_address: address.into(),
            le_audio,
            ..CoreProfile::default()
        };
        let list = [
            profile("Phones | Auto", dual, false),
            profile("Phones LE Audio", dual, true),
            profile("Buds LE Audio", set, true),
            profile("Speaker | Auto", kept, false),
        ];
        profiles::save(&dir, &list).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state =
            CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}));
        state.refresh_devices();
        state.publish(Event::Profiles {
            profiles: list.iter().map(|p| to_ipc_profile(p, 679)).collect(),
            active_profile: Some("Buds LE Audio".into()),
        });
        let saved = |state: &CoreState| -> Vec<(String, bool)> {
            let s = state.snapshot().1;
            s.devices.iter().filter(|d| d.saved).map(|d| (d.address.clone(), d.le_audio)).collect()
        };
        assert_eq!(saved(&state).len(), 4);

        // The profiles, then the device list, each once
        let mut events = state.subscribe();
        state.forget_device(set, true).unwrap();
        let kinds: Vec<&str> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|(_, e)| match *e {
                Event::Profiles { .. } => Some("profiles"),
                Event::Devices(_) => Some("devices"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["profiles", "devices"]);
        assert!(!saved(&state).iter().any(|(a, _)| a == set));
        let s = state.snapshot().1;
        assert!(s.profiles.iter().all(|p| p.device_address != set));
        assert_eq!(s.active_profile, None);

        // Both kinds of the dual-mode headset, one after the other (Audio
        // Devices removes both): only the other device and its profile stay
        state.forget_device(dual, true).unwrap();
        assert_eq!(saved(&state), [(dual.to_string(), false), (kept.to_string(), false)]);
        state.forget_device(dual, false).unwrap();
        assert_eq!(saved(&state), [(kept.to_string(), false)]);
        let names: Vec<String> = state.snapshot().1.profiles.into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Speaker | Auto"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn state_in(dir: &Path) -> Arc<CoreState> {
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.to_path_buf(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        CoreState::new(Arc::new(Service::new(options, tx)), Box::new(|_| {}), Box::new(|| {}), Box::new(|_| {}))
    }

    /// Both earbuds of a pair saved as two devices: one entry, at startup
    /// (set known from before) or once a connection learns the set
    #[test]
    fn earbuds_of_one_set_become_one_saved_device() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-sets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(paths::devices_dir(&dir)).unwrap();
        let (left, right, other) = ("C0:00:00:00:00:01", "C0:00:00:00:00:02", "00:A2:D0:00:00:33");
        devices::save_le(&dir, left, "Buds").unwrap();
        devices::save_le(&dir, right, "Buds").unwrap();
        devices::save(&dir, other, "Speaker").unwrap();
        // The profile is the right one's: it stays
        let profile = |name: &str, address: &str| CoreProfile {
            name: name.into(),
            device_address: address.into(),
            device_name: "Buds".into(),
            le_audio: true,
            ..CoreProfile::default()
        };
        profiles::save(&dir, &[profile("Buds LE Audio", right)]).unwrap();
        let state = state_in(&dir);
        // Not known as one set yet: two entries
        let saved_le = |state: &CoreState| -> Vec<String> {
            let s = state.snapshot().1;
            s.devices.iter().filter(|d| d.saved && d.le_audio).map(|d| d.address.clone()).collect()
        };
        assert_eq!(saved_le(&state), [left, right]);

        // A connection learned the set
        le_sets::remember(&dir, &[left.into(), right.into()]).unwrap();
        let mut events = state.subscribe();
        state.on_service_event(ServiceEvent::LeSetLearned);
        assert_eq!(saved_le(&state), [right]);
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|(_, e)| matches!(*e, Event::Devices(_))));
        assert_eq!(devices::load_le(&dir).len(), 1);
        // The other earbud is the same device from now on
        devices::save_le(&dir, left, "Buds").unwrap();
        assert_eq!(devices::load_le(&dir).len(), 1);

        // Saved twice again by an older version, the left one with a
        // profile and a volume: merged at startup into the one set up
        std::fs::write(paths::le_devices_file(&dir), format!("{right}|Buds\n{left}|Buds (L)\n")).unwrap();
        profiles::save(&dir, &[profile("Left", left)]).unwrap();
        let mut settings = CoreSettings::default();
        settings.saved_volumes.insert(left.into(), 55);
        settings.save(&dir).unwrap();
        let state = state_in(&dir);
        assert_eq!(saved_le(&state), [left]);
        assert_eq!(state.snapshot().1.profiles[0].device_address, left);

        // Removing it removes every member
        devices::save_le(&dir, right, "").unwrap();
        std::fs::write(paths::le_devices_file(&dir), format!("{left}|Buds\n{right}|Buds\n")).unwrap();
        profiles::save(&dir, &[profile("Left", left), profile("Right", right)]).unwrap();
        state.forget_device(left, true).unwrap();
        assert!(devices::load_le(&dir).is_empty());
        assert!(profiles::load(&dir).is_empty());
        assert!(le_sets::load(&dir).is_empty());
        assert!(!state.service.settings().saved_volumes.contains_key(left));
        assert_eq!(devices::load(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A profile saved for the other earbud of a saved set is the saved
    /// device's
    #[test]
    fn a_profile_for_another_member_goes_to_the_saved_device() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-set-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(paths::devices_dir(&dir)).unwrap();
        let (left, right) = ("C0:00:00:00:00:11", "C0:00:00:00:00:12");
        devices::save_le(&dir, left, "Buds").unwrap();
        le_sets::remember(&dir, &[left.into(), right.into()]).unwrap();
        let state = state_in(&dir);
        let profile = CoreProfile {
            name: "Buds LE Audio".into(),
            device_address: right.into(),
            device_name: "Buds".into(),
            le_audio: true,
            ..CoreProfile::default()
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(
            state.handle(Method::ProfileSave { profile: Box::new(to_ipc_profile(&profile, 679)), original_name: None }),
        );
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(devices::load_le(&dir).len(), 1);
        assert_eq!(profiles::load(&dir)[0].device_address, left);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_stream_details_reach_the_ui() {
        let d = LeStreamDetails {
            preset: "48_3_gr".into(),
            frame_us: 7500,
            octets: 75,
            cis: 2,
            devices: 2,
            low_latency: true,
            summary: "LC3 48 kHz 7.5 ms 75 octets (80 kbps/ch), 2 ch over 2 CIS, 48_3_gr".into(),
        };
        let le = to_ipc_le_stream(&d);
        assert_eq!((le.preset.as_str(), le.frame_us, le.octets, le.cis, le.devices), ("48_3_gr", 7500, 75, 2, 2));
        assert!(le.low_latency);
        assert_eq!(le.summary, d.summary);
        assert_eq!(LE_CODEC, "LC3");
        assert_eq!(bluecaster_core::Msg::new(LE_SCANNING_KEY).key, "status.le_scanning");
    }

    #[test]
    fn capture_names_use_the_local_time() {
        let s = local_stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "_");
        assert!(s.bytes().enumerate().all(|(i, b)| i == 8 || b.is_ascii_digit()));
    }

    #[test]
    fn the_adapter_in_use_comes_from_its_list_entry() {
        let mut windows = rtl(r"USB\VID_0BDA&PID_8771\1", Some(true), None);
        windows.driver = DriverKind::Bluetooth;
        windows.display_name = "the one Windows has".into();
        let mut ours = rtl(r"USB\VID_0BDA&PID_8771\2", Some(true), None);
        ours.windows_name = Some("TP-Link UB500 Adapter".into());
        ours.bt_version = Some("5.1".into());
        ours.address = Some("00:11:22:33:44:55".into());
        let a = active_adapter(0x0BDA, 0x8771, &[windows, ours]);
        assert_eq!(
            a,
            ipc::ActiveAdapter {
                display_name: "RTL8761BU".into(),
                windows_name: Some("TP-Link UB500 Adapter".into()),
                vid: 0x0BDA,
                pid: 0x8771,
                vendor: "Realtek".into(),
                chip: Some("RTL8761BU".into()),
                bt_version: Some("5.1".into()),
            }
        );
        // Neither the address nor the instance ID reaches it
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("00:11:22") && !json.contains("VID_0BDA"), "{json}");
        // Not listed: what its VID:PID tells
        let intel = active_adapter(0x8087, 0x0033, &[]);
        assert_eq!((intel.vendor.as_str(), intel.chip, intel.windows_name), ("Intel", None, None));
        assert!(intel.display_name.contains("0x8087:0x0033"), "{}", intel.display_name);
    }

    /// settings.set can turn keyboards and mice off, never on; developer
    /// mode off turns them off for good (issue #413)
    #[test]
    fn settings_set_never_turns_the_hid_host_on() {
        let off = CoreSettings { debug_mode: true, hid_host: false, ..CoreSettings::default() };
        let mut ui = to_ipc_settings(&off);
        ui.hid_host = true;
        assert!(!merge_settings(&off, &ui).hid_host);

        let on = CoreSettings { debug_mode: true, hid_host: true, ..CoreSettings::default() };
        let mut ui = to_ipc_settings(&on);
        assert!(merge_settings(&on, &ui).hid_host);
        ui.hid_host = false;
        assert!(!merge_settings(&on, &ui).hid_host);
    }

    #[test]
    fn developer_mode_off_turns_the_hid_host_off_for_good() {
        let on = CoreSettings { debug_mode: true, hid_host: true, ..CoreSettings::default() };
        let mut ui = to_ipc_settings(&on);
        ui.developer_mode = false;
        let merged = merge_settings(&on, &ui);
        assert!(!merged.debug_mode && !merged.hid_host);
        // Developer mode on again: still off
        let mut ui = to_ipc_settings(&merged);
        ui.developer_mode = true;
        ui.hid_host = true;
        let again = merge_settings(&merged, &ui);
        assert!(again.debug_mode && !again.hid_host && !again.hid_host());
    }

    #[test]
    fn adapter_and_button_events_reach_the_clients() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-adapter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = state_in(&dir);
        let mut events = state.subscribe();
        let rev = state.rev();
        state.on_service_event(ServiceEvent::Adapter(Some((0x0BDA, 0x8771))));
        let active = state.snapshot().1.adapter.active.expect("an adapter in use");
        assert_eq!((active.vid, active.pid, active.vendor.as_str()), (0x0BDA, 0x8771, "Realtek"));
        assert_eq!(state.rev(), rev + 1);
        // The same again: nothing new
        state.on_service_event(ServiceEvent::Adapter(Some((0x0BDA, 0x8771))));
        assert_eq!(state.rev(), rev + 1);
        // A button: an event without a revision of its own
        let press = ipc::ButtonPress { button: ipc::MediaButton::Next, pressed: true, le_audio: false };
        state.on_service_event(ServiceEvent::Button(press));
        assert_eq!(state.rev(), rev + 1);
        state.on_service_event(ServiceEvent::Adapter(None));
        assert_eq!(state.snapshot().1.adapter.active, None);
        let got: Vec<(u64, Event)> =
            std::iter::from_fn(|| events.try_recv().ok()).map(|(r, e)| (r, (*e).clone())).collect();
        assert!(matches!(&got[0], (r, Event::Adapter(a)) if *r == rev + 1 && a.active.is_some()));
        assert_eq!(got[1], (rev + 1, Event::MediaButton(press)));
        assert!(matches!(&got[2], (r, Event::Adapter(a)) if *r == rev + 2 && a.active.is_none()));
        assert_eq!(got.len(), 3);
        let json = serde_json::to_value(&got[1].1).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "media_button", "value": {"button": "next", "pressed": true, "le_audio": false}})
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tool_test_profile() -> Profile {
        let core = CoreProfile {
            name: "WH".into(),
            device_address: "00:11:22:33:44:55".into(),
            device_name: "WH".into(),
            headset_mic: "auto".into(),
            mic_device_id: "{cable}".into(),
            ..CoreProfile::default()
        };
        to_ipc_profile(&core, 679)
    }

    /// profile.set_headset_mic does what the window's mode buttons do
    #[test]
    fn tool_headset_mode_follows_the_mode_buttons() {
        use ipc::HeadsetMicMode::{Auto, Off, On};
        let auto = tool_test_profile();
        // Off mutes Auto, the mode kept; the mode in force already: nothing
        let muted = tool_headset_mic(&auto, Off, false).unwrap().unwrap();
        assert_eq!((muted.headset_mic.as_str(), muted.mic_muted), ("auto", true));
        assert_eq!(tool_headset_mic(&auto, Auto, false).unwrap(), None);
        assert_eq!(tool_headset_mic(&muted, Off, false).unwrap(), None);
        // Auto unmutes
        let back = tool_headset_mic(&muted, Auto, false).unwrap().unwrap();
        assert_eq!(back, auto);
        // On: developer mode only; On saved shows as Auto outside it
        assert_eq!(tool_headset_mic(&auto, On, false).unwrap_err().code, "developer.off");
        let on = tool_headset_mic(&auto, On, true).unwrap().unwrap();
        assert_eq!((on.headset_mic.as_str(), on.mic_muted), ("on", false));
        assert_eq!(tool_headset_mic(&on, Auto, false).unwrap(), None);
        assert_eq!(tool_headset_mic(&on, Auto, true).unwrap().unwrap().headset_mic, "auto");
        // From Off: the mode, once a microphone output is chosen
        let off = Profile { headset_mic: "off".into(), ..auto.clone() };
        assert_eq!(tool_headset_mic(&off, Off, true).unwrap(), None);
        assert_eq!(tool_headset_mic(&off, Auto, false).unwrap().unwrap().headset_mic, "auto");
        let no_output = Profile { mic_device_id: String::new(), ..off };
        assert_eq!(tool_headset_mic(&no_output, Auto, false).unwrap_err().code, "profile.no_mic_output");
        assert_eq!(tool_mic_muted(&no_output, true).unwrap_err().code, "profile.no_mic_output");
        // Mute alone
        assert_eq!(tool_mic_muted(&auto, false).unwrap(), None);
        assert_eq!(tool_mic_muted(&auto, true).unwrap().unwrap(), muted);
    }

    #[test]
    fn tool_media_eq_keeps_the_points() {
        let p = tool_test_profile();
        assert_eq!(tool_media_eq(&p, false), None);
        let on = tool_media_eq(&p, true).unwrap();
        assert!(on.media_eq.enabled);
        assert_eq!(on.media_eq.bands.len(), 10);
        assert!(on.media_eq.bands.iter().all(|b| b.gain == 0.0 && b.band_type == ipc::EqBandType::Peaking));
        assert_eq!(tool_media_eq(&on, true), None);
        let off = tool_media_eq(&on, false).unwrap();
        assert_eq!((off.media_eq.enabled, off.media_eq.bands.len()), (false, 10));
        // Saved points stay as they are
        let mut custom = off.clone();
        custom.media_eq.bands.truncate(2);
        custom.media_eq.bands[0].gain = 6.0;
        let again = tool_media_eq(&custom, true).unwrap();
        assert_eq!(again.media_eq.bands, custom.media_eq.bands);
        assert_eq!(Profile { media_eq: custom.media_eq.clone(), ..again }, custom);
    }

    #[test]
    fn volume_steps_are_checked() {
        let streaming = StreamStatus { state: StreamState::Streaming, volume: Some(60), ..StreamStatus::default() };
        assert_eq!(volume_step(&streaming, 8).unwrap(), 68);
        assert_eq!(volume_step(&streaming, -8).unwrap(), 52);
        assert_eq!(volume_step(&streaming, 0).unwrap(), 60);
        // Within 0..127 at either end
        assert_eq!(volume_step(&streaming, 127).unwrap(), 127);
        assert_eq!(volume_step(&streaming, -127).unwrap(), 0);
        for delta in [128, -128, i16::MAX, i16::MIN] {
            let e = volume_step(&streaming, delta).unwrap_err();
            assert_eq!((e.code.as_str(), e.message.key.as_str()), ("stream.bad_volume_step", "ipc.error.bad_request"));
        }
        let unknown = StreamStatus { volume: None, ..streaming.clone() };
        assert_eq!(volume_step(&unknown, 1).unwrap_err().code, "stream.volume_unknown");
        let connecting = StreamStatus { state: StreamState::Connecting, ..streaming };
        assert_eq!(volume_step(&connecting, 1).unwrap_err().code, "stream.volume_unknown");
    }

    /// The tools' profile.set_* save through profile.save once; the value
    /// it has already saves nothing; a connecting stream refuses the
    /// microphone's
    #[test]
    fn tool_changes_save_the_profile() {
        let dir = std::env::temp_dir().join(format!("bluecaster-state-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(paths::devices_dir(&dir)).unwrap();
        let p = tool_test_profile();
        profiles::save(&dir, &[merge_profile(None, &p)]).unwrap();
        let state = state_in(&dir);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let call = |m: Method| rt.block_on(state.handle(m));
        let profiles_events = |events: &mut broadcast::Receiver<(u64, Arc<Event>)>| {
            std::iter::from_fn(|| events.try_recv().ok()).filter(|(_, e)| matches!(**e, Event::Profiles { .. })).count()
        };

        // No profile named, none streaming or active
        let e = call(Method::ProfileSetMicMuted { name: None, muted: true }).unwrap_err();
        assert_eq!((e.code.as_str(), e.message.key.as_str()), ("profile.not_found", "ipc.error.profile_not_found"));
        let e = call(Method::ProfileSetMediaEq { name: Some("Other".into()), enabled: true }).unwrap_err();
        assert_eq!(e.code, "profile.not_found");

        let mut events = state.subscribe();
        assert_eq!(call(Method::ProfileSetMicMuted { name: Some("WH".into()), muted: true }).unwrap(), Value::Null);
        assert!(profiles::load(&dir)[0].mic_muted);
        assert_eq!(profiles_events(&mut events), 1);
        // Saved as profile.save saves it: it is the active one now
        assert_eq!(state.snapshot().1.active_profile.as_deref(), Some("WH"));
        // The same again: nothing saved, no event
        assert_eq!(call(Method::ProfileSetMicMuted { name: None, muted: true }).unwrap(), Value::Null);
        assert_eq!(
            call(Method::ProfileSetHeadsetMic { name: None, mode: ipc::HeadsetMicMode::Off }).unwrap(),
            Value::Null
        );
        assert_eq!(profiles_events(&mut events), 0);
        let e = call(Method::ProfileSetHeadsetMic { name: None, mode: ipc::HeadsetMicMode::On }).unwrap_err();
        assert_eq!((e.code.as_str(), e.message.key.as_str()), ("developer.off", "ipc.error.developer_only"));

        // While its stream connects: the EQ (in place) yes, the microphone no
        state.publish(Event::Stream(StreamStatus {
            state: StreamState::Connecting,
            profile: Some("WH".into()),
            ..StreamStatus::default()
        }));
        let e = call(Method::ProfileSetHeadsetMic { name: None, mode: ipc::HeadsetMicMode::Auto }).unwrap_err();
        assert_eq!((e.code.as_str(), e.message.key.as_str()), ("stream.busy", "ipc.error.stream_busy"));
        assert!(profiles::load(&dir)[0].mic_muted);
        assert_eq!(call(Method::ProfileSetMediaEq { name: None, enabled: true }).unwrap(), Value::Null);
        let saved = &profiles::load(&dir)[0];
        assert!(saved.media_eq.enabled && saved.mic_muted);
        assert_eq!(state.snapshot().1.profiles[0].media_eq.bands.len(), 10);
        assert_eq!(profiles_events(&mut events), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
