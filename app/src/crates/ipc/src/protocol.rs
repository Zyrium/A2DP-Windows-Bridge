//! The messages between bluecaster-core.exe (server) and the UI (client).
//!
//! 1. The client sends [`ClientMessage::Hello`]; a different major protocol
//!    version is answered with [`ServerMessage::Reject`].
//! 2. The server sends [`ServerMessage::Welcome`] and a
//!    [`ServerMessage::Snapshot`] of its whole state with a revision.
//! 3. Then [`ServerMessage::Event`]s, each replacing one part of the state,
//!    with the next revision. A gap in the revisions means a lost event: the
//!    client asks for a new snapshot ([`Method::StateSnapshot`]). Events
//!    that are not state (link, log, receiver, rfcomm_data, rfcomm_closed,
//!    media_button) carry the current revision: they are not compared with it.
//! 4. Requests ([`ClientMessage::Request`]) are answered by a
//!    [`ServerMessage::Response`] with the same id. A request this core
//!    cannot read (a method it does not know, or bad parameters) is
//!    answered with the error `ipc.bad_request`.
//!
//! Additions within protocol 1 are listed in the Welcome's `features`
//! ([`FEATURES`]). Older cores close the connection on a request they do
//! not know, so a client checks the list before using one.
//!
//! Errors and status texts are translation keys of app/lang ([`Msg`]): the
//! UI translates them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const PROTOCOL_VERSION: u32 = 1;

/// The Welcome's `features`: "rfcomm" for rfcomm.open / send / close and
/// their events (2.0.2), "media_button" for the [`Event::MediaButton`]
/// events (after 3.0.2), "tool_controls" for profile.set_headset_mic,
/// profile.set_mic_muted, profile.set_media_eq and stream.volume_step
/// (after 3.0.3), "hid-host" for the hid.* methods and the
/// [`Event::HidDevices`] events (experimental, issue #413)
pub const FEATURES: &[&str] = &["rfcomm", "media_button", "tool_controls", "hid-host"];

/// The largest step of `stream.volume_step`, either way: the whole AVRCP
/// range (0..127)
pub const MAX_VOLUME_STEP: i16 = 127;

/// The longest `device.scan` (seconds); a longer one is cut to it
pub const MAX_SCAN_SECONDS: u32 = 600;

/// A message of app/lang: key and printf-style arguments
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct Msg {
    pub key: String,
    pub args: Vec<String>,
}

impl Msg {
    pub fn new(key: impl Into<String>) -> Msg {
        Msg { key: key.into(), args: Vec::new() }
    }
}

/// An error answer: a code the UI can act on ("adapter.driver_missing"),
/// and the message to show
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ErrorInfo {
    pub code: String,
    pub message: Msg,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    /// No stack running (started on the first scan or connection)
    #[default]
    Stopped,
    Initializing,
    Ready,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// WinUSB: usable by BlueCaster
    Winusb,
    /// Windows' Bluetooth driver: the adapter is Windows'
    Bluetooth,
    Other,
    None,
}

/// A USB Bluetooth adapter
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AdapterInfo {
    pub instance_id: String,
    pub vid: u16,
    pub pid: u16,
    /// "RTL8761BU (0x2357:0x0604)"
    pub display_name: String,
    /// Windows' name of the device
    pub device_name: String,
    pub vendor: String,
    pub driver: DriverKind,
    /// Driver service (WinUSB, BTHUSB, ...)
    pub driver_service: String,
    /// Realtek chip for the firmware download, if one
    pub realtek_chip: Option<String>,
    /// The firmware files for it are in the settings folder (None: no
    /// firmware needed)
    pub firmware_present: Option<bool>,
    /// The firmware file names it needs ("rtl8761bu_fw.bin")
    pub firmware_files: Vec<String>,
    /// One of them that is there but is no Realtek firmware (a saved web
    /// page): firmware.invalid
    #[serde(default)]
    pub firmware_invalid: Option<String>,
    /// BlueCaster can fetch those files from linux-firmware
    /// (adapter.download_firmware)
    #[serde(default)]
    pub firmware_download: bool,
    /// The firmware comes from the vendor's Windows driver on this PC
    /// instead of the firmware folder (a Realtek chip, issue #86):
    /// firmware_files names the driver's file, nothing is to be done
    #[serde(default)]
    pub firmware_from_driver: bool,
    /// The vendor's Windows driver on this PC has this chip's firmware, also
    /// while Settings::realtek_custom_firmware loads the folder's instead
    #[serde(default)]
    pub firmware_driver: bool,
    /// What to tell about other vendors' firmware (firmware.vendor_intel,
    /// _broadcom, _csr, _unsupported); None for Realtek and unknown chips
    #[serde(default)]
    pub vendor_note: Option<Msg>,
    /// Windows has not loaded the driver yet: it needs the adapter
    /// replugged or Windows restarted (Device Manager's problem code 14, or
    /// the WinUSB assignment just asked for it)
    #[serde(default)]
    pub restart_needed: bool,
    /// The adapter has LE Audio's isochronous channels, as a stack running
    /// on it found out (remembered); None if none ran on it yet
    #[serde(default)]
    pub le_audio: Option<bool>,
    /// The adapter's own Bluetooth address, as a stack running on it or
    /// Windows' Bluetooth read it (remembered); None if neither ran on it yet
    #[serde(default)]
    pub address: Option<String>,
    /// Bluetooth core version the controller reports (HCI version: "5.3"),
    /// remembered; None if neither a stack nor Windows' Bluetooth ran on it
    /// yet
    #[serde(default)]
    pub bt_version: Option<String>,
    /// Device Manager's name for the radio while Windows' Bluetooth runs it
    /// ("Realtek Bluetooth 5.3 Adapter"), remembered for when BlueCaster has it
    /// on WinUSB; else the product name the device itself reports ("TP-Link
    /// UB500 Adapter"), not the driver's generic "WinUSB Device"; None if
    /// neither is known
    #[serde(default)]
    pub windows_name: Option<String>,
}

/// A Realtek firmware pair found in the settings folder
/// (adapter.firmware_files): the choices besides the chip table
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct FirmwareFile {
    /// "rtl8761bu" (the file name without "_fw.bin")
    pub stem: String,
    /// From the chip table, else the stem in capitals ("RTL8723BU")
    pub display_name: String,
    /// Realtek PID of the chip table entry it matches
    pub pid: Option<u16>,
    /// The matching *_config.bin is there too
    pub has_config: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct AdapterState {
    pub status: AdapterStatus,
    pub adapters: Vec<AdapterInfo>,
    pub error: Option<Msg>,
    /// The adapter BlueCaster uses has LE Audio's isochronous channels: the
    /// running stack's, else the first WinUSB adapter's remembered
    /// AdapterInfo::le_audio. None: not known yet (the UI then offers no LE
    /// Audio)
    #[serde(default)]
    pub le_audio: Option<bool>,
    /// The running stack's Realtek adapter came up on its ROM code: the
    /// firmware was not loaded, so it opens but cannot connect or stream
    /// (issue #81)
    #[serde(default)]
    pub rom_firmware: bool,
    /// No adapter on WinUSB, but a USB device Windows cannot read, after
    /// BlueCaster used an adapter: most likely that adapter with its chip
    /// hung, back only after a power cycle (issue #86)
    #[serde(default)]
    pub hung: bool,
    /// A radio Windows' own Bluetooth runs beside the adapter BlueCaster uses:
    /// the two may interfere (issue #86). None: no such radio
    #[ts(optional = nullable)]
    #[serde(default)]
    pub other_radio: Option<OtherRadio>,
    /// A firmware loader's progress while the adapter starts (Intel's
    /// download), then how it ended; None before any ran
    #[ts(optional = nullable)]
    #[serde(default)]
    pub loader: Option<LoaderStatus>,
    /// The USB adapter the running Bluetooth stack has open; None while no
    /// stack runs (before the first scan or connection) and over the
    /// emulator's TCP transport
    #[ts(optional = nullable)]
    #[serde(default)]
    pub active: Option<ActiveAdapter>,
}

/// The adapter BlueCaster uses (AdapterState::active): what its entry in
/// `adapters` tells, without the device's instance ID or address
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ActiveAdapter {
    /// As AdapterInfo::display_name: "RTL8761BU (0x2357:0x0604)"
    pub display_name: String,
    /// As AdapterInfo::windows_name: "TP-Link UB500 Adapter"
    #[ts(optional = nullable)]
    #[serde(default)]
    pub windows_name: Option<String>,
    pub vid: u16,
    pub pid: u16,
    /// The chip's vendor: "Realtek", "Intel", ..., "Unknown"
    pub vendor: String,
    /// The Realtek chip ("RTL8761BU"); None for other vendors and unknown
    /// chips
    #[ts(optional = nullable)]
    #[serde(default)]
    pub chip: Option<String>,
    /// As AdapterInfo::bt_version: "5.3"
    #[ts(optional = nullable)]
    #[serde(default)]
    pub bt_version: Option<String>,
}

/// Where a firmware loader is (AdapterState::loader)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct LoaderStatus {
    /// "Intel"
    pub vendor: String,
    /// The step running ("Secure Send"), or how it ended ("firmware
    /// running already", the error): technical, for developer mode
    pub step: String,
    /// Percent of the running step, when known
    #[ts(optional = nullable)]
    pub percent: Option<u8>,
    /// Finished: true when the adapter could start; None while it runs
    #[ts(optional = nullable)]
    pub ok: Option<bool>,
}

/// A radio of Windows' own Bluetooth
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct OtherRadio {
    /// Device Manager's name ("MediaTek Bluetooth Adapter")
    pub name: String,
    /// Windows has devices paired
    pub paired: bool,
}

/// A device known from the saved list or a scan
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct Device {
    /// "XX:XX:XX:XX:XX:XX"
    pub address: String,
    pub name: String,
    pub saved: bool,
    /// Audio class (headphones, speakers)
    pub audio: bool,
    /// From the last scan
    pub rssi: Option<i8>,
    /// Codec keys the device offered when it last connected since the core
    /// started ("ldac", "aac", ...); empty while not known
    #[serde(default)]
    pub codecs: Vec<String>,
    /// What its SBC endpoint offered then (the profile editor offers only
    /// those SBC settings); None while not known
    #[serde(default)]
    pub sbc: Option<SbcDeviceCaps>,
    /// What each codec it offered then lists (the codec settings offer
    /// only those sample rates, bit depths and bit rates, issue #86);
    /// empty while not known
    #[serde(default)]
    pub codec_caps: Vec<CodecDeviceCaps>,
    /// Codec keys the device may offer only once chosen, whatever `codecs`
    /// says: the UI keeps them choosable (issue #30)
    #[serde(default)]
    pub codecs_on_request: Vec<String>,
    /// An LE Audio entry (experimental; le_audio_devices.txt and the LE
    /// scan). A dual-mode device has two entries with the same address, a
    /// classic one and an LE Audio one: address and this flag identify an
    /// entry. The UI lists them only while Settings.le_audio_experimental
    /// is on.
    #[serde(default)]
    pub le_audio: bool,
    /// LE Audio: a member of a coordinated set (it advertised a CSIP RSI;
    /// both earbuds stream together)
    #[serde(default)]
    pub le_audio_set: bool,
    /// Keyboards and mice (experimental, issue #413): a Bluetooth LE
    /// keyboard or mouse with its address type for hid.add. Such entries
    /// are only in Snapshot.hid_candidates, never in the device list.
    #[serde(default)]
    pub hid_le: Option<u8>,
    /// LE Audio: the addresses of the other members of its coordinated set
    /// (the other earbud), once a connection learned it
    #[serde(default)]
    pub set_addresses: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    #[default]
    Idle,
    Connecting,
    Streaming,
    Reconnecting,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct StreamInfo {
    pub codec: String,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
    /// Bits per sample the codec carries (16, 24); 0: not reported (LE
    /// Audio, an older core)
    #[serde(default)]
    pub bits: u32,
    pub source_sample_rate: u32,
    pub source_channels: u32,
    pub source_bits: u32,
    /// The profile's codec, when the device did not offer it (fallback)
    pub requested_codec: Option<String>,
    pub requested_in_use: bool,
    /// Adaptive bit rate on (LDAC, LHDC V5, SBC and SBC XQ): LHDC V5 then
    /// stays at 400 kbps or below whatever the quality. bitrate_kbps (and
    /// SBC's details.sbc.bitpool, frame_bytes, frames_per_packet) follow
    /// it: the core sends a new StreamInfo while they change.
    #[serde(default)]
    pub abr: bool,
    /// The device's maximum bit rate lowered the profile's (LHDC V5)
    #[serde(default)]
    pub bitrate_capped: bool,
    /// OPPO earbuds turned Hi-Res on for this connection, after one
    /// reconnect for it
    #[serde(default)]
    pub hi_res_set: bool,
    /// LHDC V5 low latency is in the configuration in use (asked for by
    /// the profile, listed by the device; issue #543)
    #[serde(default)]
    pub low_latency: bool,
    /// Where the audio comes from
    #[serde(default)]
    pub source: Option<SourceInfo>,
    /// An LE Audio stream (codec "LC3"; bitrate_kbps, sample_rate and
    /// channels those of the LC3 configuration): what was set up. None for
    /// A2DP.
    #[serde(default)]
    pub le: Option<LeStreamInfo>,
    /// What an A2DP stream's codec runs with (Diagnostics); None for LE
    /// Audio and from an older core
    #[serde(default)]
    pub details: Option<Box<CodecDetails>>,
}

/// What an A2DP stream's codec runs with beyond the summary: the
/// configuration sent, what the device offered and the encoder's setup
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct CodecDetails {
    /// Bytes a packet may carry after its RTP header: media payload
    /// header and codec data (codecs without an RTP header use those 12
    /// bytes too; FastStream and aptX LL packets are shorter by design)
    pub mtu: u32,
    /// Packets start with an RTP header (not classic aptX, aptX LL,
    /// FastStream)
    pub rtp: bool,
    /// Bytes of the media payload header (frame count ...)
    pub media_header: u32,
    /// PCM frames one codec frame stands for
    pub frame_samples: u32,
    /// Bytes of every codec frame, when they all have the same length
    pub frame_bytes: Option<u32>,
    /// Codec frames per packet, when that follows from the frame length
    pub frames_per_packet: Option<u32>,
    pub sbc: Option<SbcDetails>,
    pub aac: Option<AacDetails>,
    /// Any other codec: what the device's endpoint of it lists (issue #86)
    #[serde(default)]
    pub device: Option<CodecDeviceCaps>,
}

/// SBC, SBC XQ and FastStream frames
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct SbcDetails {
    /// "mono", "dual_channel", "stereo", "joint_stereo"
    pub channel_mode: String,
    pub blocks: u32,
    pub subbands: u32,
    /// "loudness", "snr"
    pub allocation: String,
    /// The bitpool the encoder uses (under ABR: now; the core sends a new
    /// StreamInfo while it changes)
    pub bitpool: u32,
    /// The bitpool range of the configuration (SET_CONFIGURATION), min and
    /// max; None for FastStream (its codec information has none)
    pub config_bitpool: Option<(u32, u32)>,
    /// What the device's SBC endpoint offers, when it said
    pub device: Option<SbcDeviceCaps>,
    /// SBC / SBC XQ under ABR: the range the bitpool moves in (floor,
    /// ceiling); None without ABR and from an older core
    #[ts(optional = nullable)]
    #[serde(default)]
    pub abr_bitpool: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct SbcDeviceCaps {
    pub sample_rates: Vec<u32>,
    /// As SbcDetails.channel_mode
    pub channel_modes: Vec<String>,
    pub blocks: Vec<u32>,
    pub subbands: Vec<u32>,
    /// As SbcDetails.allocation
    pub allocations: Vec<String>,
    pub min_bitpool: u32,
    pub max_bitpool: u32,
}

/// What a device's endpoint of one codec lists (bluecaster_core::CodecDeviceCaps)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct CodecDeviceCaps {
    /// Codec key ("ldac", "aac", ...)
    pub codec: String,
    /// Sample rates it lists, lowest first (empty: not said)
    pub sample_rates: Vec<u32>,
    /// Bit depths it lists (empty: not said, or no choice)
    pub bit_depths: Vec<u32>,
    /// Its largest bit rate in bps (0 = not said)
    pub max_bitrate: u32,
    /// LHDC V5: it lists low latency (issue #543)
    #[serde(default)]
    pub lhdcv5_low_latency: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AacDetails {
    /// "MPEG-2 AAC-LC" or "MPEG-4 AAC-LC"
    pub object_type: String,
    /// Constant bit rate in bps (the encoder never uses VBR)
    pub bit_rate: u32,
    /// What the device's AAC endpoint offers, when it said
    pub device: Option<AacDeviceCaps>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AacDeviceCaps {
    /// By name ("MPEG-2 AAC-LC", "MPEG-4 HE-AAC", ...)
    pub object_types: Vec<String>,
    pub sample_rates: Vec<u32>,
    pub channels: Vec<u32>,
    /// Its peak bit rate in bps (0 = not said)
    pub max_bitrate: u32,
    pub vbr: bool,
}

/// The LE Audio stream that was set up (BAP unicast; v1 StreamPlan)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct LeStreamInfo {
    /// The BAP preset ("48_4_2"; GMAP low latency "48_4_gr")
    pub preset: String,
    /// LC3 frame duration: 7500 or 10000
    pub frame_us: u32,
    /// LC3 octets per channel and frame
    pub octets: u32,
    /// Connected isochronous streams
    pub cis: u32,
    /// Devices streamed to (2 for both earbuds of a coordinated set)
    pub devices: u32,
    /// The GMAP low latency (gaming) configuration
    pub low_latency: bool,
    /// English, v1's StreamPlan::describe ("LC3 48 kHz 10 ms 120 octets (96
    /// kbps/ch), 2 ch over 2 CIS, 48_4_2")
    pub summary: String,
}

/// The audio source of a stream
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct SourceInfo {
    /// "loopback" (what Windows plays), "virtual" (a virtual device),
    /// "output" (a chosen output device), "app" (one application), "tone"
    /// (the test tone)
    pub kind: String,
    /// The virtual or output device's name, or the application's executable; empty
    /// for loopback and the tone
    pub name: String,
    /// Application capture: the application is not running; the stream
    /// sends silence until it starts
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct StreamStatus {
    pub state: StreamState,
    pub message: Msg,
    /// Profile being streamed
    pub profile: Option<String>,
    pub info: Option<StreamInfo>,
    /// Headphone volume (AVRCP, 0..127), None while unknown
    pub volume: Option<u8>,
    /// Headphone battery level in percent, None while unknown (needs
    /// Settings.battery_experimental and headphones that report it)
    #[serde(default)]
    pub battery: Option<u8>,
    /// The headphones refused the connection that reports the battery
    /// (another device such as a phone may hold their hands-free profile):
    /// the battery shows as unknown
    #[serde(default)]
    pub battery_refused: bool,
    /// The voice codecs the headphones listed on their hands-free
    /// connection ("cvsd", "msbc", "lc3_swb"), for the Codecs screen's
    /// call codecs (issue #86); empty while unknown, cleared as the battery
    #[serde(default)]
    pub voice_codecs: Vec<String>,
    /// LE Audio: the microphone's sampling rates (Hz) the headphones allow,
    /// the one Auto takes first, for the Codecs screen; empty while unknown
    /// or without the microphone, cleared as `voice_codecs`
    #[serde(default)]
    pub mic_rates: Vec<u32>,
    /// The voice link (headset microphone): Some only while
    /// it carries audio, cleared on Idle / Error / Reconnecting like the
    /// battery
    #[serde(default)]
    #[ts(optional = nullable)]
    pub voice: Option<VoiceStatus>,
    /// An error state the codec caused (issue #86): the device's status
    /// line names it in a few words; the message has the whole text
    #[serde(default)]
    #[ts(optional = nullable)]
    pub codec_stop: Option<CodecStop>,
    /// The link carries less since the microphone was used (#86): the bit
    /// rate in kbps the codec is lowered to until the link is as before
    /// (info.bitrate_kbps stays the one asked for). Some only while
    /// streaming.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub slow_link: Option<u32>,
}

/// Why the stream stopped over its codec (StreamStatus.codec_stop)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CodecStop {
    /// Codec key ("ldac", "aac", ...)
    pub codec: String,
    pub kind: CodecStopKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodecStopKind {
    /// The device does not support the codec (or refused it, asked anyway)
    Unsupported,
    /// The device refused settings of the codec it does not list
    SettingsUnsupported,
    /// Another source uses the device's endpoint of the codec
    InUse,
    /// The device refused the codec's configuration
    Refused,
}

/// The headset microphone's voice link (StreamStatus.voice)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct VoiceStatus {
    /// "cvsd" | "msbc" | "lc3_swb" | "lc3"
    pub codec: String,
    /// "hfp" (classic: playback is mono call quality) | "le"
    pub link: String,
    /// Sampling rate of the voice link in Hz (8000 / 16000 / 32000 / 48000 ...)
    pub rate: u32,
    /// Microphone output device in use (id), empty while not rendering
    pub mic_device_id: String,
}

/// The microphone test (mic_test.*)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct MicTestState {
    /// "idle" | "starting" (the voice link opens) | "recording" |
    /// "recorded" | "playing"
    pub phase: String,
    /// Peak level 0..1 since the last answer: the microphone, or the
    /// recording while it plays
    pub level: f32,
    /// Length of the recording so far
    pub recorded_ms: u32,
    /// Longest recording
    pub max_ms: u32,
    /// Position of the playback
    pub played_ms: u32,
    /// Why the test stopped (a message key), None while fine
    pub error: Option<String>,
}

/// A headset mode as profile.set_headset_mic takes it (Profile.headset_mic)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HeadsetMicMode {
    Off,
    Auto,
    On,
}

