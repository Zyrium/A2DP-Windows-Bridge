//! BlueCaster's core, shared by the resident process (tray, IPC for the UI) and
//! the command line: the streaming state machine, the Bluetooth stack's
//! lifecycle, and the settings, profiles and devices of the settings folder
//! (v1's files, read and written in their format).

#![forbid(unsafe_code)]

pub mod adapter;
pub mod auracast_mix;
pub mod compat;
pub mod default_input;
pub mod devices;
mod health;
pub mod hid;
pub mod le_sets;
mod le_stream;
pub mod link;
pub mod media_eq;
pub mod messages;
mod mic_test;
pub mod migrate;
pub mod paths;
pub mod profiles;
mod service;
pub mod service_data;
pub mod settings;
pub mod stream;
pub mod vendor;
mod voice;

pub use hid::{HidCounts, HidDevice};
pub use le_stream::{
    BROADCAST_PRESET_HQ, BROADCAST_PRESET_SQ, LeAudioPath, LeMicPath, LeStreamDetails, broadcast_config, le_mic_target,
    le_silence_set, le_stats_line, start_mic,
};
pub use messages::{CodecStop, CodecStopKind, Msg};
pub use service::{LeCliOptions, ScannedDevice, Service, ServiceEvent, ServiceOptions, VoiceChange};
pub use stream::{
    AacDetails, AacDeviceCaps, CodecDetails, CodecDeviceCaps, KnownCodecs, SbcDetails, SbcDeviceCaps, SourceInfo,
    SourceRequest, StreamInfo, StreamRequest, StreamState, VolumeSync,
};
pub use vendor::huawei::is_huawei;
pub use voice::{HeadsetMic, MicOutput, MicTarget, apply_mic_processing, mic_conflict, resolve_mic_output};
pub use voice_link::{MicDsp, VoiceCodec};
pub mod diag;
pub mod firmware;
pub mod mute_output;
pub mod probe_report;
pub mod receiver;
pub mod update;
pub mod volume_sync;