impl HeadsetMicMode {
    /// Profile.headset_mic's value
    pub fn as_str(self) -> &'static str {
        match self {
            HeadsetMicMode::Off => "off",
            HeadsetMicMode::Auto => "auto",
            HeadsetMicMode::On => "on",
        }
    }
}

/// The kind of a playback EQ point
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub enum EqBandType {
    #[default]
    Peaking,
    LowShelf,
    HighShelf,
    HighPass,
    LowPass,
}

/// One playback EQ point: frequency (Hz, 20..20000), gain (dB, -24..24;
/// not used by the pass filters), Q (0.1..12; not used by the shelves)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
pub struct EqPoint {
    pub frequency: f32,
    pub gain: f32,
    pub q: f32,
    pub band_type: EqBandType,
}

/// A device's playback EQ, applied to the audio before it is encoded: on
/// or off, and up to 16 points (kept while off)
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct MediaEq {
    pub enabled: bool,
    pub bands: Vec<EqPoint>,
}

/// The Auracast matrix mixer (developer mode): outputs FL FR RL RR (rows)
/// from the source's speakers FL FR FC LFE BL BR SL SR (columns)
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct AuracastMix {
    /// The matrix from the levels below, else `matrix` as set
    pub auto: bool,
    pub center_db: f32,
    pub lfe_db: f32,
    pub surround_db: f32,
    pub normalize: bool,
    /// 4 rows of 8 gains (linear)
    pub matrix: Vec<Vec<f32>>,
    /// Per output (ms)
    pub delay_ms: Vec<f32>,
    /// Per output: polarity inverted
    pub invert: Vec<bool>,
}

/// A named mixer setting
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct AuracastMixPreset {
    pub name: String,
    pub mix: AuracastMix,
    /// One of the program's own (not saved, cannot be deleted)
    pub builtin: bool,
}

/// The mixer as the Auracast tab shows it
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
pub struct AuracastMixState {
    pub mix: AuracastMix,
    pub presets: Vec<AuracastMixPreset>,
    /// A broadcast runs with the mixer: changes are heard at once
    pub live: bool,
    /// The source's channels (of the broadcast running, else as asked)
    pub channels: u32,
    /// Which of the 8 columns the source has
    pub present: Vec<bool>,
    /// The broadcast has rear outputs (else only FL FR are heard)
    pub rear: bool,
    /// The matrix that plays (auto computed), 4 rows of 8
    pub effective: Vec<Vec<f32>>,
}

/// A connection profile, as in profiles.json
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]

pub struct Profile {
    pub name: String,
    pub device_address: String,
    pub device_name: String,
    /// auto, ldac, aptxhd, aptxll, aptx, aptxad, sbcxq, sbc, aac, lhdcv5, opus, opus05, faststream
    pub codec: String,
    /// hq, sq, mq; mhq for LHDC V5; ll for aptX Adaptive
    pub quality: String,
    pub abr: bool,
    /// LHDC V5: low latency asked for (issue #543)
    #[serde(default)]
    pub lhdcv5_low_latency: bool,
    pub codec_fallback: bool,
    /// The codec was chosen on its card while the device did not offer it:
    /// asked for anyway, no fallback (issue #86)
    #[serde(default)]
    pub codec_forced: bool,
    /// The codec's settings are not held to what the device lists (its
    /// card's "Show only supported options" off, issue #86)
    #[serde(default)]
    pub unlisted_options: bool,
    /// 0 = auto
    pub sample_rate: u32,
    /// 0 = auto, 16, 24
    pub bit_depth: u32,
    /// loopback, virtual, output, app. "output" (issue #193): the output
    /// device audio_device_id, whichever is the Windows default
    pub capture_mode: String,
    pub audio_device_id: String,
    pub audio_device_name: String,
    pub auto_switch_device: bool,
    pub app_exe: String,
    pub app_name: String,
    /// auto, off, device: kept as saved, but not used (issue #86): streams
    /// follow Settings.volume_sync alone.
    pub volume_sync: String,
    pub volume_device_id: String,
    pub volume_device_name: String,
    pub max_media_payload: u16,
    /// An LE Audio device (experimental, LC3): codec, quality, abr,
    /// sample_rate and bit_depth do not apply, capture_mode is "loopback"
    /// or "virtual" (error.le_audio_capture), and streaming needs
    /// Settings.le_audio_experimental (error.le_audio_disabled)
    #[serde(default)]
    pub le_audio: bool,
    /// LE Audio: GMAP low-latency presets with a gaming headset (LC3 LL; off
    /// by default: LC3); other headsets get the media configuration either way
    #[serde(default)]
    pub le_low_latency: bool,
    /// Headset mode: the headphones' microphone is
    /// rendered to mic_device_id. "off" (the default) | "on" | "auto";
    /// anything else reads as off. "on": classic devices switch playback to
    /// the voice link (mono call quality) for as long as they stream;
    /// "auto": only while an application records from mic_device_id's
    /// capture side (the cable's "CABLE Output"), full-quality A2DP
    /// otherwise. LE Audio keeps its media quality either way ("auto" is
    /// "on" there).
    #[serde(default = "headset_mic_off")]
    pub headset_mic: String,
    /// Render endpoint id that receives the microphone (a virtual cable's
    /// input). Empty = none chosen: headset mode stays off
    #[serde(default)]
    pub mic_device_id: String,
    /// Its friendly name, for display when the device is missing
    #[serde(default)]
    pub mic_device_name: String,
    /// The microphone muted (Home's and the flyout's chip): no voice link,
    /// as with headset_mic "off"; headset_mic is kept for unmuting
    #[serde(default)]
    pub mic_muted: bool,
    /// The microphone output's recording side is Windows' default input
    /// while the device is connected (on by default)
    #[serde(default = "yes")]
    pub mic_default_input: bool,
    /// Classic devices: the call codec chosen on its card with Auto-select
    /// off ("cvsd", "msbc", "lc3_swb"; "" = none chosen, the best both
    /// sides have). The voice link takes it, or the next lower one the
    /// headphones list. Kept but not used while Auto-select is on.
    #[serde(default)]
    pub voice_codec: String,
    /// LE Audio: the microphone's sampling rate chosen with Auto-select off
    /// (Hz; 0 = none chosen). One the headphones do not allow falls back
    /// to Auto's. Kept but not used while Auto-select is on.
    #[serde(default)]
    pub mic_rate: u32,
    /// The headset microphone's processing (all off by default); a change
    /// applies to the running microphone in place
    #[serde(default)]
    pub mic_processing: MicProcessing,
    /// SBC / SBC XQ: the bitpool to encode with, within what the device
    /// allows (0 = by quality)
    #[serde(default)]
    pub sbc_bitpool: u8,
    /// The same for SBC XQ
    #[serde(default)]
    pub sbcxq_bitpool: u8,
    /// SBC / SBC XQ: the lowest bitpool to use (the SET_CONFIGURATION's
    /// minimum and the floor of ABR), within what the device allows and
    /// never above the bitpool (0 = Auto)
    #[serde(default)]
    pub sbc_min_bitpool: u8,
    /// The same for SBC XQ
    #[serde(default)]
    pub sbcxq_min_bitpool: u8,
    /// SBC / SBC XQ: block length 4, 8, 12, 16 (0 = auto: 16). The frame
    /// setup counts in developer mode only
    #[serde(default)]
    pub sbc_blocks: u8,
    /// SBC / SBC XQ: subbands 4, 8 (0 = auto: 8)
    #[serde(default)]
    pub sbc_subbands: u8,
    /// SBC / SBC XQ: "loudness", "snr" ("" = auto: loudness)
    #[serde(default)]
    pub sbc_allocation: String,
    /// SBC: "joint_stereo", "stereo", "mono" ("" = auto: joint stereo; SBC
    /// XQ is always dual channel)
    #[serde(default)]
    pub sbc_channel_mode: String,
    /// The settings of the codecs the profile is not using, by codec key,
    /// as they were last chosen (the Codecs screen brings them back when
    /// that codec is chosen again)
    #[serde(default)]
    pub codec_settings: std::collections::BTreeMap<String, CodecValues>,
    /// The device's codec order (codec keys) as dragged on the Codecs
    /// screen: Auto, under Auto-select too, and a chosen codec the device
    /// does not offer, take the first one in it the device offers (issue
    /// #86 round 8). Empty: highest sound quality first.
    #[serde(default)]
    pub codec_order: Vec<String>,
    /// The playback EQ for this device: a change saved while it streams
    /// takes effect without reconnecting
    #[serde(default)]
    pub media_eq: MediaEq,
}

/// What is done to the headset microphone before apps get it
/// (Profile.mic_processing): gain, then noise suppression, then the noise
/// gate; a limiter holds the peaks while any of them is on. The default
/// does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
pub struct MicProcessing {
    /// The switch over all of it: off, the microphone passes untouched and
    /// the values below are kept for the next time
    pub enabled: bool,
    /// dB, -12 to 24
    pub gain_db: i8,
    /// Noise suppression: percent of the denoised signal (the rest is the
    /// original), 0 (off) to 100
    pub suppression: u8,
    /// The gate's threshold in dBFS, up to -20; -80 or lower: off
    pub gate_db: i8,
}

impl MicProcessing {
    /// The gate's threshold that means off
    pub const GATE_OFF_DB: i8 = -80;

    pub fn is_default(&self) -> bool {
        *self == MicProcessing::default()
    }

    /// Any value away from off (whether or not the switch is on)
    pub fn any_set(&self) -> bool {
        self.gain_db != 0 || self.suppression > 0 || self.gate_db > Self::GATE_OFF_DB
    }
}

impl Default for MicProcessing {
    fn default() -> Self {
        MicProcessing { enabled: false, gain_db: 0, suppression: 0, gate_db: Self::GATE_OFF_DB }
    }
}

/// Also reads the settings saved before noise suppression lost its own
/// switch (issue #86): switched off, the strength is 0; on, the strength
/// stays (100 when it was never set). Settings saved before the switch over
/// all of it are on when any value was set, as they worked then.
impl<'de> Deserialize<'de> for MicProcessing {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Saved {
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            gain_db: i8,
            #[serde(default)]
            noise_suppression: Option<bool>,
            #[serde(default)]
            suppression: Option<u8>,
            #[serde(default = "gate_off")]
            gate_db: i8,
        }
        fn gate_off() -> i8 {
            MicProcessing::GATE_OFF_DB
        }
        let s = Saved::deserialize(d)?;
        let suppression = match s.noise_suppression {
            Some(false) => 0,
            Some(true) => s.suppression.unwrap_or(100),
            None => s.suppression.unwrap_or(0),
        };
        let mut p = MicProcessing { enabled: false, gain_db: s.gain_db, suppression, gate_db: s.gate_db };
        p.enabled = s.enabled.unwrap_or_else(|| p.any_set());
        Ok(p)
    }
}

/// One codec's settings in a profile (Profile.codec_settings)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct CodecValues {
    pub quality: String,
    pub sample_rate: u32,
    pub bit_depth: u32,
    pub abr: bool,
    /// LHDC V5: low latency asked for (issue #543)
    #[serde(default)]
    pub lhdcv5_low_latency: bool,
    pub sbc_bitpool: u8,
    pub sbcxq_bitpool: u8,
    pub sbc_min_bitpool: u8,
    pub sbcxq_min_bitpool: u8,
    pub sbc_blocks: u8,
    pub sbc_subbands: u8,
    pub sbc_allocation: String,
    pub sbc_channel_mode: String,
    pub unlisted_options: bool,
}

impl Default for CodecValues {
    fn default() -> Self {
        CodecValues {
            quality: "hq".into(),
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

fn yes() -> bool {
    true
}

fn headset_mic_off() -> String {
    "off".into()
}

fn battery_low() -> u8 {
    20
}

/// Developer mode's overrides (codec::dev::DevOverrides)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct DevOverrides {
    pub rtp_marker: bool,
    pub packet_dump: bool,
    pub aac_config_bitrate0: bool,
    #[ts(type = "number")]
    pub aac_bitrate_kbps: u32,
    pub aac_no_peak: bool,
    pub aac_no_afterburner: bool,
    pub aac_latm_v0: bool,
    /// Auto-select uses each codec's own settings
    pub auto_codec_settings: bool,
    /// Rate and bit depth on Auto follow the captured output's format
    pub match_source: bool,
    /// Every codec ignores the maximum bit rate the device lists
    pub ignore_sink_max: bool,
    /// Longest a media packet waits in the send queue before it is dropped
    /// (ms, 50..=2000); 0 = the default (350). Applies to the next stream
    #[ts(type = "number")]
    pub queue_max_wait_ms: u32,
    /// Audio waits unencoded while the send queue is backed up (ABR then
    /// lowers its bit rate too). Applies to the next stream
    pub encode_at_send: bool,
    /// A link slow after a call keeps the full bit rate (not lowered)
    pub keep_bitrate_on_slow_link: bool,
    /// The microphone's Auto looks for recording apps on the session's
    /// tick, not with its own watcher
    pub auto_mic_tick_poll: bool,
}

/// Developer mode's override of one vendor's protocol
/// (bluecaster_core::vendor::Override)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct VendorOverride {
    pub mode: VendorMode,
    /// Its options turned on (VendorInfo.options)
    pub options: Vec<String>,
}

/// When earbuds are spoken to over their vendor's protocol: by their name,
/// always, or never
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum VendorMode {
    #[default]
    Auto,
    Always,
    Never,
}

/// A vendor whose earbuds BlueCaster speaks to over their own protocol
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct VendorInfo {
    /// Key of its overrides (Settings.dev_vendors)
    pub id: String,
    pub name: String,
    /// Developer mode's switches of its own (the UI labels them)
    pub options: Vec<String>,
}

/// Developer > Debug (bluecaster_core::settings::DevDebug)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct DevDebug {
    /// LE Audio on the MediaTek adapter's bulk pipe, not its interrupt
    /// interface
    pub mtk_iso_bulk: bool,
    /// Outgoing ISO packets zero-padded to this length; 0: the default
    pub mtk_iso_pad: u16,
    /// One HCI command after init, in hex ("FD98 08 01"); empty: none
    pub mtk_command: String,
    /// LE Audio SDUs queued per CIS on a MediaTek controller; 0: default
    pub mtk_iso_queue: u8,
    /// The command goes before HCI init instead of after it
    pub mtk_command_before_reset: bool,
    /// Register write after the firmware download, in hex ("40 02
    /// 12345678 9ABCDEF0": request type, request, register, value); empty:
    /// none
    pub mtk_register: String,
    /// Restore original driver hands Windows BlueCaster's LE keys too, for
    /// the devices Windows keeps LE keys of (issue #86)
    pub le_keys_to_windows: bool,
    /// LE Audio CIG: max transport latency C->P / P->C in ms; 0: as planned
    pub le_latency_c_to_p_ms: u16,
    pub le_latency_p_to_c_ms: u16,
    /// LE Audio CIG: RTN C->P / P->C; None: as planned
    #[ts(optional = nullable)]
    pub le_rtn_c_to_p: Option<u8>,
    #[ts(optional = nullable)]
    pub le_rtn_p_to_c: Option<u8>,
    /// LE Audio CIG: interleaved packing
    pub le_interleaved: bool,
    /// The LE Audio microphone's preset, taken as it is; empty: as planned
    pub le_mic_preset: String,
    /// A stream with the microphone that failed is retried without it on
    /// the same link
    pub le_mic_retry_same_link: bool,
}

/// Developer mode's logging (bluecaster_core::settings::DevLog), in force while
/// developer_mode is on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct DevLog {
    /// Daily log files kept (1..=365, 7 by default)
    #[ts(type = "number")]
    pub keep_days: u32,
    /// MB a day's log file takes before its debug and trace lines stop
    /// (10..=10240, 100 by default)
    #[ts(type = "number")]
    pub day_cap_mb: u32,
    /// Verbose logging without a time limit, from the core's start
    /// (debug or trace); None: off
    #[ts(optional = nullable)]
    pub always_verbose: Option<LogLevel>,
    /// The stream health line every 10 s for the whole stream
    pub health_every_interval: bool,
}

impl Default for DevLog {
    fn default() -> Self {
        DevLog { keep_days: 7, day_cap_mb: 100, always_verbose: None, health_every_interval: false }
    }
}

/// Application settings the UI edits
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct Settings {
    pub language: String,
    /// system, light, dark
    pub theme: String,
    pub start_with_windows: bool,
    pub start_minimized: bool,
    pub minimize_to_tray: bool,
    /// A click on the tray icon opens the main window, not the flyout
    #[serde(default)]
    pub tray_click_opens_window: bool,
    /// The flyout opens above Windows 11's Quick Settings, along with them,
    /// and a click on the tray icon opens the main window, not the flyout
    #[serde(default)]
    pub flyout_with_quick_settings: bool,
    /// A notification when a device connects: its codec, what the codec
    /// runs with, the battery (on by default)
    #[serde(default = "yes")]
    pub notify_connected: bool,
    /// The low-battery notification: below this level (percent), 0: never
    #[serde(default = "battery_low")]
    pub battery_low_percent: u8,
    /// The first-run setup was finished or skipped: the window opens it by
    /// itself only before that (a client that sends none: true)
    #[serde(default = "yes")]
    pub setup_done: bool,
    pub check_updates_on_startup: bool,
    pub include_prereleases: bool,
    /// Developer > alphas too, in force while developer_mode is on (an
    /// older core has none: off)
    #[serde(default)]
    pub include_alphas: bool,
    /// The headphones' buttons act as Windows media keys (every device)
    pub media_keys: bool,
    /// Connect a device that went away again by itself (on by default).
    /// Off: the session ends and the adapter is released.
    #[serde(default = "yes")]
    pub auto_reconnect: bool,
    /// The suggestion to install VB-CABLE was closed: Settings still offers it
    #[serde(default)]
    pub vb_cable_note_hidden: bool,
    /// The suggestion to get the Hi-Fi Cable was closed: Settings still
    /// offers it
    #[serde(default)]
    pub hifi_cable_note_hidden: bool,
    /// The virtual cables' dialog was opened by itself once after an update:
    /// it does not open by itself again (a client that sends none: true)
    #[serde(default = "yes")]
    pub cables_dialog_shown: bool,
    /// The warning before unlisted codec options was accepted: not asked again
    #[serde(default)]
    pub unlisted_options_warned: bool,
    /// Windows Volume Sync for every device (on by default): the audio
    /// device's volume and the captured device's Windows volume follow each
    /// other. Off: no stream syncs. A change starts the profile in use
    /// again.
    #[serde(default = "yes")]
    pub volume_sync: bool,
    /// The default output device is muted while streaming, so only the
    /// audio device plays (developer mode; off by default). A change
    /// starts the profile in use again.
    #[serde(default)]
    pub mute_output: bool,
    /// Developer > Volume Mixer (off by default): the mixer screen, the
    /// flyout's app list and mute_output take effect only with it on
    #[serde(default)]
    pub volume_mixer: bool,
    /// Developer > Show every audio device (off by default): the Output and
    /// Input Source lists offer every device, in force while developer_mode
    /// is on
    #[serde(default)]
    pub all_audio_devices: bool,
    /// Developer mode: receiver test, HCI capture, debug details
    pub developer_mode: bool,
    /// Developer mode: overrides of the codec and media path, in force
    /// while developer_mode is on (read when the next stream is set up)
    #[serde(default)]
    pub dev_overrides: DevOverrides,
    /// Developer mode: when each vendor's own protocol is used, and its
    /// options, by vendor id (Snapshot.vendors); absent: the default
    #[serde(default)]
    pub dev_vendors: std::collections::BTreeMap<String, VendorOverride>,
    /// Developer mode: log retention, the daily size cap, verbose logging
    /// that stays on, the stream health line every interval
    #[serde(default)]
    pub dev_log: DevLog,
    /// Developer > Debug: experiments for one adapter's transport
    #[serde(default)]
    pub dev_debug: DevDebug,
    /// 0 = auto, a Realtek PID, 0xFFFE = not Realtek
    pub bt_chip_pid: u16,
    pub bt_chip_fw_stem: String,
    /// A Realtek chip loads the firmware folder's files instead of Realtek's
    /// Windows driver's (AdapterInfo::firmware_driver)
    #[serde(default)]
    pub realtek_custom_firmware: bool,
    /// The adapter BlueCaster uses when several are on WinUSB: its
    /// AdapterInfo::instance_id; empty: automatic (the first one found).
    /// A change restarts Bluetooth (at once when idle).
    #[serde(default)]
    pub adapter_id: String,
    /// The adapter Auracast broadcasts go on (developer mode), an
    /// AdapterInfo::instance_id: Bluetooth restarts on it for a broadcast,
    /// and back on `adapter_id` for the next scan or connection; empty: the
    /// one everything else uses
    #[serde(default)]
    pub auracast_adapter_id: String,
    /// auto, off, or Wi-Fi channels such as "6,11"
    pub afh: String,
    pub remote_sink: String,
    /// LE Audio (experimental; off by default): LE Audio devices in the
    /// scan and LE Audio profiles. BTstack sets LE up when it starts: while
    /// it streams or scans the change waits for the next scan or
    /// connection (settings.le_audio_deferred).
    #[serde(default)]
    pub le_audio_experimental: bool,
    /// LE Audio: GMAP low latency for gaming headsets (read when the next
    /// LE Audio stream is set up); only with le_audio_experimental
    #[serde(default)]
    pub le_audio_low_latency: bool,
    /// Codecs > "Auto-select the codec with the highest quality", for every
    /// device (on by default): on, every A2DP profile streams as Auto; off,
    /// each with the codec chosen for it (read when the next stream starts)
    #[serde(default = "yes")]
    pub auto_codec: bool,
    /// Developer mode: keyboards and mice through BlueCaster (experimental,
    /// issue #413), in force while developer_mode is on. settings.set can
    /// only turn it off; hid.enable turns it on.
    #[serde(default)]
    pub hid_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct UpdateState {
    pub checking: bool,
    /// A newer release, if any
    pub available: Option<UpdateInfo>,
    pub current_version: String,
    /// The last check failed (update.error); None after one that worked
    #[serde(default)]
    pub error: Option<Msg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct UpdateInfo {
    pub version: String,
    /// The release page
    pub url: String,
    pub prerelease: bool,
    /// The updater manifest attached to the release (latest.json): the UI
    /// installs from it when it is an installed copy with an updater key
    pub manifest_url: String,
    /// A few lines on what is new, shown with the update (the release's
    /// in-app notes); empty when it has none
    #[serde(default)]
    pub notes: String,
}

/// The whole state
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]

pub struct Snapshot {
    pub adapter: AdapterState,
    pub devices: Vec<Device>,
    pub scanning: bool,
    /// Only the scan's LE Audio part runs (status.le_scanning): after a
    /// classic inquiry shorter than the LE scan, with LE Audio on
    #[serde(default)]
    pub le_scanning: bool,
    pub stream: StreamStatus,
    pub profiles: Vec<Profile>,
    pub active_profile: Option<String>,
    pub settings: Settings,
    pub update: UpdateState,
    /// The settings folder (shown, and opened from the UI)
    pub config_dir: String,
    /// The folder of the core's daily log files ("open logs")
    #[serde(default)]
    pub log_dir: String,
    /// A running HCI capture (diag.capture_start)
    #[serde(default)]
    pub capture: Option<CaptureState>,
    /// The core's local time of day (ms since local midnight) at time_ms 0:
    /// a line's time of day is (clock_ms + time_ms) % 86 400 000. None if
    /// the core does not know it.
    #[serde(default)]
    #[ts(optional = nullable, type = "number | null")]
    pub clock_ms: Option<u64>,
    /// A2DPWB v1 runs (its single-instance mutex is held): it may hold the
    /// Bluetooth adapter, so the UI asks to exit it
    #[serde(default)]
    pub v1_running: bool,
    /// Verbose logging is on (diag.set_verbose): until when, at what level
    #[serde(default)]
    #[ts(optional = nullable)]
    pub verbose: Option<VerboseState>,
    /// Crash reports of BlueCaster's own processes the user has not dismissed
    /// yet (diag.crashes_seen), newest first
    #[serde(default)]
    pub crashes: Vec<CrashReport>,
    /// Counts the changes to Windows' audio devices since the core started
    /// (one came, went, or a default changed): the UI reads its lists again
    #[serde(default)]
    pub audio_endpoints: u32,
    /// The vendors with protocols of their own (developer mode's overrides)
    #[serde(default)]
    pub vendors: Vec<VendorInfo>,
    /// The keyboards and mice added for the HID host (experimental)
    #[serde(default)]
    pub hid_devices: Vec<HidDevice>,
    /// The Bluetooth LE keyboards and mice the last scan found (HID host
    /// on): kept apart from `devices`, which the audio screens use
    #[serde(default)]
    pub hid_candidates: Vec<Device>,
    /// The number the user types on the keyboard being added (hid.add),
    /// while its pairing shows one
    #[serde(default)]
    pub hid_passkey: Option<HidPasskey>,
    /// The keyboards and mice being added: each stays only once its input
    /// reaches Windows
    #[serde(default)]
    pub hid_checks: Vec<HidCheck>,
    /// The keyboard or mouse removed again last because no input arrived,
    /// until the user adds one again or dismisses the notice
    #[serde(default)]
    pub hid_rolled_back: Option<HidRolledBack>,
    /// The last self-test of the path that passes keyboard and mouse input
    /// to Windows; None: none ran yet
    #[serde(default)]
    pub hid_health: Option<HidHealth>,
    /// The core runs as a Windows service: its data folder is protected.
    /// `config_dir` and `log_dir` are empty, results name files in the
    /// user's folder (handed over through the session agent), and
    /// `app.quit` is refused (`app.close` ends the user's session of the
    /// app instead).
    #[serde(default)]
    pub service_mode: bool,
}

/// A crash report (logs\crash\<process>-<stamp>.txt): a panic or a crash
/// of bluecaster-core.exe or bluecaster.exe
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct CrashReport {
    /// "bluecaster-core-20261022-012338-512": the file without .txt
    pub name: String,
    /// "bluecaster-core" or "bluecaster-ui"
    pub process: String,
    /// Local time, "2026-10-22 01:23:38"
    pub time: String,
    /// "panic in thread encode: ...", "unhandled exception 0xC0000005 at ..."
    pub reason: String,
    /// The whole report (the version, the reason, the backtrace), cut to
    /// a size that fits a bug report
    pub text: String,
}

/// Verbose logging (diag.set_verbose): BlueCaster's own modules log at `level`
/// until `until_ms`, then back to info by themselves
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct VerboseState {
    pub level: LogLevel,
    /// Milliseconds since 1970 (UTC)
    #[ts(type = "number")]
    pub until_ms: u64,
}

/// A chip probe's report (adapter.probe)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ProbeResult {
    /// The JSON report in logs\probe
    pub path: String,
    /// "Intel, Bluetooth 5.3 (HCI 12), LMP subversion 0x3102, 8 of 8
    /// commands answered"
    pub summary: String,
    /// The same report on one line, for a chip report issue
    pub report: String,
}

/// A chip script checked before it runs (adapter.check_script)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ScriptCheck {
    /// Lines that cannot be read ("line 3: ..."); empty: it can run
    pub errors: Vec<String>,
    /// Lines not known to only read ("line 2: hci fc05 ff"): the script runs
    /// only with allow_writes
    pub writes: Vec<String>,
    /// Lines the adapter's vendor never takes from a script
    pub denied: Vec<String>,
}

/// A chip script's run (adapter.run_script)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ScriptResult {
    /// The JSON report in logs\probe
    pub path: String,
    /// "9 of 9 lines done" or where it stopped
    pub summary: String,
    /// One per line run: "line 1: hci 1001  3 ms  0e0c..."
    pub lines: Vec<String>,
    /// Events that answered no line, in hex
    pub other_events: Vec<String>,
}

/// A diagnostic bundle (diag.bundle)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct DiagBundle {
    /// The .zip file
    pub path: String,
    #[ts(type = "number")]
    pub bytes: u64,
    /// Files in it
    pub files: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct DiagBundleParams {
    /// Bluetooth addresses (<addr-N>), device names (<device-N>), USB serial
    /// numbers and the Windows user name masked in the text files (default
    /// true; HCI captures keep the addresses)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub mask: Option<bool>,
    /// Include the HCI captures (default true)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub hci: Option<bool>,
}

/// A running HCI capture
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct CaptureState {
    /// The .pklg file
    pub path: String,
    /// Written so far (refreshed every few seconds while it runs)
    #[ts(type = "number")]
    pub bytes: u64,
    /// When it started: milliseconds since 1970 (UTC)
    #[ts(type = "number")]
    pub started_ms: u64,
}

/// Link quality, 4 times a second while subscribed
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct LinkSample {
    /// Milliseconds since the core started
    #[ts(type = "number")]
    pub time_ms: u64,
    pub rssi: Option<i8>,
    pub afh_in_use: Option<u32>,
    pub afh_usable: Option<u32>,
    pub afh_avoided: String,
    /// Media bytes sent per second (RTP and headers included)
    pub bytes_per_second: f64,
    pub packets_per_second: f64,
    pub queue_depth: u32,
    /// Packets lost before the radio since the stream started
    #[ts(type = "number")]
    pub dropped: u64,
    /// Captured frames dropped because the encoder fell behind
    #[ts(type = "number")]
    pub capture_dropped: u64,
    /// Share of the link's air time the stream takes, reply slots included
    /// (v1's link quality "air time")
    pub airtime: f64,
    // Optional: a core before these fields sends none
    /// Media packet size over the last interval (L2CAP payload: RTP and
    /// media headers included)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub avg_packet_bytes: Option<f64>,
    /// The ACL packet type that size takes: on the fastest packet types the
    /// remote device's features allow, on an EDR 3 Mbps link while they are
    /// not known (AclPacket.from_features); None while nothing is sent
    #[ts(optional = nullable)]
    #[serde(default)]
    pub acl_packet: Option<AclPacket>,
    /// Share of the link's usable rate the stream takes, L2CAP headers
    /// included: the largest packet the remote device allows and its reply
    /// slot (EDR 3 Mbps: 2178 kbps, 1021 bytes every 6 slots)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub bandwidth_share: Option<f64>,
    /// Packets the send queue holds (queue_depth of this)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub queue_capacity: Option<u32>,
    /// How long the oldest packet in the send queue has waited (ms)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub queue_ms: Option<u32>,
    /// The playout delay the device reported (AVDTP Delay Reporting), in
    /// ms: from a packet's arrival until it is heard. None while the
    /// device has not reported one (not every device does)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub sink_delay_ms: Option<f64>,
    /// The encoder's own time over the last interval (A2DP or LE Audio).
    /// None while nothing was encoded.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub encode: Option<Box<EncodeLoad>>,
    /// Capture rate of the stream (frames per second; 0 while not
    /// streaming): capture drops in ms = frames * 1000 / this
    #[ts(optional = nullable)]
    #[serde(default)]
    pub source_rate: Option<u32>,
    /// The cumulative counters. Since the stream started: total minus
    /// stream_start. "Since reset" (v1's Reset button): the view keeps the
    /// counters of the sample at its reset and subtracts them; the core
    /// keeps no baseline per client.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub counters: Option<LinkCounters>,
    /// An LE Audio stream's isochronous channels (None for A2DP; the A2DP
    /// fields above stay at 0 / None then)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub iso: Option<IsoCounters>,
    /// The most ACL packets of the A2DP link the controller held over the
    /// last interval: handed to it and not yet sent. Audio waits there
    /// after the send queue (queue_ms). For everyone. None while not
    /// streaming A2DP.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub controller_backlog: Option<u32>,
    /// controller_backlog in ms of audio, at the interval's ACL packet rate;
    /// None while nothing is sent
    #[ts(optional = nullable)]
    #[serde(default)]
    pub controller_backlog_ms: Option<f64>,
    /// The connected device's Bluetooth chip. For everyone. None without
    /// an A2DP link or before its controller answered.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub remote_chip: Option<Box<RemoteChip>>,
    /// The A2DP link's state and readings: developer mode only. None
    /// without an A2DP link.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub link_details: Option<Box<LinkDetails>>,
    /// Gaps Windows reported in the captured audio since the stream started
    /// (its own glitches: neither the encoder falling behind, which is
    /// capture_dropped, nor the radio)
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub capture_discontinuities: Option<u64>,
    /// Captured frames lost in those gaps, where the capture could tell (at
    /// source_rate, like capture_dropped)
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub capture_lost: Option<u64>,
    /// The channels the link hops over: 10 bytes, bit n = Bluetooth
    /// channel n (2402 + n MHz). Null while there is no reading.
    #[ts(optional = nullable)]
    #[serde(default)]
    pub afh_map: Option<Vec<u8>>,
    /// A classic device plays over its call link (the headset microphone
    /// is open): A2DP is suspended, and the media figures above are the
    /// call link's (SCO payloads sent, the uplink capture). None: not on a
    /// call link
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub voice: Option<bool>,
    /// The call link's way back (the microphone), while on it
    #[ts(optional = nullable)]
    #[serde(default)]
    pub voice_rx: Option<VoiceRxCounters>,
}

/// The connected device's Bluetooth chip, as its controller reports it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct RemoteChip {
    /// Bluetooth SIG company identifier of the chip's maker
    #[ts(optional = nullable)]
    pub company_id: Option<u16>,
    /// Its name ("Airoha"), or the ID in hex ("0x1234") when not listed
    #[ts(optional = nullable)]
    pub company: Option<String>,
    /// Bluetooth core version of the chip ("5.3"); None for an LMP version
    /// the table does not know
    #[ts(optional = nullable)]
    pub version: Option<String>,
    #[ts(optional = nullable)]
    pub lmp_version: Option<u8>,
    /// The maker's firmware / chip revision number
    #[ts(optional = nullable)]
    pub lmp_subversion: Option<u16>,
    /// What the chip supports; None before its features were read
    #[ts(optional = nullable)]
    pub features: Option<RemoteChipFeatures>,
}

/// Packet types the remote chip supports
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct RemoteChipFeatures {
    /// EDR 2 Mbps / 3 Mbps
    pub edr_2m: bool,
    pub edr_3m: bool,
    /// Basic rate 3- / 5-slot packets
    pub three_slot: bool,
    pub five_slot: bool,
    /// EDR 3- / 5-slot packets
    pub edr_three_slot: bool,
    pub edr_five_slot: bool,
    /// The largest packet type these allow ("3-DH5", "2-DH5", "DH3")
    pub largest_packet: String,
}

/// Link role of this adapter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    Central,
    Peripheral,
}

/// Power mode of the link
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LinkMode {
    Active,
    Hold,
    Sniff,
    Park,
}

/// The A2DP link's state and readings (developer mode). None: not known,
/// or the controller does not support reading it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]

pub struct LinkDetails {
    #[ts(optional = nullable)]
    pub role: Option<LinkRole>,
    /// Role changes since the stream started
    pub role_changes: u32,
    #[ts(optional = nullable)]
    pub mode: Option<LinkMode>,
    /// While in sniff mode
    #[ts(optional = nullable)]
    pub sniff_interval_ms: Option<f64>,
    /// Times the link went into sniff mode since the stream started
    pub sniff_entries: u32,
    /// Encryption key size in bytes (16 is the most)
    #[ts(optional = nullable)]
    pub encryption_key_size: Option<u8>,
    /// How long without a packet from the device before the link counts
    /// as lost
    #[ts(optional = nullable)]
    pub supervision_timeout_ms: Option<f64>,
    /// The controller's failed contact counter as last read: packets in a
    /// row it gave up on, back to 0 when one gets through
    #[ts(optional = nullable)]
    pub failed_contacts: Option<u16>,
    /// Rises of that counter since the stream started (None while it is not
    /// read)
    #[ts(optional = nullable, type = "number | null")]
    pub failed_contacts_stream: Option<u64>,
    /// Rises a second between its last two readings
    #[ts(optional = nullable)]
    pub failed_contacts_per_second: Option<f64>,
    /// Link quality, 0..255: chip specific, higher is better
    #[ts(optional = nullable)]
    pub link_quality: Option<u8>,
}

/// The encoder's time over one link sample's interval
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct EncodeLoad {
    /// Time in the encoder per time of audio encoded (0.01 = 1 %)
    pub load: f64,
    /// Average and longest encoder call (one codec frame; LE Audio: one
    /// SDU set), in microseconds
    pub avg_us: f64,
    pub max_us: f64,
}

/// The headset microphone's packets over a classic call link (cumulative
/// since the core started; the UI takes differences)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct VoiceRxCounters {
    /// SCO payloads received
    #[ts(type = "number")]
    pub packets: u64,
    /// Of them, flagged by the controller as damaged on the air
    #[ts(type = "number")]
    pub bad: u64,
    /// Codec frames concealed (made up from the ones around them)
    #[ts(type = "number")]
    pub concealed: u64,
}

/// An LE Audio stream's send counters (since the stream was set up; a
/// re-setup after a loss starts them again)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct IsoCounters {
    /// Connected isochronous streams
    pub cis: u32,
    /// SDU sets sent (one SDU per CIS and frame interval), silence included
    #[ts(type = "number")]
    pub sdu_sets: u64,
    /// Sets of silence sent because no audio was buffered
    #[ts(type = "number")]
    pub silence: u64,
    /// Times the jitter buffer ran empty
    #[ts(type = "number")]
    pub underruns: u64,
    /// Sets dropped because the jitter buffer was full
    #[ts(type = "number")]
    pub dropped: u64,
    /// The jitter buffer's prefill (frames buffered before sending starts;
    /// 3, growing to 7 after underruns)
    pub prefill: u32,
}

/// An ACL packet type and the slots one media packet takes in it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AclPacket {
    /// "3-DH1", "3-DH3", "3-DH5" ("2-DH5", "DH5" on a device without EDR
    /// 3 Mbps)
    pub name: String,
    /// Slots per media packet with the reply slot (several 3-DH5 packets
    /// for one larger than 1021 bytes)
    pub slots: u32,
    /// From the remote device's features; false: EDR 3 Mbps assumed
    #[ts(optional = nullable)]
    #[serde(default)]
    pub from_features: Option<bool>,
}

/// Media counters of the core (cumulative, never reset)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct LinkTotals {
    /// Media packets handed to L2CAP, and their bytes
    #[ts(type = "number")]
    pub packets: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    /// Packets lost before the radio (queue full, L2CAP refused)
    #[ts(type = "number")]
    pub dropped: u64,
    /// Captured frames, and those dropped because the encoder fell behind
    #[ts(type = "number")]
    pub capture_frames: u64,
    #[ts(type = "number")]
    pub capture_dropped: u64,
    /// Gaps Windows reported in the captured audio (its own glitches), and
    /// the frames lost in them where the capture could tell (at the capture
    /// rate, like capture_dropped). 0 from a core before these.
    #[ts(type = "number")]
    #[serde(default)]
    pub capture_discontinuities: u64,
    #[ts(type = "number")]
    #[serde(default)]
    pub capture_lost: u64,
}

impl LinkTotals {
    /// `self` since `base` (0 where `base` is larger)
    pub fn since(&self, base: &LinkTotals) -> LinkTotals {
        LinkTotals {
            packets: self.packets.saturating_sub(base.packets),
            bytes: self.bytes.saturating_sub(base.bytes),
            dropped: self.dropped.saturating_sub(base.dropped),
            capture_frames: self.capture_frames.saturating_sub(base.capture_frames),
            capture_dropped: self.capture_dropped.saturating_sub(base.capture_dropped),
            capture_discontinuities: self.capture_discontinuities.saturating_sub(base.capture_discontinuities),
            capture_lost: self.capture_lost.saturating_sub(base.capture_lost),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct LinkCounters {
    /// Since the core started
    pub total: LinkTotals,
    /// `total` when the current (or last) stream started
    pub stream_start: LinkTotals,
    /// Streams started since the core started (0: none yet); a change means
    /// a new stream_start
    pub stream_starts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct LogLine {
    #[ts(type = "number")]
    pub time_ms: u64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    /// 1, 2, ... for every line since the core started: FlowStepView.seq
    /// names the line that set a step, FlowView.first_seq an attempt's first
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub seq: Option<u64>,
    /// The connection attempt (FlowView.session) the line belongs to; 0
    /// before the first
    #[ts(optional = nullable)]
    #[serde(default)]
    pub session: Option<u32>,
}

/// A headphone button (Event::MediaButton)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MediaButton {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    FastForward,
    Rewind,
    VolumeUp,
    VolumeDown,
    Mute,
}

/// A headphone button was pressed or released (Event::MediaButton)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ButtonPress {
    pub button: MediaButton,
    /// False: released. LE Audio headphones send only the press.
    pub pressed: bool,
    /// The button came over LE Audio (media control), not Classic (AVRCP)
    pub le_audio: bool,
}

/// One part of the state changed, or a subscribed stream produced data
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Event {
    Adapter(AdapterState),
    Devices(Vec<Device>),
    Scanning(bool),
    /// The scan's LE Audio part started or ended (state, le_scanning)
    LeScanning(bool),
    Stream(StreamStatus),
    Profiles {
        profiles: Vec<Profile>,
        active_profile: Option<String>,
    },
    Settings(Settings),
    Update(UpdateState),
    /// Subscription (diag.subscribe link)
    Link(LinkSample),
    /// Subscription (diag.subscribe logs)
    Log(LogLine),
    /// The HCI capture started, stopped, or wrote more (state)
    Capture(Option<CaptureState>),
    /// A2DPWB v1 started or exited (state, v1_running)
    V1Running(bool),
    /// Verbose logging turned on, off, or ran out (state, verbose)
    Verbose(Option<VerboseState>),
    /// Crash reports appeared or were dismissed (state, crashes)
    Crashes(Vec<CrashReport>),
    /// Windows' audio devices changed (state, audio_endpoints)
    AudioEndpoints(u32),
    /// Subscription (diag.subscribe receiver): the receiver test, once a
    /// second and on every change
    Receiver(Box<ReceiverState>),
    /// The QCY earbuds' state (the Fones screen, read only): the QcyState
    /// of app/src/ui/src/ipc/qcy.ts, after every qcy.refresh / qcy.reconnect
    Qcy(serde_json::Value),
    /// Data an RFCOMM channel received (to the connection that opened it
    /// only, not part of the state)
    RfcommData(RfcommData),
    /// An RFCOMM channel closed without its connection's rfcomm.close (to
    /// that connection only); its last event
    RfcommClosed(RfcommClosed),
    /// A headphone button arrived (to every connection, not part of the
    /// state), also with the "media keys" setting off
    MediaButton(ButtonPress),
    /// A keyboard or mouse was added, removed, connected or went away
    /// (state, hid_devices)
    HidDevices(Vec<HidDevice>),
    /// The Bluetooth LE keyboards and mice a scan found (state,
    /// hid_candidates)
    HidCandidates(Vec<Device>),
    /// Pairing the keyboard being added shows a number to type on it, or
    /// no longer does (state, hid_passkey)
    HidPasskey(Option<HidPasskey>),
    /// A keyboard or mouse being added started or ended its countdown, or
    /// was removed again (state, hid_checks and hid_rolled_back)
    HidCheck {
        checks: Vec<HidCheck>,
        rolled_back: Option<HidRolledBack>,
    },
    /// The self-test has a new result (state, hid_health)
    HidHealth(Option<HidHealth>),
    /// The core is shutting down
    Bye,
    /// Service mode: the user chose Exit (app.close, the tray). The UI
    /// closes; the core keeps running. Not state, no revision.
    UiClose,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// A request and its parameters. Results (in [`ServerMessage::Response`])
/// are noted per method; `null` where none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    /// -> Snapshot
    #[serde(rename = "state.snapshot")]
    StateSnapshot,
    /// Streams this profile (stopping one that runs)
    #[serde(rename = "stream.start")]
    StreamStart { profile: String },
    #[serde(rename = "stream.stop")]
    StreamStop,
    /// Streams without a saved profile (receiver test, the setup's test
    /// tone); the active profile stays. `codec`, `quality`, `sample_rate`,
    /// `bit_depth`, `max_media_payload` as in a Profile (0 / None: auto);
    /// `source`: "tone", "loopback", "virtual" (`source_id`: the device's
    /// id) or "app" (`source_id`: the executable). Volume sync is off;
    /// codec fallback only when asked.
    #[serde(rename = "stream.start_custom")]
    StreamStartCustom {
        device: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        device_name: Option<String>,
        codec: String,
        quality: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        sample_rate: Option<u32>,
        #[ts(optional = nullable)]
        #[serde(default)]
        bit_depth: Option<u32>,
        source: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        source_id: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default)]
        source_name: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default)]
        max_media_payload: Option<u16>,
        #[ts(optional = nullable)]
        #[serde(default)]
        abr: Option<bool>,
        /// LHDC V5 low latency, as a profile's (issue #543)
        #[ts(optional = nullable)]
        #[serde(default)]
        lhdcv5_low_latency: Option<bool>,
        #[ts(optional = nullable)]
        #[serde(default)]
        codec_fallback: Option<bool>,
    },
    /// Developer mode: broadcasts (Auracast source) for any LE Audio
    /// receiver nearby, instead of streaming to a device; the active
    /// profile stays. `name`: the Broadcast Name (4-32 bytes); `code`: the
    /// Broadcast Code (4-16 bytes; none or empty: open); `quality`: "hq"
    /// (48 kHz) or "sq" (24 kHz); `mono`: one channel instead of left and
    /// right; `layout` (not with `mono`): "stereo" (default), "mono",
    /// "split" (left and right as two broadcasts of their own, "<name> L" /
    /// "<name> R", one mono BIS each) or "matrix_<front>_<rear>" (the
    /// matrix mixer's outputs: front FL FR and rear RL RR each "stereo",
    /// "separate", "subgroups" or "off"; older names quad, front_rear,
    /// pairs, rear_subgroups, subgroups), unknown: error stream.bad_layout;
    /// `split`: true is layout "split" (when no `layout` is given);
    /// `source` / `source_id` / `source_name` as for stream.start_custom
    /// ("app" fails with error.le_audio_capture). Error developer.off while
    /// developer mode is off; a name or code out of range fails the stream
    /// (error.auracast).
    #[serde(rename = "stream.start_broadcast")]
    StreamStartBroadcast {
        name: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        code: Option<String>,
        quality: String,
        /// A matrix layout's rear (RL RR): "hq" or "sq"; None: `quality`
        #[ts(optional = nullable)]
        #[serde(default)]
        rear_quality: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default)]
        mono: Option<bool>,
        #[ts(optional = nullable)]
        #[serde(default)]
        split: Option<bool>,
        #[ts(optional = nullable)]
        #[serde(default)]
        layout: Option<String>,
        source: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        source_id: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default)]
        source_name: Option<String>,
    },
    /// AVRCP absolute volume 0..127
    #[serde(rename = "stream.set_volume")]
    StreamSetVolume { volume: u8 },
    /// Moves the headphone volume by `delta` (-127..127, within 0..127)
    /// from the one the headphones last reported (StreamStatus.volume), as
    /// stream.set_volume -> number: the volume asked for. Errors
    /// stream.volume_unknown (no stream, or no volume reported yet),
    /// stream.bad_volume_step (`delta` out of range). 0: nothing is sent.
    #[serde(rename = "stream.volume_step")]
    StreamVolumeStep { delta: i16 },
    /// -> Device[] (also as an event). `seconds` from 3 to
    /// [`MAX_SCAN_SECONDS`] (the setup looks until a device is added)
    #[serde(rename = "device.scan")]
    DeviceScan { seconds: u32 },
    /// Ends a running scan early: device.scan answers with what it found
    #[serde(rename = "device.scan_stop")]
    DeviceScanStop,
    /// -> string[]: the addresses ("XX:XX:XX:XX:XX:XX") of the devices
    /// paired in Windows' own Bluetooth, on any of its radios (adding one
    /// here may conflict with it, issue #86)
    #[serde(rename = "device.windows_paired")]
    DeviceWindowsPaired,
    /// -> DevicePairing: what BlueCaster keeps of a classic device's pairing
    /// (developer mode only)
    #[serde(rename = "device.pairing")]
    DevicePairing { address: String },
    /// -> ServiceList: the services a classic device lists, asked over the
    /// connection BlueCaster has to it, else the list from its last
    /// connection; `le_audio` true: the GATT services its LE Audio entry had
    /// at its last connection
    #[serde(rename = "device.services")]
    DeviceServices {
        address: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        le_audio: Option<bool>,
    },
    /// -> WindowsPairedDevice[]: the classic audio devices Windows'
    /// Bluetooth paired through the adapter with the Bluetooth address
    /// `adapter` (what device.import_windows_pairings can take over; no
    /// administrator prompt, no running radio needed)
    #[serde(rename = "device.windows_paired_through")]
    DeviceWindowsPairedThrough { adapter: String },
    /// -> WindowsImport: takes over the link keys Windows' Bluetooth keeps
    /// for the adapter with the Bluetooth address `adapter`, so the devices
    /// it paired with that adapter connect without pairing again, and saves
    /// the audio ones (an administrator prompt); `addresses`: only those
    /// devices' (the ones ticked in the setup's list, issue #86)
    #[serde(rename = "device.import_windows_pairings")]
    DeviceImportWindowsPairings {
        adapter: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        addresses: Option<Vec<String>>,
    },
    /// -> KeysComparison (developer mode): BlueCaster's pairing keys against
    /// the ones Windows keeps for the adapter with the Bluetooth address
    /// `adapter` (one administrator prompt; numbers only)
    #[serde(rename = "dev.compare_windows_keys")]
    DevCompareWindowsKeys { adapter: String },
    /// Removes a saved device; `le_audio` true: its LE Audio entry (absent
    /// or false: the classic one)
    #[serde(rename = "device.forget")]
    DeviceForget {
        address: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        le_audio: Option<bool>,
    },
    /// Renames a device (it is saved with that name); `le_audio` as in
    /// device.forget
    #[serde(rename = "device.rename")]
    DeviceRename {
        address: String,
        name: String,
        #[ts(optional = nullable)]
        #[serde(default)]
        le_audio: Option<bool>,
    },
    /// Forgets what a classic device offered (and that OPPO earbuds were
    /// asked for Hi-Res): its next connection reads it again
    #[serde(rename = "device.forget_codecs")]
    DeviceForgetCodecs { address: String },
    /// Adds or replaces a profile (`original_name`: the one renamed)
    #[serde(rename = "profile.save")]
    ProfileSave { profile: Box<Profile>, original_name: Option<String> },
    #[serde(rename = "profile.delete")]
    ProfileDelete { name: String },
    /// Plays the stream with this EQ while it streams the profile `name`,
    /// without saving it (while a point is dragged; profile.save keeps it).
    /// Nothing happens for another profile or no stream.
    #[serde(rename = "media_eq.preview")]
    MediaEqPreview { name: String, eq: MediaEq },
    #[serde(rename = "profile.set_active")]
    ProfileSetActive { name: String },
    /// The auracast.* methods: error developer.off while developer mode is
    /// off.
    /// -> AuracastMixState: for the broadcast running, else a source of
    /// `channels` (default 8) and rear outputs `rear` (default true)
    #[serde(rename = "auracast.mix")]
    AuracastMixGet {
        #[ts(optional = nullable)]
        #[serde(default)]
        channels: Option<u32>,
        #[ts(optional = nullable)]
        #[serde(default)]
        rear: Option<bool>,
    },
    /// -> AuracastMixState: saved, and heard at once while broadcasting
    #[serde(rename = "auracast.mix_set")]
    AuracastMixSet {
        mix: AuracastMix,
        #[ts(optional = nullable)]
        #[serde(default)]
        channels: Option<u32>,
        #[ts(optional = nullable)]
        #[serde(default)]
        rear: Option<bool>,
    },
    /// -> AuracastMixState; a built-in preset's name is refused
    /// (auracast.mix_save_failed)
    #[serde(rename = "auracast.mix_preset_save")]
    AuracastMixPresetSave { name: String, mix: AuracastMix },
    /// -> AuracastMixState
    #[serde(rename = "auracast.mix_preset_delete")]
    AuracastMixPresetDelete { name: String },
    /// -> bool: the running broadcast takes "tone" or "loopback" (system
    /// audio) in place, its broadcasts staying up; false: none runs
    #[serde(rename = "auracast.set_source")]
    AuracastSetSource { source: String },
    /// For tools: the headset mode of the profile `name` (absent: the one
    /// streaming, else the active one), saved as the window's mode buttons
    /// save it: "off" mutes the microphone while the mode saved is Auto or
    /// On (mic_muted, the mode kept), "auto" / "on" set the mode and
    /// unmute. A running stream takes it as after profile.save. The value
    /// in force already: nothing happens. Errors profile.not_found,
    /// developer.off ("on" outside developer mode), profile.no_mic_output
    /// (no microphone output chosen), stream.busy (its stream is
    /// connecting) -> null
    #[serde(rename = "profile.set_headset_mic")]
    ProfileSetHeadsetMic {
        #[ts(optional = nullable)]
        #[serde(default)]
        name: Option<String>,
        mode: HeadsetMicMode,
    },
    /// For tools: mutes or unmutes the headset microphone of the profile
    /// `name` (absent as for profile.set_headset_mic), as Home's
    /// microphone button; the mode stays. Errors as profile.set_headset_mic
    /// but developer.off -> null
    #[serde(rename = "profile.set_mic_muted")]
    ProfileSetMicMuted {
        #[ts(optional = nullable)]
        #[serde(default)]
        name: Option<String>,
        muted: bool,
    },
    /// For tools: turns the playback EQ of the profile `name` (absent as
    /// for profile.set_headset_mic) on or off, its points kept (none
    /// saved: the flat ones the window starts with). A running stream
    /// takes it in place. Error profile.not_found -> null
    #[serde(rename = "profile.set_media_eq")]
    ProfileSetMediaEq {
        #[ts(optional = nullable)]
        #[serde(default)]
        name: Option<String>,
        enabled: bool,
    },
    /// -> SettingsApplied (the settings come back as an event too)
    #[serde(rename = "settings.set")]
    SettingsSet { settings: Box<Settings> },
    /// -> AdapterInfo[] (also as an event)
    #[serde(rename = "adapter.list")]
    AdapterList,
    /// Assigns WinUSB (UAC prompt) -> AdapterInfo[]
    #[serde(rename = "adapter.assign_winusb")]
    AdapterAssignWinusb { instance_id: String },
    /// Back to the previous driver (UAC prompt) -> AdapterInfo[]
    #[serde(rename = "adapter.restore_driver")]
    AdapterRestoreDriver { instance_id: String },
    /// -> WindowsPairedDevice[]: every device Windows' Bluetooth paired
    /// through the adapter with this instance ID (classic and LE, audio or
    /// not, each once; the ones that are not audio first): what stops
    /// working with Windows once it is on WinUSB. Empty when none or not
    /// readable; no administrator prompt.
    #[serde(rename = "adapter.windows_devices")]
    AdapterWindowsDevices { instance_id: String },
    /// Restarts the Bluetooth stack
    #[serde(rename = "adapter.restart")]
    AdapterRestart,
    /// Developer mode: the chip probe (read commands only) on this WinUSB
    /// adapter; the stack stops first -> ProbeResult
    #[serde(rename = "adapter.probe")]
    AdapterProbe { instance_id: String },
    /// Developer mode: reads a chip script for this adapter -> ScriptCheck
    #[serde(rename = "adapter.check_script")]
    AdapterCheckScript { instance_id: String, script: String },
    /// Developer mode: runs a chip script on this WinUSB adapter; one with
    /// lines that may change the chip only with allow_writes, never one
    /// with denied lines -> ScriptResult
    #[serde(rename = "adapter.run_script")]
    AdapterRunScript { instance_id: String, script: String, allow_writes: bool },
    /// -> FirmwareFile[]: the Realtek firmware pairs in the settings folder
    #[serde(rename = "adapter.firmware_files")]
    AdapterFirmwareFiles,
    /// Fetches the Realtek files of the chip whose firmware file this is
    /// (an AdapterInfo.firmware_files entry) from linux-firmware into the
    /// settings folder -> AdapterInfo[]
    #[serde(rename = "adapter.download_firmware")]
    AdapterDownloadFirmware { file: String },
    /// -> AudioDevice[]
    #[serde(rename = "audio.devices")]
    AudioDevices,
    /// -> AudioApp[]
    #[serde(rename = "audio.apps")]
    AudioApps,
    /// Downloads VB-Audio's VB-CABLE pack and installs it (UAC prompt)
    /// -> bool: VB-CABLE is among Windows' outputs now (false: Windows may
    /// need a restart first)
    #[serde(rename = "audio.install_vb_cable")]
    AudioInstallVbCable,
    /// Removes VB-CABLE, or the Hi-Fi Cable (`hifi`), with VB-Audio's own
    /// setup (UAC prompt) -> bool: it is gone from Windows' outputs (false:
    /// Windows may need a restart first)
    #[serde(rename = "audio.uninstall_cable")]
    AudioUninstallCable { hifi: bool },
    /// -> MixerApp[]
    #[serde(rename = "audio.mixer")]
    AudioMixer,
    /// Sets an app's volume (0-100) and / or mute
    #[serde(rename = "audio.mixer_set")]
    AudioMixerSet { app: String, volume: Option<u8>, muted: Option<bool> },
    /// Log lines at `logs` level and above (None: off), link samples, the
    /// receiver test's state (absent: off)
    #[serde(rename = "diag.subscribe")]
    DiagSubscribe {
        logs: Option<LogLevel>,
        link: bool,
        #[ts(optional = nullable)]
        #[serde(default)]
        receiver: Option<bool>,
    },
    /// -> the capture file's path
    #[serde(rename = "diag.capture_start")]
    DiagCaptureStart,
    /// -> CaptureState | null: the file and its final size
    #[serde(rename = "diag.capture_stop")]
    DiagCaptureStop,
    /// -> DiagInfo (text for "copy diagnostic info"); Bluetooth addresses
    /// masked unless `mask` is false. Without parameters as before.
    #[serde(rename = "diag.info")]
    DiagInfo(Option<DiagInfoParams>),
    /// -> WifiAccessPoint[]: a fresh Wi-Fi scan, access points on every
    /// band, loudest first (a few seconds). Errors "wifi.none" (no Wi-Fi) and
    /// "wifi.location" (Windows' location setting keeps the list), "wifi.off".
    #[serde(rename = "diag.wifi_scan")]
    DiagWifiScan,
    /// -> FlowView: the connection flow as the core followed it (every log
    /// line, also while no window was open)
    #[serde(rename = "diag.flow")]
    DiagFlow,
    /// -> LogLine[]: the kept lines at `level` and above, oldest first, for
    /// a log view that opens after they were logged
    #[serde(rename = "diag.recent_logs")]
    DiagRecentLogs { level: LogLevel },
    /// Verbose logging at `level` for 24 hours (debug) or 1 hour (trace),
    /// also across restarts of the core; None: back to info -> VerboseState | null
    #[serde(rename = "diag.set_verbose")]
    DiagSetVerbose { level: Option<LogLevel> },
    /// The crash reports shown (state, crashes) were seen: they are not
    /// shown again, newer ones are -> null
    #[serde(rename = "diag.crashes_seen")]
    DiagCrashesSeen,
    /// A marker line in the log ("===== MARK n: note =====", info), to find
    /// a moment again (the stutter starts now) -> number (n)
    #[serde(rename = "diag.mark")]
    DiagMark { note: String },
    /// Collects the logs, HCI captures, crash reports and the diagnostic
    /// info into one .zip in the logs folder -> DiagBundle
    #[serde(rename = "diag.bundle")]
    DiagBundle(Option<DiagBundleParams>),
    /// -> ReceiverInfo[]: measuring receivers on the local network (UDP
    /// broadcast, about 1.5 s)
    #[serde(rename = "receiver.discover")]
    ReceiverDiscover,
    /// Connects to a receiver's statistics: "host[:port]", or "auto" / ""
    /// for the first one found; saved as settings.remote_sink. Reconnects
    /// until receiver.disconnect. -> ReceiverState
    #[serde(rename = "receiver.connect")]
    ReceiverConnect { target: String },
    #[serde(rename = "receiver.disconnect")]
    ReceiverDisconnect,
    /// Receiver settings for the next stream (None / 0: unchanged): the
    /// L2CAP MTU it offers, its playout buffer. Error receiver.not_connected.
    #[serde(rename = "receiver.configure")]
    ReceiverConfigure {
        #[ts(optional = nullable)]
        #[serde(default)]
        mtu: Option<u32>,
        #[ts(optional = nullable)]
        #[serde(default)]
        buffer_ms: Option<u32>,
    },
    /// -> ReceiverState
    #[serde(rename = "receiver.status")]
    ReceiverStatus,
    /// The microphone test: records the headset microphone of the classic
    /// stream (the voice link opens for it, the stream first starts again
    /// on a stack without voice) -> MicTestState. Errors
    /// "mic_test.not_streaming", "mic_test.le_audio".
    #[serde(rename = "mic_test.record")]
    MicTestRecord,
    /// Stops the recording or the playback -> MicTestState
    #[serde(rename = "mic_test.stop")]
    MicTestStop,
    /// Plays the recording to the headphones -> MicTestState
    #[serde(rename = "mic_test.play")]
    MicTestPlay,
    /// -> MicTestState. Also keeps the test alive: one not asked about for
    /// a few seconds ends (the voice link closes unless headset mode wants it)
    #[serde(rename = "mic_test.status")]
    MicTestStatus,
    /// Ends the test -> null
    #[serde(rename = "mic_test.end")]
    MicTestEnd,
    /// Opens an RFCOMM channel to the service `uuid` ("0000xxxx-0000-1000-
    /// 8000-00805F9B34FB", or "xxxx" on the Bluetooth base UUID) of the
    /// device at `address`, the device being streamed to, over the link
    /// the core has to it: the core never connects or pairs for this
    /// (error rfcomm.not_connected). Its data comes as rfcomm_data events
    /// to this connection only, which owns the channel; it closes when this
    /// connection ends, the A2DP connection ends (rfcomm_closed
    /// "disconnected"), or the device closes it ("remote"). Answered within
    /// 20 s, also behind other opens: wait at least 25 s. -> RfcommOpened.
    /// Errors: rfcomm.not_connected, rfcomm.service_not_found,
    /// rfcomm.service_reserved (headset / hands-free 0x1108, 0x1112,
    /// 0x111E, 0x111F: the core's own headset microphone link),
    /// rfcomm.open_failed (also when out of time: a channel that opens
    /// later is closed), rfcomm.busy (too many channels, or out of time
    /// behind other opens).
    #[serde(rename = "rfcomm.open")]
    RfcommOpen { address: String, uuid: String },
    /// Sends `data` (base64) on a channel of this connection, in frames of
    /// at most its `mtu`, in the order of the requests -> null. At most
    /// 64 KiB at once (else rfcomm.bad_request). Errors: rfcomm.no_channel,
    /// rfcomm.busy (send queue full: not sent), rfcomm.timeout (sent or
    /// not, unknown: not to be repeated; reopen the channel).
    #[serde(rename = "rfcomm.send")]
    RfcommSend { channel_id: u32, data: String },
    /// Closes a channel of this connection after the data sent before; no
    /// rfcomm_closed follows -> null. Errors: rfcomm.no_channel,
    /// rfcomm.timeout.
    #[serde(rename = "rfcomm.close")]
    RfcommClose { channel_id: u32 },
    /// Keyboards and mice through BlueCaster (experimental, developer mode,
    /// feature "hid-host"): a new code the user has to type into hid.enable,
    /// valid for 120 s and once -> HidCode
    #[serde(rename = "hid.code")]
    HidCode,
    /// Turns the HID host on: the code of hid.code, and the user's two
    /// statements -> null. Errors ipc.error.developer_only,
    /// error.hid.not_confirmed, error.hid.wrong_code (a new code is needed).
    #[serde(rename = "hid.enable")]
    HidEnable { code: String, other_input_at_hand: bool, risk_understood: bool },
    /// Turns it off at once -> null
    #[serde(rename = "hid.disable")]
    HidDisable,
    /// -> HidStatus
    #[serde(rename = "hid.status")]
    HidStatus,
    /// Adds a keyboard or mouse and connects it, pairing on the way (it has
    /// to be in pairing mode; several seconds) -> null. Errors error.hid.off,
    /// error.hid.too_many, error.hid.connect_failed, error.hid.restart_pending,
    /// error.hid.le_unavailable (an LE device, the adapter without LE).
    #[serde(rename = "hid.add")]
    HidAdd {
        address: String,
        name: String,
        /// A Bluetooth LE device: its address type (Device.hid_le); None
        /// for a classic one
        #[serde(default)]
        le: Option<u8>,
    },
    /// Connects one on the list again -> null; errors as hid.add
    #[serde(rename = "hid.connect")]
    HidConnect { address: String },
    /// Disconnects one, takes it off the list and forgets its pairing -> null
    #[serde(rename = "hid.remove")]
    HidRemove { address: String },
    /// Removes a keyboard or mouse being added (one of Snapshot.hid_checks)
    /// now, as its countdown would -> null. Does nothing for a device that
    /// is not under check (its countdown may have just ended). There is no
    /// request that confirms a device: only its input reaching Windows does.
    #[serde(rename = "hid.check_cancel")]
    HidCheckCancel { address: String },
    /// The notice of a device removed again (Snapshot.hid_rolled_back)
    /// goes away -> null
    #[serde(rename = "hid.check_dismiss")]
    HidCheckDismiss,
    /// -> UpdateState; error update.failed (update.error) when the check
    /// failed
    #[serde(rename = "app.check_update")]
    AppCheckUpdate,
    /// -> the QcyState of qcy.ts: the last read of the QCY earbuds (no
    /// Bluetooth traffic)
    #[serde(rename = "qcy.state")]
    QcyState,
    /// Reads the QCY earbuds now (read-only queries only, never pairs,
    /// never restarts the stack) -> QcyState; also sent as Event::Qcy
    #[serde(rename = "qcy.refresh")]
    QcyRefresh,
    /// The same read, for "Tentar de novo" -> QcyState
    #[serde(rename = "qcy.reconnect")]
    QcyReconnect,
    /// Changes ONE setting of the QCY earbuds: kind "game_mode",
    /// "sleep_mode" or "wind_reduction" with `enabled` (opcodes 0x09 / 0x10 /
    /// 0x2A, 01/02); anything else (the prompt volume 0x1D too: read-only)
    /// is refused
    /// with qcy.read_only before any Bluetooth traffic. Reads the old value,
    /// sends one command, reads it back (3 s each) -> QcyState with `write`
    /// (confirmed or failed, never re-sent); also sent as Event::Qcy
    #[serde(rename = "qcy.set")]
    QcySet {
        kind: String,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<u8>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minutes: Option<u16>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        eq: Option<serde_json::Value>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        side: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gesture: Option<String>,
        #[ts(optional = nullable)]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
    },
    /// The UI hid its main window to the tray: the core shows v1's "still
    /// running in the tray" balloon the first time
    #[serde(rename = "app.hidden_to_tray")]
    AppHiddenToTray,
    /// Ends the core (streaming stops)
    #[serde(rename = "app.quit")]
    AppQuit,
    /// Ends the core so that the UI can install an update, unless it
    /// streams or connects (error `app.streaming`): an update never cuts
    /// the audio
    #[serde(rename = "app.quit_for_update")]
    AppQuitForUpdate,
    /// The user's Exit in service mode, where the core keeps running:
    /// streaming stops, the UI closes (event ui_close) and the tray icon
    /// goes until BlueCaster is started again -> null. Keyboards and mice
    /// keep working. Without service mode the same as app.quit.
    #[serde(rename = "app.close")]
    AppClose,
    /// Service mode: runs `call`, a request that answered ipc.needs_admin,
    /// through the elevated helper (a UAC prompt) -> the call's result.
    /// Errors ipc.admin_declined, ipc.bad_request (a call that needs no
    /// administrator), and the call's own.
    #[serde(rename = "app.run_as_admin")]
    AppRunAsAdmin { call: Box<Method> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AudioDevice {
    pub id: String,
    /// Windows' friendly name, which already ends in the adapter's name in
    /// parentheses: "Speakers (Realtek(R) Audio)"
    pub name: String,
    /// The endpoint's own name without the adapter's ("Speakers"); None:
    /// Windows has none
    #[ts(optional = nullable)]
    #[serde(default)]
    pub short_name: Option<String>,
    /// The adapter's name ("VB-Audio Hi-Fi Cable"); None: Windows has none
    #[ts(optional = nullable)]
    #[serde(default)]
    pub adapter_name: Option<String>,
    pub is_default: bool,
    pub is_virtual: bool,
    /// A monitor's or TV's audio (HDMI, DisplayPort)
    #[serde(default)]
    pub is_display: bool,
    /// Windows' shared-mode mix format: what capturing it delivers without
    /// resampling (None: not known)
    #[serde(default)]
    pub mix_rate: Option<u32>,
    #[serde(default)]
    pub mix_channels: Option<u16>,
    #[serde(default)]
    pub mix_bits: Option<u16>,
    /// The recording side apps pick as a microphone when the headset
    /// microphone is rendered here ("CABLE Output" for "CABLE Input");
    /// None: it has none or is not a virtual device
    #[serde(default)]
    pub capture_name: Option<String>,
    /// The sample rate Windows runs that recording side at (None: not known
    /// or no recording side)
    #[serde(default)]
    pub capture_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct AudioApp {
    pub exe: String,
    pub name: String,
}

/// An app on the default output device
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct MixerApp {
    /// What audio.mixer_set takes; "*system" for the system sounds
    pub key: String,
    pub name: String,
    /// 0-100
    pub volume: u8,
    pub muted: bool,
}

/// What settings.set did besides saving the settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct SettingsApplied {
    /// LE Audio (experimental) was turned on or off while BTstack is in
    /// use (streaming or scanning): the change applies at the next scan or
    /// connection (v1's settings.le_audio_deferred)
    #[serde(default)]
    pub le_audio_deferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct DiagInfo {
    pub text: String,
}

/// An access point a Wi-Fi scan heard
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct WifiAccessPoint {
    /// Empty for a hidden network
    pub ssid: String,
    /// The channel number on its band (1-14 on 2.4 GHz)
    pub channel: u8,
    /// Center frequency: below 2500 is 2.4 GHz, below 5925 is 5 GHz, else
    /// 6 GHz
    pub mhz: u32,
    /// dBm
    pub rssi: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct DiagInfoParams {
    /// Bluetooth addresses as <addr-N>, device names as <device-N>, the
    /// Windows user name and USB serial numbers hidden (default true)
    #[ts(optional = nullable)]
    #[serde(default)]
    pub mask: Option<bool>,
}

/// The connection flow of the diagnostics view: the steps of the current
/// (or last) streaming session
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct FlowView {
    /// 1, 2, ... since the core started; 0 = no session yet
    pub session: u32,
    /// LogLine.seq of the attempt's first line ("this attempt only")
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub first_seq: Option<u64>,
    #[ts(type = "number")]
    pub start_ms: u64,
    #[ts(type = "number | null")]
    pub stop_ms: Option<u64>,
    pub reconnects: u32,
    /// In order: start, btstack, connect, caps, codec, capture, config,
    /// encoder, stream, audio, link
    pub steps: Vec<FlowStepView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct FlowStepView {
    /// "start", "btstack", ... "link"
    pub step: String,
    /// "pending", "running", "done", "warning" or "failed"
    pub status: String,
    /// Since the session started
    #[ts(type = "number | null")]
    pub elapsed_ms: Option<u64>,
    /// English; `detail_msg` translates it where the core computed it
    pub detail: String,
    pub detail_msg: Option<Msg>,
    /// LogLine.seq of the line that set the status (None: none yet)
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub seq: Option<u64>,
    /// LogLine.time_ms of that line
    #[ts(optional = nullable, type = "number | null")]
    #[serde(default)]
    pub time_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Peer receiver test
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverConnection {
    #[default]
    Off,
    Connecting,
    Connected,
    /// Not reachable; retried every few seconds (`detail` says why)
    Failed,
}

/// A measuring receiver (tools/linux_sink): from discovery or its hello
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct ReceiverInfo {
    pub ip: String,
    pub port: u16,
    pub host: String,
    /// Its Bluetooth adapter: the device to stream to
    pub bt_address: String,
    pub bt_name: String,
    pub codecs: Vec<String>,
    /// L2CAP MTU it offers for AVDTP
    pub mtu: u32,
    /// Its playout buffer model (0 = not said)
    pub buffer_ms: u32,
}

/// The stream the receiver measures; counters per stream (`id`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct ReceivedStream {
    pub id: u32,
    pub codec: String,
    pub config: String,
    /// It decodes (not only header checks)
    pub decoding: bool,
    #[ts(type = "number")]
    pub packets: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub frames: u64,
    #[ts(type = "number")]
    pub lost: u64,
    #[ts(type = "number")]
    pub late: u64,
    #[ts(type = "number")]
    pub ts_errors: u64,
    #[ts(type = "number")]
    pub frame_errors: u64,
    #[ts(type = "number")]
    pub decode_errors: u64,
    /// Dropouts of the modelled playout buffer, and skips
    #[ts(type = "number")]
    pub underruns: u64,
    #[ts(type = "number")]
    pub overflows: u64,
    /// The source paused sending
    #[ts(type = "number")]
    pub pauses: u64,
    /// The source sends nothing right now
    pub idle: bool,
    pub audio_ms: f64,
    pub underrun_ms: f64,
    pub overflow_ms: f64,
    pub max_gap_ms: f64,
    pub jitter_ms: f64,
    /// Modelled playout buffer, None = not playing
    pub buffer_ms: Option<f64>,
    /// The last reporting interval (about a second)
    #[ts(type = "number")]
    pub interval_packets: u64,
    #[ts(type = "number")]
    pub interval_bytes: u64,
    pub interval_max_gap_ms: f64,
}

/// A statistics report of the receiver
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct ReceiverStats {
    /// A source is connected to it
    pub connected: bool,
    /// Media flows right now
    pub streaming: bool,
    /// Bluetooth address of that source
    pub device: String,
    pub buffer_target_ms: u32,
    /// L2CAP MTU it offers for new streams
    pub mtu: u32,
    pub codecs: Vec<String>,
    /// BR/EDR RSSI relative to the golden receive range (not dBm)
    pub rssi: Option<i32>,
    pub tx_power: Option<i32>,
    /// Why RSSI is missing, if it says
    pub rssi_note: String,
    /// Channels the link hops over, seen from the receiver
    pub afh_channels: Option<u32>,
    pub stream: Option<ReceivedStream>,
}

/// The receiver test as the core runs it (receiver.status, Event::Receiver)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]

pub struct ReceiverState {
    pub connection: ReceiverConnection,
    /// Why it failed (English, from the connection)
    pub detail: String,
    /// "host[:port]" or "auto"; empty while off
    pub target: String,
    /// The receiver, once found or introduced
    pub receiver: Option<ReceiverInfo>,
    /// Its latest report and how old that is (it reports once a second;
    /// older than 3 s means it stopped)
    pub stats: Option<ReceiverStats>,
    #[ts(type = "number | null")]
    pub stats_age_ms: Option<u64>,
    /// What this PC sent since the current (or last) stream started, to set
    /// beside the receiver's numbers; None before any stream
    pub sent: Option<LinkTotals>,
    /// LinkCounters.stream_starts: a change means `sent` starts again
    pub stream_starts: u32,
}

/// device.pairing's result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct DevicePairing {
    /// The link key BlueCaster keeps, None when it has none (not paired)
    pub link_key: Option<LinkKeyInfo>,
    /// The LE keys BlueCaster has of the device (derived from the link key
    /// while LE Audio and developer mode are on, issue #86), None when it
    /// has none or no stack with LE Audio runs
    pub le_keys: Option<LeKeysInfo>,
    /// Why there are no LE keys; None when there are
    pub le_keys_missing: Option<LeKeysMissing>,
}

/// Why BlueCaster has no LE keys of a classic device (issue #86)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LeKeysMissing {
    /// No stack runs
    NoStack,
    /// Not asked since Bluetooth started (no connection since)
    NotAsked,
    /// Asked, no answer yet
    Asking,
    /// The link is not encrypted with Secure Connections
    NotSecure,
    /// The device opened the connection; only one BlueCaster opens asks
    DeviceLed,
    /// The device does not support it
    NoSupport,
    /// The device refused
    Refused,
    /// The device did not answer
    NoAnswer,
    /// The device answered without keys
    NoneGiven,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct LeKeysInfo {
    /// The LTK: 32 hex digits, least significant octet first (the order
    /// SMP and HCI carry it in, as the link key); None when there is none
    pub long_term_key: Option<String>,
    /// The LTK's encryption key size in octets (0 without an LTK)
    pub key_size: u8,
    /// The device's IRK, in the same order; None when it gave none
    pub identity_resolving_key: Option<String>,
    /// The identity address the IRK belongs to ("XX:XX:XX:XX:XX:XX")
    pub identity_address: String,
    /// The identity address is a random static one (else public)
    pub identity_random: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct LinkKeyInfo {
    /// 32 hex digits, in the order of HCI's Link Key Notification (the
    /// order BlueZ and Windows keep it in too)
    pub key: String,
    /// HCI's link key type (4: unauthenticated P-192, 5: authenticated
    /// P-192, 7 and 8: their P-256 forms). A key taken from Windows, which
    /// keeps no type, has 4.
    pub key_type: u8,
}

/// One service record of device.services
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct DeviceService {
    /// Its service class UUIDs, 128-bit form in lowercase
    pub uuids: Vec<String>,
    /// The name the device gives it
    pub name: Option<String>,
}

/// device.services' result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct ServiceList {
    pub services: Vec<DeviceService>,
    /// Not connected now: the list from its last connection
    pub from_last_connection: bool,
}

/// A device Windows' Bluetooth paired through an adapter: a classic audio
/// one (device.windows_paired_through) or any (adapter.windows_devices)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct WindowsPairedDevice {
    pub address: String,
    /// Windows' name for it (empty when it has none)
    pub name: String,
    /// An audio device (headphones, speakers: class of device Audio/Video),
    /// shown with their icon in the warning before WinUSB (issue #86)
    #[serde(default)]
    pub audio: bool,
}

/// How many devices' keys agree, for one kind of key
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KeyCounts {
    /// The same in BlueCaster and Windows
    pub same: u32,
    /// Both have keys of the device, but not the same
    pub differ: u32,
    /// Only BlueCaster has keys of the device
    pub only_bluecaster: u32,
    /// Only Windows has keys of the device (for this adapter)
    pub only_windows: u32,
}

/// A value Windows keeps under its keys: its name, type and size, never its
/// data
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WindowsValueName {
    pub name: String,
    /// "binary", "DWORD", "QWORD", "string" or "type N"
    pub kind: String,
    /// Bytes of data
    pub size: u32,
    /// How many keys have it (LE devices)
    pub count: u32,
}

/// dev.compare_windows_keys' result: counts and value names, no address or
/// key
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KeysComparison {
    /// Windows keeps keys for the adapter
    pub adapter_found: bool,
    /// Link keys
    pub classic: KeyCounts,
    /// LE keys; None: BlueCaster's could not be read
    #[ts(optional = nullable)]
    pub le: Option<KeyCounts>,
    /// The values of the adapter's key in Windows (its own, not the devices')
    pub radio_values: Vec<WindowsValueName>,
    /// The values of Windows' LE device keys, counted over the devices
    pub le_values: Vec<WindowsValueName>,
}

/// device.import_windows_pairings' result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct WindowsImport {
    /// Windows has keys for the adapter (false: it never paired a device
    /// with it)
    pub adapter_found: bool,
    /// The addresses whose keys were taken
    pub imported: Vec<String>,
    /// The addresses BlueCaster has a key for already: it stays (the device
    /// was paired with BlueCaster after Windows, Windows' key is an old one)
    pub kept: Vec<String>,
    /// The audio devices among `imported` that were added to the saved
    /// devices, with Windows' names
    #[serde(default)]
    pub added: Vec<String>,
}

/// rfcomm.open's result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct RfcommOpened {
    pub channel_id: u32,
    /// Largest frame the channel sends and receives (bytes)
    pub mtu: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct RfcommData {
    pub channel_id: u32,
    /// base64 (standard alphabet, padded)
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct RfcommClosed {
    pub channel_id: u32,
    /// "remote": the device closed it or the link went down;
    /// "disconnected": the A2DP connection ended; "stopped": the Bluetooth
    /// stack stopped; "overflow": this connection did not read the data
    /// (over 1 MiB waiting)
    pub reason: String,
}

/// A keyboard or mouse added for the HID host (experimental)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidDevice {
    /// "XX:XX:XX:XX:XX:XX"
    pub address: String,
    pub name: String,
    pub connected: bool,
    /// Added as a Bluetooth LE device
    #[serde(default)]
    pub le: bool,
}

/// What the HID host did since its stack started: counts only, nothing of
/// what was typed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]

pub struct HidCounts {
    /// Input reports received
    pub reports: u64,
    /// Reports in a format the host does not read
    pub undecoded: u64,
    /// Key and pointer events decoded
    pub events: u64,
    /// Events Windows took
    pub injected: u64,
    /// Events Windows did not take (a window running as administrator, the
    /// secure desktop)
    pub blocked: u64,
    /// Connections refused (a device not on the list)
    pub refused: u64,
}

/// hid.status' result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidStatus {
    pub devices: Vec<HidDevice>,
    /// None while no stack with the HID host runs
    pub counts: Option<HidCounts>,
}

/// A keyboard or mouse being added, until its input reaches Windows
/// (Snapshot.hid_checks)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidCheck {
    /// "XX:XX:XX:XX:XX:XX"
    pub address: String,
    pub name: String,
    /// From its report descriptor: "keyboard", "mouse", "both", or "" while
    /// not known
    pub kind: String,
    /// The countdown runs; false: the device is not ready for input yet
    /// (connecting, pairing)
    pub waiting: bool,
    /// Milliseconds left of the countdown when this was sent
    pub remaining_ms: u32,
    /// The countdown's length in milliseconds
    pub window_ms: u32,
}

/// A keyboard or mouse removed again because its input did not reach
/// Windows (Snapshot.hid_rolled_back)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidRolledBack {
    pub address: String,
    pub name: String,
    /// As hid.add's `le`, for adding it again
    #[serde(default)]
    pub le: Option<u8>,
    /// "no_input": connected, but no input arrived in time;
    /// "disconnected": it went away during the countdown; "not_connected":
    /// it never got ready; "cancelled": hid.check_cancel
    pub reason: String,
}

/// The self-test of the path to Windows (Snapshot.hid_health)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidHealth {
    /// "ok", "not_tested" (no input goes to Windows) or "failed"
    pub state: String,
    /// With "failed": "helper" (the input helper did not start),
    /// "refused" (Windows refused the test input) or "timeout"
    #[serde(default)]
    pub reason: Option<String>,
}

/// A number to type on a keyboard being added, then Enter (Snapshot.hid_passkey)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidPasskey {
    /// The device's "XX:XX:XX:XX:XX:XX"
    pub address: String,
    /// Six digits
    pub passkey: String,
}

/// Its Debug form hides the number: a logged state never holds it
impl std::fmt::Debug for HidPasskey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HidPasskey").field("passkey", &"hidden").finish_non_exhaustive()
    }
}

/// hid.code's result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]

pub struct HidCode {
    /// Six digits
    pub code: String,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol: u32,
        /// "bluecaster-ui/2.0.0"
        client: String,
    },
    Request {
        #[ts(type = "number")]
        id: u64,
        #[serde(flatten)]
        call: Method,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        protocol: u32,
        /// "bluecaster-core/2.0.0"
        server: String,
        /// Additions to the protocol this core has ([`FEATURES`]): a client
        /// checks for one before using it. Absent from older cores.
        #[serde(default)]
        features: Vec<String>,
    },
    Reject {
        reason: String,
    },
    Snapshot {
        #[ts(type = "number")]
        rev: u64,
        state: Box<Snapshot>,
    },
    Event {
        #[ts(type = "number")]
        rev: u64,
        event: Box<Event>,
    },
    Response {
        #[ts(type = "number")]
        id: u64,
        #[ts(type = "unknown")]
        result: Option<serde_json::Value>,
        error: Option<ErrorInfo>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_processing_reads_the_old_noise_suppression_switch() {
        let read = |j: serde_json::Value| serde_json::from_value::<MicProcessing>(j).unwrap();
        // Switched off: the strength is 0 whatever was saved
        let off =
            read(serde_json::json!({"gain_db": 6, "noise_suppression": false, "suppression": 70, "gate_db": -50}));
        assert_eq!(off, MicProcessing { enabled: true, gain_db: 6, suppression: 0, gate_db: -50 });
        let on = read(serde_json::json!({"noise_suppression": true, "suppression": 70}));
        assert_eq!(
            on,
            MicProcessing { enabled: true, gain_db: 0, suppression: 70, gate_db: MicProcessing::GATE_OFF_DB }
        );
        // Saved before the switch over all of it: on when anything was set
        let nothing =
            read(serde_json::json!({"gain_db": 0, "noise_suppression": false, "suppression": 100, "gate_db": -80}));
        assert_eq!(nothing, MicProcessing::default());
        assert!(read(serde_json::json!({"gate_db": -50})).enabled);
        assert_eq!(read(serde_json::json!({"noise_suppression": true})).suppression, 100);
        // Without the switch: the strength as saved, none at all is off
        assert_eq!(read(serde_json::json!({"suppression": 40})).suppression, 40);
        assert_eq!(read(serde_json::json!({})), MicProcessing::default());
        // The switch as saved, off with values kept
        let now = MicProcessing { enabled: false, gain_db: -3, suppression: 55, gate_db: -40 };
        let j = serde_json::to_value(now).unwrap();
        assert_eq!(j, serde_json::json!({"enabled": false, "gain_db": -3, "suppression": 55, "gate_db": -40}));
        assert_eq!(read(j), now);
    }

    #[test]
    fn wire_format() {
        let m = ClientMessage::Request { id: 7, call: Method::StreamStart { profile: "WH".into() } };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"type": "request", "id": 7, "method": "stream.start", "params": {"profile": "WH"}})
        );
        let back: ClientMessage = serde_json::from_value(j).unwrap();
        assert_eq!(back, m);
        let stop: ClientMessage =
            serde_json::from_value(serde_json::json!({"type": "request", "id": 1, "method": "stream.stop"})).unwrap();
        assert_eq!(stop, ClientMessage::Request { id: 1, call: Method::StreamStop });
        let e = ServerMessage::Event { rev: 3, event: Box::new(Event::Scanning(true)) };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"type": "event", "rev": 3, "event": {"kind": "scanning", "value": true}})
        );
    }

    /// Requests of a UI built before the additions still parse
    #[test]
    fn older_requests_parse() {
        let m = |j: serde_json::Value| serde_json::from_value::<Method>(j).unwrap();
        assert_eq!(m(serde_json::json!({"method": "diag.info"})), Method::DiagInfo(None));
        assert_eq!(m(serde_json::json!({"method": "diag.info", "params": null})), Method::DiagInfo(None));
        assert_eq!(
            m(serde_json::json!({"method": "diag.info", "params": {"mask": false}})),
            Method::DiagInfo(Some(DiagInfoParams { mask: Some(false) }))
        );
        assert_eq!(
            m(serde_json::json!({"method": "diag.subscribe", "params": {"logs": "info", "link": true}})),
            Method::DiagSubscribe { logs: Some(LogLevel::Info), link: true, receiver: None }
        );
        // Each call sets both: a missing `logs` turns the log lines off, a
        // missing `link` is refused
        assert_eq!(
            m(serde_json::json!({"method": "diag.subscribe", "params": {"link": true}})),
            Method::DiagSubscribe { logs: None, link: true, receiver: None }
        );
        assert!(
            serde_json::from_value::<Method>(
                serde_json::json!({"method": "diag.subscribe", "params": {"logs": "info"}})
            )
            .is_err()
        );
        let custom = m(serde_json::json!({"method": "stream.start_custom", "params": {
            "device": "AA:BB:CC:DD:EE:FF", "codec": "sbc", "quality": "hq", "source": "tone"}}));
        assert!(matches!(custom, Method::StreamStartCustom { sample_rate: None, source_id: None, .. }));
    }

    /// The tools' methods ("tool_controls"): enums, bools and numbers only;
    /// anything else does not parse (answered with ipc.bad_request)
    #[test]
    fn tool_control_requests() {
        assert!(FEATURES.contains(&"tool_controls"));
        let m = |j: serde_json::Value| serde_json::from_value::<Method>(j);
        assert_eq!(
            m(serde_json::json!({"method": "profile.set_headset_mic", "params": {"mode": "auto"}})).unwrap(),
            Method::ProfileSetHeadsetMic { name: None, mode: HeadsetMicMode::Auto }
        );
        assert_eq!(
            m(serde_json::json!({"method": "profile.set_headset_mic", "params": {"name": "WH", "mode": "on"}}))
                .unwrap(),
            Method::ProfileSetHeadsetMic { name: Some("WH".into()), mode: HeadsetMicMode::On }
        );
        assert_eq!(
            m(serde_json::json!({"method": "profile.set_mic_muted", "params": {"name": null, "muted": true}})).unwrap(),
            Method::ProfileSetMicMuted { name: None, muted: true }
        );
        assert_eq!(
            m(serde_json::json!({"method": "profile.set_media_eq", "params": {"enabled": false}})).unwrap(),
            Method::ProfileSetMediaEq { name: None, enabled: false }
        );
        assert_eq!(
            m(serde_json::json!({"method": "stream.volume_step", "params": {"delta": -8}})).unwrap(),
            Method::StreamVolumeStep { delta: -8 }
        );
        for bad in [
            serde_json::json!({"method": "profile.set_headset_mic", "params": {"mode": "loud"}}),
            serde_json::json!({"method": "profile.set_headset_mic", "params": {"mode": 1}}),
            serde_json::json!({"method": "profile.set_headset_mic", "params": {}}),
            serde_json::json!({"method": "profile.set_mic_muted", "params": {"muted": "yes"}}),
            serde_json::json!({"method": "profile.set_media_eq", "params": {"name": 3, "enabled": true}}),
            serde_json::json!({"method": "stream.volume_step", "params": {"delta": 1.5}}),
            serde_json::json!({"method": "stream.volume_step", "params": {"delta": 70000}}),
            serde_json::json!({"method": "stream.volume_step"}),
        ] {
            assert!(m(bad.clone()).is_err(), "{bad}");
        }
        let j = serde_json::to_value(Method::ProfileSetHeadsetMic { name: None, mode: HeadsetMicMode::Off }).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"method": "profile.set_headset_mic", "params": {"name": null, "mode": "off"}})
        );
        for mode in [HeadsetMicMode::Off, HeadsetMicMode::Auto, HeadsetMicMode::On] {
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.as_str());
        }
    }

    /// State of a core built before the additions still parses
    #[test]
    fn older_state_parses() {
        let line: LogLine =
            serde_json::from_value(serde_json::json!({"time_ms": 1, "level": "info", "target": "t", "message": "m"}))
                .unwrap();
        assert_eq!((line.seq, line.session), (None, None));
        let sample: LinkSample = serde_json::from_value(serde_json::json!({
            "time_ms": 0, "rssi": null, "afh_in_use": null, "afh_usable": null, "afh_avoided": "",
            "bytes_per_second": 0.0, "packets_per_second": 0.0, "queue_depth": 0, "dropped": 0,
            "capture_dropped": 0, "airtime": 0.0}))
        .unwrap();
        assert!(sample.counters.is_none());
        let dev: Device = serde_json::from_value(
            serde_json::json!({"address": "A", "name": "", "saved": true, "audio": true, "rssi": null}),
        )
        .unwrap();
        assert!(dev.codecs.is_empty());
    }

    /// The LE Audio additions: absent in an older core's state and an older
    /// UI's requests, and they round-trip
    #[test]
    fn le_audio_fields_are_optional() {
        let m = |j: serde_json::Value| serde_json::from_value::<Method>(j).unwrap();
        assert_eq!(
            m(serde_json::json!({"method": "device.forget", "params": {"address": "A"}})),
            Method::DeviceForget { address: "A".into(), le_audio: None }
        );
        assert_eq!(
            m(serde_json::json!({"method": "device.rename", "params": {"address": "A", "name": "N"}})),
            Method::DeviceRename { address: "A".into(), name: "N".into(), le_audio: None }
        );
        let forget = Method::DeviceForget { address: "A".into(), le_audio: Some(true) };
        let j = serde_json::to_value(&forget).unwrap();
        assert_eq!(j, serde_json::json!({"method": "device.forget", "params": {"address": "A", "le_audio": true}}));
        assert_eq!(serde_json::from_value::<Method>(j).unwrap(), forget);

        let dev: Device = serde_json::from_value(
            serde_json::json!({"address": "A", "name": "", "saved": true, "audio": true, "rssi": null}),
        )
        .unwrap();
        assert!(!dev.le_audio && !dev.le_audio_set);
        let le = Device { le_audio: true, le_audio_set: true, ..dev };
        let back: Device = serde_json::from_value(serde_json::to_value(&le).unwrap()).unwrap();
        assert_eq!(back, le);

        let profile: Profile = serde_json::from_value(serde_json::json!({
            "name": "P", "device_address": "A", "device_name": "", "codec": "auto", "quality": "hq",
            "abr": false, "codec_fallback": true, "sample_rate": 0, "bit_depth": 0, "capture_mode": "loopback",
            "audio_device_id": "", "audio_device_name": "", "auto_switch_device": true, "app_exe": "",
            "app_name": "", "volume_sync": "auto", "volume_device_id": "", "volume_device_name": "",
            "max_media_payload": 1023}))
        .unwrap();
        assert!(!profile.le_audio);
        assert!(profile.headset_mic == "off" && profile.mic_device_id.is_empty() && profile.mic_device_name.is_empty());
        let le = Profile {
            le_audio: true,
            headset_mic: "auto".into(),
            mic_device_id: "{cable}".into(),
            mic_device_name: "CABLE Input".into(),
            ..profile
        };
        assert_eq!(serde_json::from_value::<Profile>(serde_json::to_value(&le).unwrap()).unwrap(), le);
        assert_eq!(serde_json::to_value(&le).unwrap()["headset_mic"], "auto");

        // A stream status without voice, and one with it
        let st: StreamStatus = serde_json::from_value(serde_json::json!({
            "state": "streaming", "message": {"key": "", "args": []}, "profile": "P", "info": null,
            "volume": null}))
        .unwrap();
        assert!(st.voice.is_none());
        let voice = StreamStatus {
            voice: Some(VoiceStatus {
                codec: "msbc".into(),
                link: "hfp".into(),
                rate: 16000,
                mic_device_id: "{cable}".into(),
            }),
            ..st
        };
        let j = serde_json::to_value(&voice).unwrap();
        assert_eq!(
            j["voice"],
            serde_json::json!({"codec": "msbc", "link": "hfp", "rate": 16000, "mic_device_id": "{cable}"})
        );
        assert_eq!(serde_json::from_value::<StreamStatus>(j).unwrap(), voice);

        let settings: Settings = serde_json::from_value(serde_json::json!({
            "language": "en", "theme": "system", "start_with_windows": false, "start_minimized": false,
            "minimize_to_tray": true, "check_updates_on_startup": true, "include_prereleases": false,
            "media_keys": true, "developer_mode": false, "bt_chip_pid": 0,
            "bt_chip_fw_stem": "", "afh": "auto", "remote_sink": ""}))
        .unwrap();
        assert!(!settings.le_audio_experimental && !settings.le_audio_low_latency);
        assert!(settings.volume_sync);
        let on = Settings { le_audio_experimental: true, le_audio_low_latency: true, volume_sync: false, ..settings };
        assert_eq!(serde_json::from_value::<Settings>(serde_json::to_value(&on).unwrap()).unwrap(), on);

        let info: StreamInfo = serde_json::from_value(serde_json::json!({
            "codec": "SBC", "bitrate_kbps": 328, "sample_rate": 48000, "channels": 2, "source_sample_rate": 48000,
            "source_channels": 2, "source_bits": 32, "requested_codec": null, "requested_in_use": false}))
        .unwrap();
        assert!(info.le.is_none());
        let lc3 = StreamInfo {
            codec: "LC3".into(),
            bitrate_kbps: 192,
            details: None,
            le: Some(LeStreamInfo {
                preset: "48_4_2".into(),
                frame_us: 10000,
                octets: 120,
                cis: 2,
                devices: 1,
                low_latency: false,
                summary: "LC3 48 kHz 10 ms 120 octets (96 kbps/ch), 2 ch over 2 CIS, 48_4_2".into(),
            }),
            ..info
        };
        assert_eq!(serde_json::from_value::<StreamInfo>(serde_json::to_value(&lc3).unwrap()).unwrap(), lc3);

        let sample: LinkSample = serde_json::from_value(serde_json::json!({
            "time_ms": 0, "rssi": null, "afh_in_use": null, "afh_usable": null, "afh_avoided": "",
            "bytes_per_second": 0.0, "packets_per_second": 0.0, "queue_depth": 0, "dropped": 0,
            "capture_dropped": 0, "airtime": 0.0}))
        .unwrap();
        assert!(sample.iso.is_none());
        let iso = LinkSample {
            iso: Some(IsoCounters { cis: 2, sdu_sets: 1000, silence: 3, underruns: 1, dropped: 0, prefill: 4 }),
            ..sample
        };
        assert_eq!(serde_json::from_value::<LinkSample>(serde_json::to_value(&iso).unwrap()).unwrap(), iso);

        // A snapshot without le_scanning, and the event
        let mut snap = serde_json::to_value(Snapshot {
            adapter: AdapterState::default(),
            devices: Vec::new(),
            scanning: false,
            le_scanning: true,
            stream: StreamStatus::default(),
            profiles: Vec::new(),
            active_profile: None,
            settings: on,
            update: UpdateState::default(),
            config_dir: String::new(),
            log_dir: String::new(),
            capture: None,
            clock_ms: None,
            v1_running: false,
            verbose: None,
            crashes: Vec::new(),
            audio_endpoints: 0,
            vendors: Vec::new(),
            hid_devices: Vec::new(),
            hid_candidates: Vec::new(),
            hid_passkey: None,
            hid_checks: Vec::new(),
            hid_rolled_back: None,
            hid_health: None,
            service_mode: false,
        })
        .unwrap();
        snap.as_object_mut().unwrap().remove("le_scanning");
        assert!(!serde_json::from_value::<Snapshot>(snap).unwrap().le_scanning);
        let e = ServerMessage::Event { rev: 4, event: Box::new(Event::LeScanning(true)) };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j, serde_json::json!({"type": "event", "rev": 4, "event": {"kind": "le_scanning", "value": true}}));
        assert_eq!(serde_json::from_value::<ServerMessage>(j).unwrap(), e);
    }

    /// RFCOMM passthrough: the requests and the per-connection events
    #[test]
    fn rfcomm_wire_format() {
        let m = |j: serde_json::Value| serde_json::from_value::<Method>(j).unwrap();
        assert_eq!(
            m(serde_json::json!({"method": "rfcomm.open", "params": {"address": "A", "uuid": "1101"}})),
            Method::RfcommOpen { address: "A".into(), uuid: "1101".into() }
        );
        assert_eq!(
            m(serde_json::json!({"method": "rfcomm.send", "params": {"channel_id": 3, "data": "AQI="}})),
            Method::RfcommSend { channel_id: 3, data: "AQI=".into() }
        );
        assert_eq!(
            m(serde_json::json!({"method": "rfcomm.close", "params": {"channel_id": 3}})),
            Method::RfcommClose { channel_id: 3 }
        );
        assert_eq!(
            serde_json::to_value(RfcommOpened { channel_id: 3, mtu: 990 }).unwrap(),
            serde_json::json!({"channel_id": 3, "mtu": 990})
        );
        let e = ServerMessage::Event {
            rev: 5,
            event: Box::new(Event::RfcommData(RfcommData { channel_id: 3, data: "AQI=".into() })),
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"type": "event", "rev": 5,
                "event": {"kind": "rfcomm_data", "value": {"channel_id": 3, "data": "AQI="}}})
        );
        assert_eq!(serde_json::from_value::<ServerMessage>(j).unwrap(), e);
        let closed = Event::RfcommClosed(RfcommClosed { channel_id: 3, reason: "remote".into() });
        assert_eq!(
            serde_json::to_value(&closed).unwrap(),
            serde_json::json!({"kind": "rfcomm_closed", "value": {"channel_id": 3, "reason": "remote"}})
        );
    }

    /// The HID host's requests and state; an older core's settings
    /// without it read as off
    #[test]
    fn hid_wire_format() {
        let m = |j: serde_json::Value| serde_json::from_value::<Method>(j).unwrap();
        assert_eq!(m(serde_json::json!({"method": "hid.code"})), Method::HidCode);
        assert_eq!(
            m(serde_json::json!({"method": "hid.enable", "params": {
                "code": "123456", "other_input_at_hand": true, "risk_understood": false}})),
            Method::HidEnable { code: "123456".into(), other_input_at_hand: true, risk_understood: false }
        );
        assert_eq!(
            m(serde_json::json!({"method": "hid.add", "params": {"address": "A", "name": "K"}})),
            Method::HidAdd { address: "A".into(), name: "K".into(), le: None }
        );
        assert_eq!(
            m(serde_json::json!({"method": "hid.add", "params": {"address": "A", "name": "K", "le": 1}})),
            Method::HidAdd { address: "A".into(), name: "K".into(), le: Some(1) }
        );
        assert_eq!(
            m(serde_json::json!({"method": "hid.check_cancel", "params": {"address": "A"}})),
            Method::HidCheckCancel { address: "A".into() }
        );
        assert_eq!(m(serde_json::json!({"method": "hid.check_dismiss"})), Method::HidCheckDismiss);
        let check = HidCheck {
            address: "A".into(),
            name: "K".into(),
            kind: "keyboard".into(),
            waiting: true,
            remaining_ms: 1000,
            window_ms: 30000,
        };
        assert_eq!(
            serde_json::to_value(Event::HidCheck { checks: vec![check], rolled_back: None }).unwrap(),
            serde_json::json!({"kind": "hid_check", "value": {"checks": [{"address": "A", "name": "K",
                "kind": "keyboard", "waiting": true, "remaining_ms": 1000, "window_ms": 30000}], "rolled_back": null}})
        );
        assert_eq!(
            serde_json::to_value(Event::HidHealth(Some(HidHealth {
                state: "failed".into(),
                reason: Some("refused".into())
            })))
            .unwrap(),
            serde_json::json!({"kind": "hid_health", "value": {"state": "failed", "reason": "refused"}})
        );
        let e = Event::HidDevices(vec![HidDevice { address: "A".into(), name: "K".into(), connected: true, le: true }]);
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"kind": "hid_devices", "value": [{"address": "A", "name": "K", "connected": true, "le": true}]})
        );
        // An older core's list: classic devices
        let old: HidDevice =
            serde_json::from_value(serde_json::json!({"address": "A", "name": "K", "connected": false})).unwrap();
        assert!(!old.le);
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "language": "en", "theme": "system", "start_with_windows": false, "start_minimized": false,
            "minimize_to_tray": true, "check_updates_on_startup": true, "include_prereleases": false,
            "media_keys": true, "developer_mode": true, "bt_chip_pid": 0,
            "bt_chip_fw_stem": "", "afh": "auto", "remote_sink": ""}))
        .unwrap();
        assert!(!settings.hid_host);
        assert!(FEATURES.contains(&"hid-host"));
        let p = HidPasskey { address: "A".into(), passkey: "012345".into() };
        assert_eq!(
            serde_json::to_value(Event::HidPasskey(Some(p.clone()))).unwrap(),
            serde_json::json!({"kind": "hid_passkey", "value": {"address": "A", "passkey": "012345"}})
        );
        assert_eq!(
            serde_json::to_value(Event::HidPasskey(None)).unwrap(),
            serde_json::json!({"kind": "hid_passkey", "value": null})
        );
        assert!(!format!("{:?}", Event::HidPasskey(Some(p))).contains("012345"));
    }

    #[test]
    fn totals_since_a_baseline() {
        let now = LinkTotals {
            packets: 10,
            bytes: 1000,
            dropped: 2,
            capture_frames: 480,
            capture_dropped: 0,
            capture_discontinuities: 3,
            capture_lost: 960,
        };
        let base = LinkTotals {
            packets: 4,
            bytes: 400,
            dropped: 3,
            capture_frames: 0,
            capture_dropped: 0,
            capture_discontinuities: 1,
            capture_lost: 0,
        };
        assert_eq!(
            now.since(&base),
            LinkTotals {
                packets: 6,
                bytes: 600,
                dropped: 0,
                capture_frames: 480,
                capture_dropped: 0,
                capture_discontinuities: 2,
                capture_lost: 960,
            }
        );
    }
}
