//! An LE Audio (experimental) streaming session: v1's
//! A2dpService::le_audio_streaming_thread_inner (a2dp_service_le_audio.cpp),
//! on the session thread of [`crate::Service`] for a [`StreamRequest`] whose
//! `le` is Some.
//!
//! Same lifecycle as the A2DP session ([`crate::stream`]): refuse when the
//! setting is off (error.le_audio_disabled) or the capture mode is "app"
//! (error.le_audio_capture); BTstack up with `HostConfig::le_audio` (wait
//! up to 5 s for `BtHost::le_ready`, else error.le_audio_unavailable);
//! find and connect the device and the extra devices (`le_find` first for
//! the address type, then `le_connect`; error.le_audio_connect); the stream
//! request from `LeRequest` (preset -> preset order, low latency ->
//! `le_set_low_latency`); `le_start_stream` (error.le_audio_stream);
//! capture at the LC3 rate into pipeline's LE encode thread, which pushes
//! SDU sets into the returned sink; VolumeSync against
//! `le_remote_volume` / `le_request_volume` (the LeHeadphones target) when
//! a module knows a volume; on `le_stream_lost` stop and set the stream up
//! again ("LE Audio stream restored"; up to 10 times); on stop
//! `le_stop_stream` and `le_disconnect_all`.
//!
//! [`LeAudioPath`] (capture -> LE encode thread -> the stream's SDU sink) is
//! shared with the command line's LE Audio flow (cli.rs), as is
//! [`LeMicPath`] (the headset microphone of a conversational stream:
//! `LeRequest::headset_mic`; the stream's received SDUs -> pipeline's
//! le-decode thread -> the output [`crate::voice`] resolves: the voice
//! link's renderer at the source rate, or a WAV file).

use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_capture::{Capture, CaptureOptions, Format, Running, Sink, Source};
use bt_host::{BtHost, LeAddr, LeDeviceId, LeRequest, LeRxHandle, LeStreamRequest, LeStreamStarted, LinkStats};
use le_audio::broadcast::{self, BroadcastConfig};
use le_audio::defs::{LOCATION_FRONT_LEFT, LOCATION_FRONT_RIGHT};
use le_audio::plan::StreamPlan;
use le_audio::sdu_queue::SduSet;
use pipeline::le::{LePipeline, LePipelineConfig};
use pipeline::le_rx::{LeRxPipeline, MicCounters, PcmOut};
use pipeline::{BufferPreset, CaptureInput};
use voice_link::PcmProducer;

use crate::messages::Msg;
use crate::mute_output::OutputMute;
use crate::service::Inner;
use crate::stream::{
    DefaultDeviceGuard, DiscontinuityLog, SourceRequest, StreamInfo, StreamRequest, StreamState, VolumeSync,
};
use crate::voice::{MicOutput, MicTarget, mic_conflict, resolve_mic_output};

/// Waits for the LE core after BTstack started (v1: 50 x 100 ms)
const READY_WAIT: Duration = Duration::from_secs(5);
/// `le_find` before connecting, for the address type and name (v1 GUI)
const FIND_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The link to a device just seen: up by then, or looked for again (its
/// address may have changed), [`LINK_TRIES`] times in all
const LINK_TIMEOUT: Duration = Duration::from_secs(4);
const LINK_TRIES: u32 = 3;
/// Set-ups after a lost stream (v1's command line; its GUI tried 10 times,
/// up to 40 s each)
const RECONNECT_ATTEMPTS: u32 = 3;
const RECONNECT_WAIT: Duration = Duration::from_secs(3);
/// One scan for headphones that were turned off; short to see a stop
const OFF_FIND: Duration = Duration::from_secs(2);
/// [`Rejoin`]: the look for a set member missing from the stream
const REJOIN_FIRST: Duration = Duration::from_secs(3);
const REJOIN_EVERY: Duration = Duration::from_secs(4);
const REJOIN_MAX: Duration = Duration::from_secs(60);
const REJOIN_SCAN: Duration = Duration::from_millis(1500);
/// A member that leaves again this soon after it joined is waited for longer
/// ([`Rejoin`])
const REJOIN_SETTLE: Duration = Duration::from_secs(30);
/// CSIP's look for the other members at set-up when one that comes later
/// joins the stream anyway ([`member_wait`])
const MEMBER_WAIT: Duration = Duration::from_secs(3);
/// The same where a stream lost is set up again: the sound first, the other
/// member joins
const MEMBER_WAIT_RECONNECT: Duration = Duration::from_secs(1);
/// A CIS stalled within this of the start never got audio through: the
/// stream is set up again once
const STALLED_START: Duration = Duration::from_secs(5);
/// A member that left this recently may still advertise on its way off
const JUST_LEFT: Duration = Duration::from_secs(5);
/// How long the rest of a set is looked for when its last earbud left,
/// before Windows plays through the device before
const SWAP_FIND: Duration = Duration::from_secs(3);
/// Broadcast presets of stream.start_broadcast's qualities: high (48 kHz)
/// and standard (24 kHz)
pub use le_audio::broadcast::{DEFAULT_PRESET as BROADCAST_PRESET_HQ, STANDARD_PRESET as BROADCAST_PRESET_SQ};
/// The codec name the UI shows (v1)
pub const CODEC_NAME: &str = "LC3 (LE Audio)";

/// The silence set of a plan for `BtHost::le_start_stream`: one encoded
/// silent LC3 SDU per CIS
pub fn le_silence_set(plan: &StreamPlan) -> SduSet {
    pipeline::le::silence_set(plan)
}

/// v1's statistics line of the command line ("LE Audio: N SDU sets sent (M
/// SDUs), E encoded, S silence, U underruns, D dropped, X send errors")
pub fn le_stats_line(s: &bt_host::LeStreamStats) -> String {
    le_audio::sdu_queue::SendCounters {
        sets_encoded: s.sets_encoded,
        sets_sent: s.sets_sent,
        sdus_sent: s.sdus_sent,
        silence_sets: s.silence_sets,
        underruns: s.underruns,
        overflows: s.overflows,
        send_errors: s.send_errors,
    }
    .stats_message()
}

/// What an LE Audio stream was set up with (the UI's stream details)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeStreamDetails {
    /// BAP preset ("48_4_2"; GMAP low latency "48_4_gr")
    pub preset: String,
    pub frame_us: u32,
    /// LC3 octets per channel and frame
    pub octets: u32,
    /// Connected isochronous streams
    pub cis: u32,
    /// Devices streamed to (both earbuds of a coordinated set: 2)
    pub devices: u32,
    /// The GMAP low-latency configuration
    pub low_latency: bool,
    /// English, v1's StreamPlan::describe
    pub summary: String,
}

impl LeStreamDetails {
    pub fn of(started: &LeStreamStarted) -> LeStreamDetails {
        let status = &started.status;
        let plan = &status.plan;
        // Each configuration (a matrix broadcast's rear may run at another)
        let mut presets: Vec<&str> = Vec::new();
        for c in plan.sink_cis() {
            let name = plan.preset_of(c).name.as_str();
            if !presets.contains(&name) {
                presets.push(name);
            }
        }
        LeStreamDetails {
            preset: if presets.is_empty() { plan.preset.name.clone() } else { presets.join(" + ") },
            frame_us: plan.preset.frame_us.into(),
            octets: plan.max_octets().into(),
            cis: plan.cis.len() as u32,
            devices: status.devices.len() as u32,
            low_latency: plan.preset.name.ends_with("_gr"),
            summary: status.summary.clone(),
        }
    }
}

/// The capture's end of the LE encode thread's ring
struct InputSink(CaptureInput);

impl Sink for InputSink {
    fn audio(&mut self, data: &[u8], _frames: usize, _format: Format) {
        self.0.push(data);
    }

    fn silence(&mut self, frames: usize, _format: Format) {
        self.0.push_silence(frames);
    }

    fn discontinuity(&mut self, lost_frames: Option<u64>) {
        self.0.discontinuity(lost_frames);
    }
}

/// Audio into a running LE stream: capture (or the test tone) at the LC3
/// rate (WASAPI converts to it), the LE encode thread (downmix, resample if
/// still needed, LC3 per channel), the SDU sets into the stream's sink
pub struct LeAudioPath {
    capture: Option<Running>,
    pipeline: Option<LePipeline>,
    format: Format,
    rate: u32,
    channel_mask: u32,
}

impl LeAudioPath {
    /// The capture source for `source` (application capture is not
    /// supported with LE Audio: error.le_audio_capture before this)
    fn capture_source(source: &SourceRequest) -> Result<Source, String> {
        match source {
            SourceRequest::TestTone => Ok(Source::TestTone),
            SourceRequest::SystemLoopback => Ok(Source::SystemLoopback),
            SourceRequest::VirtualDevice { id, .. } if !id.is_empty() => Ok(Source::RenderDevice { id: id.clone() }),
            SourceRequest::VirtualDevice { .. } => Err("no virtual device selected".into()),
            SourceRequest::OutputDevice { id, .. } if !id.is_empty() => Ok(Source::RenderDevice { id: id.clone() }),
            SourceRequest::OutputDevice { .. } => Err("no output device selected".into()),
            SourceRequest::Application { .. } => Err("application capture is not supported with LE Audio".into()),
        }
    }

    /// Starts capture and encoding for the stream `started` (`mix`: the
    /// broadcast's matrix mixer, else the fixed downmix)
    pub fn start(
        source: &SourceRequest,
        started: &LeStreamStarted,
        stats: Arc<LinkStats>,
        mix: Option<pipeline::mix::MixHandle>,
    ) -> Result<LeAudioPath, String> {
        let plan = &started.status.plan;
        let rate = plan.top_sampling_hz();
        let test_tone = matches!(source, SourceRequest::TestTone);
        let options = CaptureOptions { preferred_rate: Some(rate), fill_silence: !test_tone };
        let capture = Capture::open(&LeAudioPath::capture_source(source)?, &options).map_err(|e| e.to_string())?;
        let format = capture.format();
        let channel_mask = capture.channel_mask();
        tracing::info!(
            "LE Audio: capture {} Hz, {} channel(s), {:?} -> LC3 at {rate} Hz",
            format.sample_rate,
            format.channels,
            format.sample_format
        );
        let (pipeline, input) = LePipeline::start(
            LePipelineConfig {
                plan: plan.clone(),
                input: crate::stream::input_format(format),
                channel_mask,
                buffer: BufferPreset::Standard,
                media_eq: Some(crate::media_eq::handle()),
                mix,
            },
            started.sink.clone(),
            stats,
        )?;
        let running = match capture.start(Box::new(InputSink(input))) {
            Ok(r) => r,
            Err(e) => {
                pipeline.stop();
                return Err(e.to_string());
            }
        };
        Ok(LeAudioPath { capture: Some(running), pipeline: Some(pipeline), format, rate, channel_mask })
    }

    /// The captured speakers (dwChannelMask)
    pub fn channel_mask(&self) -> u32 {
        self.channel_mask
    }

    /// Both channels' mix on every channel (the earbud left alone) or the
    /// plan's channels again
    pub fn set_mono(&self, on: bool) {
        if let Some(p) = &self.pipeline {
            p.set_mono(on);
        }
    }

    /// The captured format
    pub fn format(&self) -> Format {
        self.format
    }

    /// The LC3 rate
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Gaps WASAPI reported in the captured audio
    pub fn discontinuities(&self) -> u64 {
        self.capture.as_ref().map_or(0, Running::discontinuities)
    }

    /// Producer first, then the encode thread
    pub fn stop(mut self) {
        self.stop_now();
    }

    fn stop_now(&mut self) {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        if let Some(p) = self.pipeline.take() {
            p.stop();
        }
    }
}

impl Drop for LeAudioPath {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// The le-decode thread's output: the voice link's queue in front of the
/// [`MicOutput`] (the renderer's drift control absorbs the headset's clock;
/// the decode thread has none of its own)
struct ProducerOut(PcmProducer);

impl PcmOut for ProducerOut {
    fn push(&mut self, mono_i16: &[i16]) {
        // What does not fit counts as the queue's overflow
        self.0.push(mono_i16);
    }
}

/// The headset microphone of a running LE stream: the received SDUs
/// decoded on pipeline's le-decode thread into the [`MicOutput`] (rendered
/// at the source rate, or a WAV file)
pub struct LeMicPath {
    pipeline: Option<LeRxPipeline>,
    output: Option<MicOutput>,
    target: MicTarget,
    rate: u32,
}

impl LeMicPath {
    /// Opens `target` at the stream's source rate and starts decoding into
    /// it, through the processing `dsp` asks for
    pub fn start(rx: LeRxHandle, target: &MicTarget, dsp: voice_link::MicDsp) -> Result<LeMicPath, String> {
        let rate = rx.sample_rate;
        let mut output = MicOutput::open(target, rate)?;
        let mut producer = output.producer().ok_or("the microphone output has no producer")?;
        producer.set_processing(dsp, rate);
        let pipeline = LeRxPipeline::start(rx, Box::new(ProducerOut(producer)))?;
        tracing::info!("LE Audio: microphone to {target} at {rate} Hz");
        Ok(LeMicPath { pipeline: Some(pipeline), output: Some(output), target: target.clone(), rate })
    }

    /// The microphone's sampling rate
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn target(&self) -> &MicTarget {
        &self.target
    }

    /// What the UI shows while it runs
    pub fn status(&self) -> ipc::VoiceStatus {
        ipc::VoiceStatus {
            codec: "lc3".into(),
            link: "le".into(),
            rate: self.rate,
            mic_device_id: self.target.device_id().to_string(),
        }
    }

    /// The render device went away or failed: why
    pub fn ended(&self) -> Option<String> {
        self.output.as_ref().and_then(MicOutput::ended)
    }

    pub fn stats(&self) -> MicCounters {
        self.pipeline.as_ref().map(LeRxPipeline::stats).unwrap_or_default()
    }

    /// Ends the decode thread, then the output (a WAV file is finished)
    pub fn stop(mut self) -> MicCounters {
        self.halt()
    }

    fn halt(&mut self) -> MicCounters {
        let counters = self.pipeline.take().map(LeRxPipeline::stop).unwrap_or_default();
        if let Some(o) = self.output.take() {
            tracing::debug!("LE Audio: microphone output: {}", o.describe());
        }
        counters
    }
}

impl Drop for LeMicPath {
    fn drop(&mut self) {
        self.halt();
    }
}

/// The microphone of a just started stream into `target` (the `stream`th of
/// the session: [`MicTarget::nth`]). Ok(None): the stream has none (none
/// asked for, or the plan found none); Err: the output did not open.
pub fn start_mic(
    started: &mut LeStreamStarted,
    target: &MicTarget,
    stream: u32,
    dsp: voice_link::MicDsp,
) -> Result<Option<LeMicPath>, String> {
    let Some(rx) = started.rx.take() else { return Ok(None) };
    LeMicPath::start(rx, &target.nth(stream), dsp).map(Some).inspect_err(|e| {
        tracing::error!("LE Audio: microphone: {e}");
    })
}

/// Where the microphone of an LE request goes, checked before the stream is
/// set up (`headset_mic`: the request's mode is not off; Auto is On for LE
/// Audio, whose conversational stream keeps the media preset): Ok(None)
/// when none is asked for; Err the status message when it
/// cannot be (no output device, or one that feeds the capture: the stream
/// then plays without the microphone)
pub fn le_mic_target(
    headset_mic: bool,
    mic_output: Option<&str>,
    source: &SourceRequest,
) -> Result<Option<MicTarget>, Msg> {
    if !headset_mic {
        return Ok(None);
    }
    let Some(target) = resolve_mic_output(mic_output) else {
        tracing::warn!("LE Audio: no output device for the microphone, streaming without it");
        return Err(Msg::new("status.headset_mic_output_failed"));
    };
    if let Some(key) = mic_conflict(&target, source) {
        tracing::warn!("LE Audio: the microphone output {target} feeds the stream ({source}), streaming without it");
        return Err(Msg::new(key));
    }
    Ok(Some(target))
}

/// The stream request of an [`LeRequest`]: an explicit preset is the only
/// one tried. GMAP low latency: the request's flag or (from a profile) the
/// setting, not with an explicit preset. The headset microphone asks for
/// the conversational configuration.
/// Developer > Debug for LE Audio streams (service.rs apply_dev_overrides)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LeDebug {
    /// The microphone's preset, taken as it is; empty: as planned
    pub mic_preset: String,
    /// The retry without the microphone stays on the same link
    pub retry_same_link: bool,
}

static LE_DEBUG: std::sync::Mutex<LeDebug> =
    std::sync::Mutex::new(LeDebug { mic_preset: String::new(), retry_same_link: false });

pub(crate) fn set_debug(d: LeDebug) {
    *LE_DEBUG.lock().unwrap_or_else(|e| e.into_inner()) = d;
}

fn debug() -> LeDebug {
    LE_DEBUG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Developer > Debug's microphone preset into the request
fn apply_debug(request: &mut LeStreamRequest, d: &LeDebug) {
    if !d.mic_preset.is_empty() {
        request.source_preset_order = vec![d.mic_preset.clone()];
        request.source_preset_exact = true;
    }
}

pub(crate) fn stream_request(le: &LeRequest) -> (LeStreamRequest, bool) {
    let mut request = LeStreamRequest::default();
    if let Some(p) = &le.preset {
        request.preset_order = vec![p.clone()];
    }
    request.conversational = le.headset_mic.is_on();
    request.source_rate = le.mic_rate;
    // The profile's choice (or --le-low-latency); an explicit preset wins
    let low_latency = le.low_latency && le.preset.is_none();
    (request, low_latency)
}

/// Why a session refuses an LE request before BTstack is touched
pub(crate) fn refusal(le_enabled: bool, developer: bool, source: &SourceRequest) -> Option<&'static str> {
    if !le_enabled {
        // Its switch is there in developer mode only (issue #86)
        Some(if developer { "error.le_audio_disabled" } else { "error.le_audio_developer" })
    } else if matches!(source, SourceRequest::Application { .. }) {
        Some("error.le_audio_capture")
    } else {
        None
    }
}

/// LE Audio is off and `req` is an LE Audio device's: when its LE pairing
/// gave a classic link key (cross-transport key derivation, stored under its
/// public address), the classic request for it, with its classic side saved
/// so it is listed with LE Audio off. Else None (it was paired over LE
/// only, without a classic key: refused).
fn classic_instead(config_dir: &std::path::Path, req: &StreamRequest) -> Option<StreamRequest> {
    let keys = bt_host::link_keys::FileLinkKeyStore::open(&crate::paths::link_keys_file(config_dir));
    if !keys.entries().iter().any(|e| e.addr == req.device) {
        return None;
    }
    crate::devices::save_classic_sides(config_dir, keys.entries());
    Some(StreamRequest { le: None, codec: None, codec_forced: false, ..req.clone() })
}

/// The session thread for `req` (`req.le` is Some). Returns when stopped or
/// on an error (state reported), as `stream::run` does.
pub(crate) fn run(inner: &Arc<Inner>, req: StreamRequest) {
    if req.le.as_ref().is_some_and(|l| l.broadcast.is_some()) {
        return run_broadcast(inner, req);
    }
    let mut le = req.le.clone().unwrap_or_default();
    let addr = bt_host::addr_str(&req.device);
    tracing::info!("LE Audio stream to {addr} ({})", crate::diag::quoted(&req.device_name));
    tracing::debug!("le stream: request {:?}", req.redacted());
    let stopped = || inner.stop_requested();
    let ready = || inner.notify(StreamState::Idle, Msg::new("status.ready"));
    let session_start = Instant::now();

    if let Some(key) = refusal(inner.le_wanted(), inner.developer(), &req.source) {
        if key == "error.le_audio_disabled"
            && let Some(classic) = classic_instead(inner.config_dir(), &req)
        {
            tracing::info!("LE Audio is off: streaming over classic with the link key of the LE pairing");
            return crate::stream::run(inner, classic);
        }
        tracing::debug!("le stream: refused ({key}), LE wanted {}, source {}", inner.le_wanted(), req.source);
        return inner.notify(StreamState::Error, Msg::new(key));
    }

    inner.notify(StreamState::Connecting, Msg::new("status.initializing_btstack"));
    let host = match inner.ensure_host() {
        Ok(h) => h,
        Err(m) => return inner.notify(StreamState::Error, m),
    };
    let wait = Instant::now();
    let until = wait + READY_WAIT;
    while !host.le_ready() && Instant::now() < until && !stopped() {
        inner.wait_tick(Duration::from_millis(100));
    }
    tracing::debug!(
        "le stream: LE core ready {} after {} ms (stack up {} ms after the request)",
        host.le_ready(),
        wait.elapsed().as_millis(),
        wait.duration_since(session_start).as_millis()
    );
    if stopped() {
        return ready();
    }
    if !host.le_ready() {
        return inner.notify(StreamState::Error, Msg::new("error.le_audio_unavailable"));
    }

    // The headset microphone: its output is checked before the stream asks
    // for the conversational configuration; refused, the stream plays
    // without it and the status says why
    let (mic_target, mic_note) = match le_mic_target(le.headset_mic.is_on(), le.mic_output.as_deref(), &req.source) {
        Ok(t) => (t, None),
        Err(m) => (None, Some(m)),
    };
    if mic_target.is_none() {
        le.headset_mic = bt_host::HeadsetMic::Off;
    }
    let mut mic_note = mic_note;
    let (mut request, low_latency) = stream_request(&le);
    let le_debug = debug();
    apply_debug(&mut request, &le_debug);
    if le_debug != LeDebug::default() {
        tracing::info!("LE Audio: Developer > Debug: {le_debug:?}");
    }
    tracing::debug!("le stream: stream request {request:?}, low latency {low_latency}");
    host.le_set_low_latency(low_latency);
    let mut devices = vec![req.device];
    devices.extend(le.extra_devices.iter().copied());
    tracing::debug!("le stream: devices {:?}", devices.iter().map(bt_host::addr_str).collect::<Vec<_>>());

    // Connect (the address type comes from the advertisement) and stream.
    // A stream with the microphone the headphones refuse plays without it
    // (issue #86: the microphone kept LE Audio from connecting at all), and
    // so do the reconnects after it.
    let mic_refused = std::cell::Cell::new(false);
    // `skip`: members of the set on their way off, left out (they join later)
    let connect_and_stream = |how: SetUp, skip: &[[u8; 6]]| -> Result<LeStreamStarted, Msg> {
        // (a rejoin keeps the status "earbud back" its caller shows)
        if how != SetUp::Rejoin {
            inner.notify(StreamState::Connecting, Msg::new("status.connecting_device"));
        }
        let t = Instant::now();
        let others = other_set_members(inner, &host, req.device, &devices);
        host.le_set_member_wait(match skip {
            [] => member_wait(inner.auto_reconnect(), devices.len() + others.len(), how),
            _ => Some(Duration::ZERO),
        });
        let ids = connect_devices(&host, &devices, &others, skip, &req.device_name, &stopped)?;
        tracing::debug!("le stream: connected {ids:?} in {} ms, starting the stream", t.elapsed().as_millis());
        let t = Instant::now();
        let mut asked = request.clone();
        asked.conversational &= !mic_refused.get();
        let mut r = host.le_start_stream(ids.clone(), asked.clone(), &le_silence_set);
        if let Err(e) = &r
            && asked.conversational
            && how != SetUp::Rejoin
            && !stopped()
        {
            tracing::warn!("LE Audio: the stream with the microphone failed ({e}): trying without it");
            host.le_stop_stream();
            mic_refused.set(true);
            asked.conversational = false;
            // On a new link: right after the refused CIS, EarFun earbuds
            // left ASE 2 in Enabling on the same link, while a fresh
            // connection streamed every time (issue #86). Developer > Debug
            // can keep the link, to compare.
            let ids = if le_debug.retry_same_link {
                ids
            } else {
                host.le_disconnect_all();
                connect_devices(&host, &devices, &others, skip, &req.device_name, &stopped)?
            };
            r = host.le_start_stream(ids, asked, &le_silence_set);
        }
        let r = r.map_err(|e| {
            tracing::debug!("le stream: start failed after {} ms: {e}", t.elapsed().as_millis());
            host.le_stop_stream();
            Msg::with("error.le_audio_stream", &[&e])
        });
        if let Ok(s) = &r {
            tracing::debug!(
                "le stream: started in {} ms: preset {}, {} CIS, {} device(s)",
                t.elapsed().as_millis(),
                s.status.plan.preset.name,
                s.status.plan.cis.len(),
                s.status.devices.len()
            );
        }
        r
    };
    // The stream to devices connected still (those that played before a
    // member back failed to join, or all of it again): nothing is looked
    // for or connected
    let stream_to = |ids: Vec<LeDeviceId>| -> Result<LeStreamStarted, Msg> {
        host.le_set_member_wait(Some(Duration::ZERO));
        let mut asked = request.clone();
        asked.conversational &= !mic_refused.get();
        host.le_start_stream(ids, asked, &le_silence_set).map_err(|e| {
            host.le_stop_stream();
            Msg::with("error.le_audio_stream", &[&e])
        })
    };
    let mut started = match connect_and_stream(SetUp::First, &[]) {
        Ok(s) => s,
        Err(m) => {
            host.le_disconnect_all();
            return if stopped() { ready() } else { inner.notify(StreamState::Error, m) };
        }
    };
    if mic_refused.get() {
        mic_note = Some(Msg::new("status.headset_mic_failed"));
    }

    // Audio at the LC3 rate
    let switch_default = || match req.source.default_switch_device() {
        Some((id, name)) => DefaultDeviceGuard::switch_to(id, name),
        None => DefaultDeviceGuard(None),
    };
    let mut default_guard = switch_default();
    // Restored while the headphones are off: switched again when they are back
    let mut switch_default_again = false;
    let mut audio = match LeAudioPath::start(&req.source, &started, host.stats().clone(), None) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::error!("LE Audio: audio capture: {e}");
            host.le_stop_stream();
            host.le_disconnect_all();
            drop(default_guard);
            return inner.notify(StreamState::Error, Msg::new("error.wasapi_init"));
        }
    };
    // The headset microphone (a conversational stream)
    let mut mic_streams = 1;
    let mut mic = open_mic(inner, &mut started, mic_target.as_ref(), mic_streams, &mut mic_note);

    let report = |started: &LeStreamStarted, audio: Option<&LeAudioPath>, note: Option<&Msg>| {
        // Which earbuds of a set play (a member may be left out: not found,
        // or its link went down)
        inner.notify(StreamState::Streaming, note.cloned().unwrap_or_else(|| set_status(&host, started, &[], 0)));
        let details = LeStreamDetails::of(started);
        let plan = &started.status.plan;
        let channels = plan.channel_count() as u32;
        let rate = plan.preset.sampling_hz;
        let format = audio.map(LeAudioPath::format);
        inner.set_le_stream(Some(details));
        inner.mic_rates(plan.source.as_ref().map(|s| s.rates.clone()).unwrap_or_default());
        inner.stream_info(StreamInfo {
            codec: CODEC_NAME.into(),
            bitrate_kbps: plan.preset.kbps_per_channel() * channels,
            sample_rate: rate,
            channels,
            bits: 0,
            source_sample_rate: format.map_or(rate, |f| f.sample_rate),
            source_channels: format.map_or(2, |f| f.channels as u32),
            source_bits: format.map_or(16, |f| (f.sample_format.bytes_per_sample() * 8) as u32),
            requested_codec: None,
            requested_in_use: false,
            abr: false,
            bitrate_capped: false,
            hi_res_set: false,
            low_latency: false,
            source: req.source.info(),
            details: None,
        });
        tracing::info!("LE Audio streaming: {}", started.status.summary);
    };
    report(&started, audio.as_ref(), mic_note.as_ref());
    // The set first: the other earbud is never saved as a device of its own
    inner.learn_le_set(&host, &req.device, &started.status.devices, le.extra_devices.is_empty());
    inner.save_le_device(&addr, &req.device_name);

    // Windows volume <-> headset volume through the VCP module
    let sync_volume = req.source != SourceRequest::TestTone && req.volume_sync != VolumeSync::Off;
    let mut volume = crate::volume_sync::EndpointChoice::for_stream(&req.volume_sync, &req.source)
        .map(crate::volume_sync::VolumeSyncer::new);
    let mutes_output = req.mutes_output(&req.source);
    let mut output_mute = OutputMute::new();
    output_mute.tick(mutes_output);

    let mut lost_for_good = false;
    let mut discontinuities = DiscontinuityLog::default();
    // The headset volume (VCP) for the volume slider (issue #86: disabled
    // while LE Audio streamed), as AVRCP's reports are for classic devices
    let mut shown_volume = None;
    // CIS of the stream that get no SDU through while the others play (an
    // earbud whose controller queue never empties)
    let mut shown_stalled = 0;
    // Devices of the stream that went away while the others play on (an
    // earbud put in its case), and when to look for the missing ones
    let mut shown_dropped = 0;
    let mut rejoin = Rejoin::new(Instant::now());
    let mut joining: Option<Joining> = None;
    // The stream was lost (while a member back was connected: handled once
    // that ended), and whether the status says so
    let mut lost = false;
    let mut shown_lost = false;
    // The last earbud that streamed ended its ASE and was let go
    let mut let_go = false;
    // The set members that left the stream, and when
    let mut left: Vec<([u8; 6], Instant)> = Vec::new();
    // When the stream started, whether it is to be set up again (a CIS got
    // no audio through from the start) and whether it just was
    let mut stream_at = Instant::now();
    let mut rebuild = false;
    let mut rebuilt = false;
    'session: while !stopped() {
        inner.wait_tick(Duration::from_millis(200));
        if stopped() {
            break;
        }
        // The flag after the count: a stream lost ends its sending, which
        // looks like no stalled CIS and is no reason to say "Connected". The
        // devices after the flag: the one that ended the stream is in there.
        let stalled = host.le_stream_stats().cis_stalled;
        lost |= host.le_stream_lost();
        let dropped = host.le_stream_dropped();
        if stalled != shown_stalled && !lost {
            shown_stalled = stalled;
            if stalled > 0 && !rebuilt && stream_at.elapsed() < STALLED_START && inner.auto_reconnect() {
                // Seen with several earbuds and headphones; the next set-up
                // mostly worked
                tracing::warn!(
                    "LE Audio: {stalled} CIS got no audio through from the start, setting the stream up again"
                );
                rebuild = true;
            } else if stalled > 0 {
                tracing::warn!("LE Audio: {stalled} CIS get no audio through, the others play on");
                inner.notify(StreamState::Streaming, set_status(&host, &started, &dropped, stalled));
            } else {
                tracing::info!("LE Audio: every CIS gets audio through again");
                rebuild = false;
                inner.notify(
                    StreamState::Streaming,
                    mic_note.clone().unwrap_or_else(|| set_status(&host, &started, &dropped, 0)),
                );
            }
        }
        if let Some(v) = volume.as_mut() {
            v.tick(sync_volume, &crate::volume_sync::LeHeadphones(&host));
        }
        if let Some(level) = host.le_remote_volume().filter(|&l| Some(l) != shown_volume) {
            shown_volume = Some(level);
            inner.send_event(crate::service::ServiceEvent::Volume(level));
        }
        output_mute.tick(mutes_output);
        discontinuities.tick(audio.as_ref().map_or(0, LeAudioPath::discontinuities));
        if let Some(why) = mic.as_ref().and_then(LeMicPath::ended) {
            // The stream goes on without it until it is set up again
            tracing::warn!("LE Audio: the microphone output ended ({why}), streaming without it");
            stop_mic(inner, &mut mic);
            mic_note = Some(Msg::new("status.headset_mic_output_failed"));
            inner.notify(StreamState::Streaming, Msg::new("status.headset_mic_output_failed"));
        }
        // With none left the stream is lost: the flag may have come after it
        // was read above, for the next time
        let playing = started.status.devices.iter().filter(|d| !dropped.contains(d)).count();
        if dropped.len() != shown_dropped && (lost || playing > 0) {
            shown_dropped = dropped.len();
            tracing::info!("LE Audio: {shown_dropped} device(s) left the stream, {playing} play on");
            // The one left alone gets both channels
            if let Some(a) = audio.as_ref() {
                a.set_mono(playing <= 1);
            }
            let known = host.le_devices();
            for d in known.iter().filter(|d| dropped.contains(&d.id)) {
                if !left.iter().any(|(a, _)| *a == d.addr.addr) {
                    left.push((d.addr.addr, Instant::now()));
                }
            }
            // One whose link stayed up (its CIS or ASE ended) is let go: it
            // comes back advertising, as one taken out of its case (not the
            // one being connected again). Headphones that ended the stream
            // keep their link.
            let joins = |a: &[u8; 6]| joining.as_ref().is_some_and(|j| j.missing.contains(a));
            let in_set =
                !lost || devices.len() > 1 || !other_set_members(inner, &host, req.device, &devices).is_empty();
            for d in known.iter().filter(|d| in_set && d.connected && dropped.contains(&d.id) && !joins(&d.addr.addr)) {
                tracing::info!("LE Audio: disconnecting {}, it left the stream", d.addr);
                host.le_disconnect(d.id);
                let_go |= lost;
            }
            if !lost {
                inner.notify(StreamState::Streaming, set_status(&host, &started, &dropped, stalled));
            }
            rejoin.look_soon(Instant::now());
        }
        let joined = if let Some(j) = joining.take_if(|j| j.thread.is_finished()) {
            Some(j)
        } else if joining.is_some() {
            if lost && !shown_lost {
                // The member may still bring the sound back: waited for, but
                // not without a word (it took 9.5 s on a WF-1000XM6)
                shown_lost = true;
                tracing::warn!("LE Audio stream lost while the missing member is looked for or connected");
                inner.notify(StreamState::Reconnecting, Msg::new("status.connection_lost"));
            }
            continue;
        } else {
            None
        };
        let mut member_back = false;
        if joined.is_none()
            && !lost
            && inner.auto_reconnect()
            && (shown_dropped > 0 || host.le_set_members_missing() > 0)
            && !rebuild
            && rejoin.due(Instant::now())
        {
            // The other earbud out of its case again is connected while the
            // one there plays on (a failed connection costs it nothing),
            // then the stream is set up again with both (a short gap on the
            // one that played)
            let missing = missing_members(inner, &host, req.device, &devices, &started, &dropped);
            rejoin.looked(Instant::now(), false);
            // One connected already does not advertise: no scan finds it, the
            // stream is set up with it (a failure lets it go)
            let linked: Vec<String> = host
                .le_devices()
                .iter()
                .filter(|d| d.connected && missing.contains(&d.addr.addr))
                .map(|d| d.addr.to_string())
                .collect();
            if linked.is_empty() {
                joining = Joining::start(inner, &host, missing);
            } else {
                tracing::info!(
                    "LE Audio: {} of the set connected but not in the stream, setting the stream up with it",
                    linked.join(", ")
                );
                member_back = true;
            }
        }
        // The member back, connected
        match joined.map(|j| j.thread.join().unwrap_or(Joined::NotSeen)) {
            None | Some(Joined::NotSeen) => {}
            Some(Joined::Connected) => member_back = true,
            Some(Joined::Failed) => rejoin.restored(Instant::now(), false),
        }
        if stopped() {
            break;
        }
        if !lost && !member_back && !rebuild {
            continue;
        }

        // Set up again with a member back, or as it was (a CIS that got no
        // audio through from the start)
        if member_back || (rebuild && !lost) {
            rebuild = false;
            inner.notify(
                StreamState::Connecting,
                Msg::new(if member_back { "status.le_set_member_back" } else { "status.connecting_device" }),
            );
            if let Some(a) = audio.take() {
                a.stop();
            }
            // Those that play now: what a failure falls back to
            let gone = host.le_stream_dropped();
            let played: Vec<LeDeviceId> =
                started.status.devices.iter().copied().filter(|d| !gone.contains(d)).collect();
            host.le_stop_stream();
            stop_mic(inner, &mut mic);
            rebuilt = !member_back;
            let again = match (member_back, lost) {
                (true, false) => connect_and_stream(SetUp::Rejoin, &[]),
                // Nothing plays meanwhile
                (true, true) => connect_and_stream(SetUp::Reconnect, &[]),
                (false, _) => stream_to(played.clone()),
            };
            let set_up = match again {
                Ok(s) => {
                    tracing::info!("LE Audio stream set up again: {}", s.status.summary);
                    Some(s)
                }
                Err(_) if stopped() => break,
                Err(m) => {
                    tracing::warn!("LE Audio: setting the stream up again failed: {}", m.english());
                    // Back to the one that played; the member that failed is
                    // let go (it comes back advertising). Where the one that
                    // played went (the earbuds were swapped), the member back
                    // plays alone.
                    let linked: Vec<LeDeviceId> =
                        host.le_devices().into_iter().filter(|d| d.connected).map(|d| d.id).collect();
                    let keep = fall_back_to(&played, &linked);
                    for &id in linked.iter().filter(|id| !keep.contains(id)) {
                        host.le_disconnect(id);
                    }
                    if keep.is_empty() {
                        tracing::warn!("LE Audio: no earbud of the stream is connected any more");
                        None
                    } else {
                        tracing::info!("LE Audio: setting the stream up with {keep:?} only (played: {played:?})");
                        match stream_to(keep) {
                            Ok(s) => {
                                tracing::info!("LE Audio: the stream plays on one earbud: {}", s.status.summary);
                                Some(s)
                            }
                            Err(_) if stopped() => break,
                            Err(m) => {
                                // As a stream lost: the reconnect attempts below
                                tracing::warn!("LE Audio: the stream on one earbud failed too: {}", m.english());
                                None
                            }
                        }
                    }
                }
            };
            match set_up {
                Some(s) => {
                    started = s;
                    (shown_dropped, shown_stalled) = (0, 0);
                    (lost, shown_lost, let_go) = (false, false, false);
                    left.clear();
                    stream_at = Instant::now();
                    rejoin.restored(Instant::now(), host.le_set_members_missing() == 0);
                    mic_streams += 1;
                    mic = open_mic(inner, &mut started, mic_target.as_ref(), mic_streams, &mut mic_note);
                    audio = match LeAudioPath::start(&req.source, &started, host.stats().clone(), None) {
                        Ok(a) => Some(a),
                        Err(e) => {
                            tracing::error!("LE Audio: audio capture: {e}");
                            inner.notify(StreamState::Error, Msg::new("error.wasapi_init"));
                            lost_for_good = true;
                            break;
                        }
                    };
                    report(&started, audio.as_ref(), mic_note.as_ref());
                    continue;
                }
                None => rejoin.restored(Instant::now(), false),
            }
        }

        // A device or CIS went away: tear down and set it up again
        tracing::debug!("le stream: lost after {} s of the session", session_start.elapsed().as_secs());
        if !inner.auto_reconnect() {
            tracing::info!("LE Audio stream lost: reconnecting is off, the session ends");
            // Not connected again by a Bluetooth restart either
            inner.waiting_for_device();
            inner.done_waiting_for_device(false);
            inner.notify(StreamState::Idle, Msg::new("status.device_off"));
            lost_for_good = true;
            break;
        }
        // Either earbud of a set may be the one the session connected
        let mut members = devices.clone();
        members.extend(other_set_members(inner, &host, req.device, &devices));
        let playing: Vec<LeDeviceId> = started.status.devices.clone();
        for d in host.le_devices().iter().filter(|d| playing.contains(&d.id)) {
            if !left.iter().any(|(a, _)| *a == d.addr.addr) {
                left.push((d.addr.addr, Instant::now()));
            }
        }
        // The last earbud let go: as one that ended its link itself
        let off = (let_go && !host.le_devices().iter().any(|d| d.connected && members.contains(&d.addr.addr)))
            || device_turned_off(inner, &host, &members);
        if !off {
            tracing::warn!("LE Audio stream lost, reconnecting");
            if !shown_lost {
                inner.notify(StreamState::Reconnecting, Msg::new("status.connection_lost"));
            }
        }
        if let Some(a) = audio.take() {
            a.stop();
        }
        host.le_stop_stream();
        stop_mic(inner, &mut mic);
        let mut lost_state = StreamState::Reconnecting;
        let mut first_wait = RECONNECT_WAIT;
        // Another earbud of the set may be out already (they were swapped):
        // the sound goes to it at once
        let others: Vec<[u8; 6]> = members.iter().copied().filter(|a| !just_left(&left, a)).collect();
        let mut swapped = off && {
            inner.notify(StreamState::Idle, Msg::new("status.device_off"));
            !others.is_empty() && host.le_find_any(&others, SWAP_FIND).is_some()
        };
        if stopped() {
            break;
        }
        // Those on their way off are not the ones to connect
        let just_gone = || -> Vec<[u8; 6]> { members.iter().copied().filter(|a| just_left(&left, a)).collect() };
        let mut skip: Vec<[u8; 6]> = Vec::new();
        let mut restored = None;
        loop {
            if swapped {
                tracing::info!("LE Audio: the other earbud is there, going on with it");
                first_wait = Duration::ZERO;
                lost_state = StreamState::Connecting;
                skip = just_gone();
            } else if off {
                // As the A2DP stream: nothing to retry until they advertise again
                tracing::info!("the headphones were turned off or disconnected themselves");
                // Windows plays through the device before meanwhile (#132)
                if default_guard.0.is_some() {
                    default_guard = DefaultDeviceGuard(None);
                    switch_default_again = true;
                }
                output_mute.tick(false);
                let Some(back) = wait_for_device(inner, &host, &members) else {
                    lost_for_good = true;
                    break 'session;
                };
                tracing::info!("the headphones are back, resuming");
                // An earbud of a set that did not just go is no one on its way off
                if members.len() > 1 && !just_left(&left, &back) {
                    first_wait = Duration::ZERO;
                    skip = just_gone();
                }
                lost_state = StreamState::Connecting;
                inner.notify(lost_state, Msg::new("status.device_back"));
            }
            for attempt in 1..=RECONNECT_ATTEMPTS {
                if stopped() {
                    break;
                }
                inner.notify(lost_state, Msg::with("status.reconnect_attempt", &[&attempt, &RECONNECT_ATTEMPTS]));
                let wait = if attempt == 1 { first_wait } else { RECONNECT_WAIT };
                tracing::debug!(
                    "le stream: reconnect attempt {attempt}/{RECONNECT_ATTEMPTS} in {} ms",
                    wait.as_millis()
                );
                let until = Instant::now() + wait;
                while Instant::now() < until && !stopped() {
                    inner.wait_tick(Duration::from_millis(100));
                }
                if stopped() {
                    break;
                }
                match connect_and_stream(SetUp::Reconnect, if attempt == 1 { &skip } else { &[] }) {
                    Ok(s) => {
                        restored = Some(s);
                        break;
                    }
                    Err(m) => tracing::warn!("LE Audio reconnect attempt {attempt} failed: {}", m.english()),
                }
            }
            // Earbuds gone again meanwhile (both in their case) are waited for
            // as before
            if restored.is_some()
                || !off
                || members.len() < 2
                || stopped()
                || host.le_find_any(&members, OFF_FIND).is_some()
                || stopped()
            {
                break;
            }
            tracing::info!("LE Audio: the earbuds are off again");
            inner.notify(StreamState::Idle, Msg::new("status.device_off"));
            swapped = false;
            first_wait = RECONNECT_WAIT;
            skip.clear();
        }
        let Some(s) = restored else {
            tracing::debug!("le stream: not restored (stop requested {})", stopped());
            if !stopped() {
                inner.notify(StreamState::Error, Msg::new("error.reconnect_failed"));
                lost_for_good = true;
            }
            break;
        };
        started = s;
        (shown_dropped, shown_stalled) = (0, 0);
        (lost, shown_lost, let_go) = (false, false, false);
        left.clear();
        stream_at = Instant::now();
        (rebuild, rebuilt) = (false, false);
        tracing::info!("LE Audio stream restored: {}", started.status.summary);
        mic_streams += 1;
        mic = open_mic(inner, &mut started, mic_target.as_ref(), mic_streams, &mut mic_note);
        audio = match LeAudioPath::start(&req.source, &started, host.stats().clone(), None) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::error!("LE Audio: audio capture: {e}");
                inner.notify(StreamState::Error, Msg::new("error.wasapi_init"));
                lost_for_good = true;
                break;
            }
        };
        if switch_default_again {
            default_guard = switch_default();
            switch_default_again = false;
        }
        report(&started, audio.as_ref(), mic_note.as_ref());
    }

    // Still looking for or connecting a member: the stop cancels that
    if let Some(j) = joining.take() {
        let _ = j.thread.join();
    }
    tracing::info!("LE Audio streaming stopped, cleaning up");
    tracing::debug!(
        "le stream: ended after {} s (stop requested {}, lost for good {lost_for_good}), {}",
        session_start.elapsed().as_secs(),
        stopped(),
        le_stats_line(&host.le_stream_stats())
    );
    if let Some(a) = audio.take() {
        a.stop();
    }
    host.le_stop_stream();
    // After the stream stopped: what arrived until then is decoded
    stop_mic(inner, &mut mic);
    host.le_disconnect_all();
    if let Some(mut v) = volume.take() {
        v.stop();
    }
    drop(output_mute);
    drop(default_guard);
    inner.set_le_stream(None);
    if !lost_for_good {
        ready();
    } else if !inner.auto_reconnect() {
        // As the A2DP stream: free for the phone the headphones may go to
        inner.shutdown_host();
    }
}

/// The microphone of a (re)started stream into `target` (None: not asked
/// for or refused), its status reported. `note` becomes the status message
/// when the output does not open (the stream plays without it), and is
/// cleared when it does.
fn open_mic(
    inner: &Inner,
    started: &mut LeStreamStarted,
    target: Option<&MicTarget>,
    stream: u32,
    note: &mut Option<Msg>,
) -> Option<LeMicPath> {
    let target = target?;
    match start_mic(started, target, stream, inner.mic_dsp.clone()) {
        Ok(Some(m)) => {
            inner.voice_status(Some(m.status()));
            *note = None;
            Some(m)
        }
        Ok(None) => {
            let why = &started.status.plan.source_note;
            tracing::warn!(
                "LE Audio: headset microphone asked for, none in the stream: {}",
                if why.is_empty() { "?" } else { why }
            );
            None
        }
        Err(_) => {
            *note = Some(Msg::new("status.headset_mic_output_failed"));
            None
        }
    }
}

/// Ends the microphone (what arrived is decoded first) and its status
fn stop_mic(inner: &Inner, mic: &mut Option<LeMicPath>) {
    if let Some(m) = mic.take() {
        m.stop();
        inner.voice_status(None);
    }
}

/// The broadcast configuration of a request: name and code checked, the
/// preset (`--le-preset`, else [`le_audio::broadcast::DEFAULT_PRESET`]), a
/// fresh random Broadcast_ID. Err: why, for the user.
pub fn broadcast_config(le: &LeRequest) -> Result<BroadcastConfig, String> {
    let b = le.broadcast.clone().unwrap_or_default();
    let name = broadcast::check_name(&b.name)?;
    let code = match b.code.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => Some(broadcast::broadcast_code(c)?),
        None => None,
    };
    let preset_name = le.preset.as_deref().unwrap_or(broadcast::DEFAULT_PRESET);
    let preset = broadcast::find_preset(preset_name).ok_or_else(|| {
        let names: Vec<String> = broadcast::presets().into_iter().map(|p| p.name).collect();
        format!("no broadcast configuration \"{preset_name}\" (one of {})", names.join(", "))
    })?;
    let rear_preset = match b.rear_preset.as_deref() {
        Some(n) => {
            let r = broadcast::find_preset(n).ok_or_else(|| format!("no broadcast configuration \"{n}\""))?;
            if r.frame_us != preset.frame_us {
                return Err(format!("the rear's {n} has another frame duration than {}", preset.name));
            }
            Some(r)
        }
        None => None,
    };
    let random = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id()).rotate_left(32);
    Ok(BroadcastConfig {
        name,
        code,
        preset,
        rear_preset,
        layout: if b.mono { broadcast::BroadcastLayout::Mono } else { b.layout },
        broadcast_id: broadcast::broadcast_id(random),
    })
}

/// Set-ups of a broadcast after its adapter stopped working: a reset or
/// replugged adapter (an nRF5340 DK) is back within a few seconds
const BROADCAST_ATTEMPTS: u32 = 5;
/// error.auracast's reason when the adapter went away under the broadcast
const ADAPTER_LOST: &str = "the adapter stopped working";
/// ... and did not come back for the attempts
const ADAPTER_NOT_BACK: &str = "the adapter stopped working and did not come back";
/// ... when the adapter chosen for Auracast is not there at the start
const AURACAST_ADAPTER_MISSING: &str = "the adapter chosen for Auracast is not connected";

/// How one broadcast on one stack ended
#[derive(Debug)]
enum BroadcastEnd {
    /// Stopped by a request
    Stopped,
    /// It did not start: no stack, no LE, the controller refused the BIGs
    NotStarted(Msg),
    /// It ended for good: the capture failed, or the controller ended it
    Failed(Msg),
    /// The adapter stopped working under it (reset, unplugged, sleep): the
    /// stack is dead, a new one can carry the broadcast again
    AdapterLost,
}

/// The session of a broadcast (Auracast source, developer mode): no device
/// to connect; BTstack up with LE, `le_start_broadcast`, capture into the
/// LE encode thread as for a unicast stream, until stopped. A broadcast the
/// controller ends is an error (error.auracast). One whose adapter stopped
/// working (reset, unplugged) starts again on a new stack, as a lost
/// unicast stream reconnects (when reconnecting is on).
fn run_broadcast(inner: &Arc<Inner>, req: StreamRequest) {
    let le = req.le.clone().unwrap_or_default();
    let stopped = || inner.stop_requested();
    let ready = || inner.notify(StreamState::Idle, Msg::new("status.ready"));
    tracing::info!("Auracast (developer mode): broadcast {}", crate::diag::quoted(&req.device_name));
    tracing::debug!("broadcast: request {:?}", req.redacted());

    if let Some(key) = refusal(inner.le_wanted(), inner.developer(), &req.source) {
        tracing::debug!("broadcast: refused ({key})");
        return inner.notify(StreamState::Error, Msg::new(key));
    }
    let config = match broadcast_config(&le) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Auracast: {e}");
            return inner.notify(StreamState::Error, Msg::with("error.auracast", &[&e]));
        }
    };

    inner.notify(StreamState::Connecting, Msg::new("status.initializing_btstack"));
    // The Auracast adapter, when one is set, for as long as this session
    // runs (Settings::auracast_adapter_id)
    let _auracast_adapter = inner.broadcast_wanted();
    // Not on the first other adapter found: the Auracast adapter was chosen
    // because that one may not broadcast at all
    if inner.auracast_adapter_missing() {
        tracing::warn!("Auracast: the Auracast adapter is not on WinUSB: the broadcast does not start");
        return inner.notify(StreamState::Error, Msg::with("error.auracast", &[&AURACAST_ADAPTER_MISSING]));
    }
    // A source switched in place (stream.start_broadcast's choice changed
    // while it runs) was asked for before this one started: not again
    inner.take_source_change();
    let mut source = req.source.clone();
    let mut end = broadcast_once(inner, &req, &config, &mut source);
    loop {
        match end {
            BroadcastEnd::Stopped => return ready(),
            BroadcastEnd::NotStarted(m) | BroadcastEnd::Failed(m) => {
                return if stopped() { ready() } else { inner.notify(StreamState::Error, m) };
            }
            BroadcastEnd::AdapterLost if stopped() => return ready(),
            BroadcastEnd::AdapterLost => {}
        }
        if !inner.auto_reconnect() {
            tracing::info!("Auracast: the adapter stopped working, reconnecting is off: the broadcast ends");
            return inner.notify(StreamState::Error, Msg::with("error.auracast", &[&ADAPTER_LOST]));
        }
        tracing::warn!("Auracast: the adapter stopped working, starting the broadcast again");
        inner.notify(StreamState::Reconnecting, Msg::new("status.connection_lost"));
        let mut attempt = 1;
        end = loop {
            inner.notify(
                StreamState::Reconnecting,
                Msg::with("status.reconnect_attempt", &[&attempt, &BROADCAST_ATTEMPTS]),
            );
            tracing::debug!("broadcast: attempt {attempt}/{BROADCAST_ATTEMPTS} in {} ms", RECONNECT_WAIT.as_millis());
            let until = Instant::now() + RECONNECT_WAIT;
            while Instant::now() < until && !stopped() {
                inner.wait_tick(Duration::from_millis(100));
            }
            if stopped() {
                break BroadcastEnd::Stopped;
            }
            if inner.power.suspending() {
                // The next round waits through the sleep
                tracing::debug!("broadcast: going to sleep, attempt {attempt} not counted");
                continue;
            }
            // A reset or replugged adapter takes a moment to come back: the
            // stack would open the first other one found meanwhile
            if !inner.wanted_adapter_present() {
                tracing::warn!("Auracast: attempt {attempt}/{BROADCAST_ATTEMPTS}: the adapter is not back yet");
                if attempt == BROADCAST_ATTEMPTS {
                    break BroadcastEnd::Failed(Msg::with("error.auracast", &[&ADAPTER_NOT_BACK]));
                }
                attempt += 1;
                continue;
            }
            // A source chosen meanwhile is the one to broadcast now
            if let Some(next) = inner.take_source_change() {
                source = next;
            }
            match broadcast_once(inner, &req, &config, &mut source) {
                BroadcastEnd::NotStarted(m) => {
                    tracing::warn!("Auracast: attempt {attempt}/{BROADCAST_ATTEMPTS} failed: {}", m.english());
                    if attempt == BROADCAST_ATTEMPTS {
                        break BroadcastEnd::Failed(Msg::with("error.auracast", &[&ADAPTER_NOT_BACK]));
                    }
                    attempt += 1;
                }
                other => break other,
            }
        };
    }
}

/// One broadcast on the running stack, or a new one when that stopped
/// working, until it ends
fn broadcast_once(
    inner: &Arc<Inner>,
    req: &StreamRequest,
    config: &BroadcastConfig,
    source: &mut SourceRequest,
) -> BroadcastEnd {
    let stopped = || inner.stop_requested();
    let host = match inner.ensure_host() {
        Ok(h) => h,
        Err(m) => return BroadcastEnd::NotStarted(m),
    };
    inner.note_broadcast_adapter();
    let until = Instant::now() + READY_WAIT;
    while !host.le_ready() && Instant::now() < until && !stopped() {
        inner.wait_tick(Duration::from_millis(100));
    }
    if stopped() {
        return BroadcastEnd::Stopped;
    }
    if !host.le_ready() {
        return BroadcastEnd::NotStarted(Msg::new("error.le_audio_unavailable"));
    }

    let started = match host.le_start_broadcast(config.clone(), &le_silence_set) {
        Ok(s) => s,
        Err(e) => {
            return if stopped() {
                BroadcastEnd::Stopped
            } else {
                BroadcastEnd::NotStarted(Msg::with("error.auracast", &[&e]))
            };
        }
    };
    // The matrix mixer feeds a matrix layout only; stereo, mono and split
    // keep the fixed downmix
    let mix = config.layout.is_matrix().then(crate::auracast_mix::handle);
    let mut audio = match LeAudioPath::start(source, &started, host.stats().clone(), mix.clone()) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Auracast: audio capture: {e}");
            host.le_stop_broadcast();
            return BroadcastEnd::Failed(Msg::new("error.wasapi_init"));
        }
    };

    inner.notify(StreamState::Streaming, Msg::new("status.broadcasting"));
    let plan = &started.status.plan;
    let channels = plan.channel_count() as u32;
    let rate = plan.top_sampling_hz();
    let rear = plan.cis.iter().flat_map(|c| &c.channels).any(|c| c.source.is_rear());
    inner.set_le_stream(Some(LeStreamDetails::of(&started)));
    // What the stream info and the mixer show of the capture
    let show = |audio: &LeAudioPath, source: &SourceRequest| {
        let format = audio.format();
        if config.layout.is_matrix() {
            crate::auracast_mix::set_live(Some(crate::auracast_mix::Live {
                channels: usize::from(format.channels),
                mask: audio.channel_mask(),
                rear,
            }));
        }
        inner.stream_info(StreamInfo {
            codec: CODEC_NAME.into(),
            bitrate_kbps: plan.sink_cis().map(|c| plan.preset_of(c).kbps_per_channel() * c.channels.len() as u32).sum(),
            sample_rate: rate,
            channels,
            bits: 0,
            source_sample_rate: format.sample_rate,
            source_channels: format.channels as u32,
            source_bits: (format.sample_format.bytes_per_sample() * 8) as u32,
            requested_codec: None,
            requested_in_use: false,
            abr: false,
            bitrate_capped: false,
            hi_res_set: false,
            low_latency: false,
            source: source.info(),
            details: None,
        });
    };
    show(&audio, source);

    let mut mutes_output = req.mutes_output(source);
    let mut output_mute = OutputMute::new();
    output_mute.tick(mutes_output);
    let mut discontinuities = DiscontinuityLog::default();
    // The transport lost the adapter (a reset or unplugged one), or the PC
    // slept: the stack reports no end of the broadcast, it just stops
    let adapter_gone = || host.stalled() || inner.power.stale();
    let mut lost = false;
    let mut adapter_lost = false;
    let mut capture_failed = false;
    while !stopped() {
        inner.wait_tick(Duration::from_millis(200));
        if let Some(next) = inner.take_source_change().filter(|n| n != source) {
            // The BIGs stay: the old capture stops, the new one encodes into
            // the same queue (silence goes out in between)
            tracing::info!("Auracast: source {:?} -> {:?}", source, next);
            audio.stop_now();
            match LeAudioPath::start(&next, &started, host.stats().clone(), mix.clone()) {
                Ok(a) => {
                    audio = a;
                    *source = next;
                    discontinuities = DiscontinuityLog::default();
                }
                Err(e) => {
                    tracing::error!("Auracast: audio capture of the new source: {e}");
                    match LeAudioPath::start(source, &started, host.stats().clone(), mix.clone()) {
                        Ok(a) => {
                            audio = a;
                            // The request says the new source: back to this one
                            inner.keep_source(source.clone());
                        }
                        Err(e) => {
                            tracing::error!("Auracast: audio capture of the old source again: {e}");
                            capture_failed = true;
                            break;
                        }
                    }
                }
            }
            mutes_output = req.mutes_output(source);
            show(&audio, source);
        }
        output_mute.tick(mutes_output);
        discontinuities.tick(audio.discontinuities());
        if adapter_gone() {
            adapter_lost = true;
            break;
        }
        if host.le_stream_lost() {
            lost = true;
            // A transport that failed may say so a moment after the BIG went
            let until = Instant::now() + Duration::from_secs(1);
            while !adapter_gone() && Instant::now() < until && !stopped() {
                inner.wait_tick(Duration::from_millis(100));
            }
            adapter_lost = adapter_gone();
            break;
        }
    }
    tracing::debug!(
        "broadcast: ended (stop requested {}, lost {lost}, adapter lost {adapter_lost}), {}",
        stopped(),
        le_stats_line(&host.le_stream_stats())
    );
    if !capture_failed {
        audio.stop();
    }
    crate::auracast_mix::set_live(None);
    // A stack without its adapter would only wait out the stop
    if !adapter_lost {
        host.le_stop_broadcast();
    }
    drop(output_mute);
    inner.set_le_stream(None);
    if capture_failed {
        BroadcastEnd::Failed(Msg::new("error.wasapi_init"))
    } else if stopped() {
        BroadcastEnd::Stopped
    } else if adapter_lost {
        BroadcastEnd::AdapterLost
    } else if lost {
        BroadcastEnd::Failed(Msg::with("error.auracast", &[&"the adapter ended the broadcast"]))
    } else {
        BroadcastEnd::Stopped
    }
}

/// A device of the stream ended the link itself (turned off, or its user
/// switched it away), as `stream::device_turned_off`; the disconnection may
/// come a moment after the stream broke
fn device_turned_off(inner: &Inner, host: &BtHost, devices: &[[u8; 6]]) -> bool {
    let until = Instant::now() + Duration::from_millis(1500);
    loop {
        // Off: one ended its link itself and none is connected any more (an
        // earbud in its case while the other is connected is not)
        let ours: Vec<_> = host.le_devices().into_iter().filter(|d| devices.contains(&d.addr.addr)).collect();
        let off = !ours.iter().any(|d| d.connected)
            && ours.iter().any(|d| !d.connected && matches!(d.last_error, 0x13 | 0x15));
        if off {
            return true;
        }
        if Instant::now() >= until || inner.stop_requested() {
            return false;
        }
        inner.wait_tick(Duration::from_millis(100));
    }
}

/// When the stream that plays on part of its set looks for the missing
/// members (a quiet scan, [`REJOIN_SCAN`] long): [`REJOIN_FIRST`] after
/// one went, then every [`REJOIN_EVERY`]; a member found that did not
/// make it into the stream doubles the wait, up to [`REJOIN_MAX`] (no gap
/// every few seconds for an earbud that advertises but does not stream),
/// and so does one that leaves again within [`REJOIN_SETTLE`] of joining
/// (in-ear detection that goes on and off)
struct Rejoin {
    next: Instant,
    wait: Duration,
    /// When the stream was last set up with every member
    restored_at: Option<Instant>,
}

impl Rejoin {
    fn new(now: Instant) -> Rejoin {
        Rejoin { next: now + REJOIN_FIRST, wait: REJOIN_EVERY, restored_at: None }
    }

    /// A member went
    fn look_soon(&mut self, now: Instant) {
        if self.restored_at.is_some_and(|t| now.saturating_duration_since(t) < REJOIN_SETTLE) {
            self.wait = (self.wait * 2).min(REJOIN_MAX);
            self.next = now + self.wait;
        } else {
            self.wait = REJOIN_EVERY;
            self.next = now + REJOIN_FIRST;
        }
    }

    fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// A look ended (found: the stream is set up again now)
    fn looked(&mut self, now: Instant, found: bool) {
        if !found {
            self.next = now + self.wait;
        }
    }

    /// The stream was set up with the member found: `complete`, every
    /// member in it
    fn restored(&mut self, now: Instant, complete: bool) {
        if complete {
            self.restored_at = Some(now);
        } else {
            self.wait = (self.wait * 2).min(REJOIN_MAX);
        }
        self.next = now + self.wait;
    }
}

/// What a stream is set up for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetUp {
    First,
    /// With a member back next to the one that played. Its stream just ran
    /// with the microphone, so a failure is no reason to try without it on a
    /// new link (that would drop the one that played too).
    Rejoin,
    /// After the stream was lost
    Reconnect,
}

/// The status of a stream that plays: which earbuds of a set do
/// ([`set_status_key`]), from the locations they report
fn set_status(host: &BtHost, started: &LeStreamStarted, dropped: &[LeDeviceId], stalled: u32) -> Msg {
    let known = host.le_devices();
    let playing: Vec<u32> = started
        .status
        .devices
        .iter()
        .filter(|id| !dropped.contains(id))
        .map(|id| known.iter().find(|d| d.id == *id).map_or(0, |d| d.sink_locations))
        .collect();
    let partial = host.le_set_members_missing() > 0 || !dropped.is_empty();
    Msg::new(set_status_key(&playing, partial, stalled))
}

/// `playing`: the sink locations of the devices that play; `partial`, others
/// of the set do not; `stalled` CIS get no audio through. One earbud alone
/// is no fault: the status only names it, where its location tells the side.
fn set_status_key(playing: &[u32], partial: bool, stalled: u32) -> &'static str {
    const LR: u32 = LOCATION_FRONT_LEFT | LOCATION_FRONT_RIGHT;
    match playing.iter().map(|l| l & LR).collect::<Vec<_>>()[..] {
        // Which one gets no audio is not known here
        _ if stalled > 0 => "status.le_no_audio",
        [LOCATION_FRONT_LEFT] if partial => "status.le_set_left",
        [LOCATION_FRONT_RIGHT] if partial => "status.le_set_right",
        _ if partial => "status.le_set_partial",
        [LOCATION_FRONT_LEFT, LOCATION_FRONT_RIGHT] | [LOCATION_FRONT_RIGHT, LOCATION_FRONT_LEFT] => {
            "status.le_set_both"
        }
        _ => "status.connected",
    }
}

/// The missing members of the set, looked for (a quiet scan,
/// [`REJOIN_SCAN`] long; another when the one seen gives no answer, up to
/// [`LINK_TRIES`]) and the one that advertises again (out of its case)
/// connected, on a thread of its own while the session goes on with the
/// others
struct Joining {
    missing: Vec<[u8; 6]>,
    thread: std::thread::JoinHandle<Joined>,
}

enum Joined {
    NotSeen,
    Connected,
    Failed,
}

impl Joining {
    fn start(inner: &Arc<Inner>, host: &Arc<BtHost>, missing: Vec<[u8; 6]>) -> Option<Joining> {
        if missing.is_empty() {
            return None;
        }
        let (inner, host, look_for) = (inner.clone(), host.clone(), missing.clone());
        std::thread::Builder::new()
            .name("le-join".into())
            .spawn(move || Joining::run(&inner, &host, &look_for))
            .inspect_err(|e| tracing::warn!("LE Audio: no thread to look for the missing member: {e}"))
            .ok()
            .map(|thread| Joining { missing, thread })
    }

    fn run(inner: &Inner, host: &BtHost, missing: &[[u8; 6]]) -> Joined {
        for _ in 0..LINK_TRIES {
            if let Some(j) = Joining::try_one(inner, host, missing) {
                return j;
            }
            if inner.stop_requested() {
                break;
            }
        }
        Joined::NotSeen
    }

    /// One scan and connection; None: seen, but not at that address any more
    fn try_one(inner: &Inner, host: &BtHost, missing: &[[u8; 6]]) -> Option<Joined> {
        let Some((i, seen)) = host.le_find_any_quietly(missing, REJOIN_SCAN) else { return Some(Joined::NotSeen) };
        // A stop the scan's start may have taken for an earlier one
        if inner.stop_requested() {
            return Some(Joined::NotSeen);
        }
        let member = bt_host::addr_str(&missing[i]);
        tracing::info!("LE Audio: {member} of the set is back (as {}), connecting it", seen.addr);
        let t = Instant::now();
        match host.le_connect_within(seen.addr, &seen.name, LINK_TIMEOUT, CONNECT_TIMEOUT) {
            Ok(_) => {
                tracing::info!(
                    "LE Audio: {member} connected in {} ms, setting the stream up with it",
                    t.elapsed().as_millis()
                );
                Some(Joined::Connected)
            }
            Err(e) => {
                tracing::warn!("LE Audio: connecting {member} failed ({e})");
                (e != bt_host::LE_NO_LINK).then_some(Joined::Failed)
            }
        }
    }
}

/// After the stream with a member back failed: the devices to set it up
/// with, those that `played` and are `linked` still, else the others linked
/// (the one that played went while the member came back)
fn fall_back_to(played: &[LeDeviceId], linked: &[LeDeviceId]) -> Vec<LeDeviceId> {
    let keep: Vec<LeDeviceId> = played.iter().copied().filter(|id| linked.contains(id)).collect();
    if keep.is_empty() { linked.to_vec() } else { keep }
}

/// How long a set-up looks for the members not connected, for
/// `BtHost::le_set_member_wait`: shortly where one that comes later joins
/// the playing stream (reconnecting is on and the members' addresses,
/// `known` of them, are known to look for); else CSIP's own time
fn member_wait(auto_reconnect: bool, known: usize, how: SetUp) -> Option<Duration> {
    let wait = if how == SetUp::Reconnect { MEMBER_WAIT_RECONNECT } else { MEMBER_WAIT };
    (auto_reconnect && known > 1).then_some(wait)
}

/// `a` is in `left` and went only now
fn just_left(left: &[([u8; 6], Instant)], a: &[u8; 6]) -> bool {
    left.iter().any(|(l, at)| l == a && at.elapsed() < JUST_LEFT)
}

/// The members of the set missing from the stream
fn missing_members(
    inner: &Inner,
    host: &BtHost,
    device: [u8; 6],
    devices: &[[u8; 6]],
    started: &LeStreamStarted,
    dropped: &[LeDeviceId],
) -> Vec<[u8; 6]> {
    let mut members = devices.to_vec();
    members.extend(other_set_members(inner, host, device, devices));
    let playing: Vec<[u8; 6]> = host
        .le_devices()
        .into_iter()
        .filter(|d| d.connected && started.status.devices.contains(&d.id) && !dropped.contains(&d.id))
        .map(|d| d.addr.addr)
        .collect();
    members.into_iter().filter(|a| !playing.contains(a)).collect()
}

/// The other members of `device`'s coordinated set (the other earbud), not
/// in `devices` already: either one may be the earbud out of its case
fn other_set_members(inner: &Inner, host: &BtHost, device: [u8; 6], devices: &[[u8; 6]]) -> Vec<[u8; 6]> {
    let sets = inner.known_le_sets(Some(host));
    let Some(group) = sets.group_of(&bt_host::addr_str(&device)) else { return Vec::new() };
    group.iter().filter_map(|a| bt_host::parse_addr(a)).filter(|a| !devices.contains(a)).collect()
}

/// The headphones were turned off: waits until one of `devices` (the
/// earbuds of a set) advertises again: its address. None when the session
/// ends instead (stopped, sleep, a stopped adapter).
fn wait_for_device(inner: &Inner, host: &BtHost, devices: &[[u8; 6]]) -> Option<[u8; 6]> {
    inner.waiting_for_device();
    let t = Instant::now();
    let back = loop {
        if inner.stop_requested()
            || inner.power.suspending()
            || inner.power.stale()
            || host.stalled()
            || !inner.auto_reconnect()
        {
            break None;
        }
        if let Some((i, _)) = host.le_find_any(devices, OFF_FIND) {
            break Some(devices[i]);
        }
        inner.wait_tick(Duration::from_millis(200));
    };
    tracing::debug!(
        "le device off: waited {} s, back {:?}",
        t.elapsed().as_secs(),
        back.map(|a| bt_host::addr_str(&a))
    );
    inner.done_waiting_for_device(back.is_some());
    back
}

/// Finds (address type, name, current RPA) and connects each device in
/// order; the first one's name is the profile's when it has one. In place
/// of the first device, one of `others` (the rest of its coordinated set)
/// that shows up first is connected: the earbud out of its case (none of
/// `skip`, on their way off). CSIP then connects the rest of the set.
fn connect_devices(
    host: &BtHost,
    devices: &[[u8; 6]],
    others: &[[u8; 6]],
    skip: &[[u8; 6]],
    first_name: &str,
    stopped: &dyn Fn() -> bool,
) -> Result<Vec<LeDeviceId>, Msg> {
    let mut ids = Vec::with_capacity(devices.len());
    for (i, &asked) in devices.iter().enumerate() {
        let mut addr = asked;
        let mut target = LeAddr { addr, addr_type: LeAddr::PUBLIC };
        let mut name = if i == 0 { first_name.to_string() } else { String::new() };
        let t = Instant::now();
        let mut look_for = vec![asked];
        if i == 0 {
            look_for.extend_from_slice(others);
            if look_for.iter().any(|a| !skip.contains(a)) {
                look_for.retain(|a| !skip.contains(a));
            }
        }
        for attempt in 1..=LINK_TRIES {
            let mut seen = false;
            // One connected already (the earbud that played on, one connected
            // while it did) advertises no more: no scan for it
            let linked = host.le_devices();
            let linked = linked
                .iter()
                .filter(|d| d.connected)
                .find(|d| d.addr.addr == asked && look_for.contains(&asked))
                .or_else(|| linked.iter().filter(|d| d.connected).find(|d| look_for.contains(&d.addr.addr)));
            if let Some(d) = linked {
                addr = d.addr.addr;
                target = d.addr;
                tracing::debug!("le connect: {} connected already as {:?}", bt_host::addr_str(&addr), d.id);
            } else if let Some((which, found)) = host.le_find_any(&look_for, FIND_TIMEOUT) {
                seen = true;
                addr = look_for[which];
                if addr != asked {
                    tracing::info!(
                        "LE Audio: {} is not around, connecting {} of its coordinated set",
                        bt_host::addr_str(&asked),
                        bt_host::addr_str(&addr)
                    );
                }
                tracing::debug!(
                    "le connect: {} found in {} ms as {} \"{}\" rssi {}",
                    bt_host::addr_str(&addr),
                    t.elapsed().as_millis(),
                    found.addr,
                    found.name,
                    found.rssi
                );
                // The address it advertises now (an RPA of a bonded device)
                target = found.addr;
                if name.is_empty() {
                    name = found.name;
                }
            } else if attempt > 1 {
                return Err(Msg::with("error.le_audio_connect", &[&bt_host::LE_NO_LINK]));
            } else {
                tracing::debug!(
                    "le connect: {} not found in {} ms, trying its public address",
                    bt_host::addr_str(&addr),
                    t.elapsed().as_millis()
                );
            }
            if stopped() {
                return Err(Msg::new("status.ready"));
            }
            let t = Instant::now();
            // Seen just now, or of a set (the one asked for may be off): soon
            let link = if seen || (i == 0 && !others.is_empty()) { LINK_TIMEOUT } else { CONNECT_TIMEOUT };
            match host.le_connect_within(target, &name, link, CONNECT_TIMEOUT) {
                Ok(id) => {
                    tracing::debug!("le connect: {target} connected as {id:?} in {} ms", t.elapsed().as_millis());
                    ids.push(id);
                    break;
                }
                Err(e) => {
                    tracing::debug!("le connect: {target} failed after {} ms: {e}", t.elapsed().as_millis());
                    // At another address by now: looked for again
                    if e == bt_host::LE_NO_LINK && seen && attempt < LINK_TRIES && !stopped() {
                        continue;
                    }
                    return Err(Msg::with("error.le_audio_connect", &[&e]));
                }
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_that_leaves_again_soon_is_waited_for_longer() {
        let t = Instant::now();
        let mut r = Rejoin::new(t);
        assert!(!r.due(t) && r.due(t + REJOIN_FIRST));
        // Gone, back with every member
        r.look_soon(t);
        r.restored(t + Duration::from_secs(4), true);
        // Gone again within REJOIN_SETTLE: twice the wait each time
        let mut at = t + Duration::from_secs(10);
        for wait in [8, 16, 32, 60, 60] {
            r.look_soon(at);
            assert!(!r.due(at + Duration::from_secs(wait) - Duration::from_millis(1)), "{wait} s");
            assert!(r.due(at + Duration::from_secs(wait)));
            at += Duration::from_secs(wait);
            r.restored(at, true);
            at += Duration::from_secs(5);
        }
        // Gone after it stayed longer: soon again
        at += REJOIN_SETTLE;
        r.look_soon(at);
        assert!(r.due(at + REJOIN_FIRST));
        r.looked(at + REJOIN_FIRST, false);
        assert!(r.due(at + REJOIN_FIRST + REJOIN_EVERY));
    }

    #[test]
    fn a_member_found_that_does_not_stream_doubles_the_wait() {
        let t = Instant::now();
        let mut r = Rejoin::new(t);
        r.restored(t, false);
        assert!(!r.due(t + REJOIN_EVERY) && r.due(t + 2 * REJOIN_EVERY));
        r.restored(t, false);
        assert!(r.due(t + 4 * REJOIN_EVERY));
    }

    #[test]
    fn a_developer_microphone_preset_is_taken_as_it_is() {
        let le = LeRequest { headset_mic: bt_host::HeadsetMic::On, ..LeRequest::default() };
        let (mut r, _) = stream_request(&le);
        apply_debug(&mut r, &LeDebug::default());
        assert!(!r.source_preset_exact);
        apply_debug(&mut r, &LeDebug { mic_preset: "32_2_1".into(), retry_same_link: false });
        assert_eq!((r.source_preset_order.as_slice(), r.source_preset_exact), (&["32_2_1".to_string()][..], true));
    }

    #[test]
    fn refused_while_off_or_for_app_capture() {
        let app = SourceRequest::Application { exe: "x.exe".into() };
        assert_eq!(refusal(false, true, &SourceRequest::SystemLoopback), Some("error.le_audio_disabled"));
        assert_eq!(refusal(false, true, &app), Some("error.le_audio_disabled"));
        // Outside developer mode there is no switch to point to
        assert_eq!(refusal(false, false, &SourceRequest::SystemLoopback), Some("error.le_audio_developer"));
        assert_eq!(refusal(true, true, &app), Some("error.le_audio_capture"));
        assert_eq!(refusal(true, true, &SourceRequest::SystemLoopback), None);
        assert_eq!(refusal(true, true, &SourceRequest::TestTone), None);
        let virt = SourceRequest::VirtualDevice { id: "{x}".into(), name: "CABLE".into(), auto_switch: true };
        assert_eq!(refusal(true, true, &virt), None);
        // The messages exist in English
        for key in [
            "error.le_audio_disabled",
            "error.le_audio_developer",
            "error.le_audio_capture",
            "error.le_audio_unavailable",
        ] {
            assert_ne!(Msg::new(key).english(), key);
        }
        assert!(Msg::with("error.le_audio_connect", &[&"timeout"]).english().contains("(timeout)"));
        assert!(Msg::with("error.le_audio_stream", &[&"no ISO buffers"]).english().contains("(no ISO buffers)"));
    }

    #[test]
    fn the_status_names_the_earbuds_that_play() {
        let (l, r) = (LOCATION_FRONT_LEFT, LOCATION_FRONT_RIGHT);
        assert_eq!(set_status_key(&[l, r], false, 0), "status.le_set_both");
        assert_eq!(set_status_key(&[r, l], false, 0), "status.le_set_both");
        assert_eq!(set_status_key(&[l], true, 0), "status.le_set_left");
        assert_eq!(set_status_key(&[r], true, 0), "status.le_set_right");
        // No side reported, or both on one device
        assert_eq!(set_status_key(&[0], true, 0), "status.le_set_partial");
        assert_eq!(set_status_key(&[l | r], true, 0), "status.le_set_partial");
        // A CIS without audio: not known whose
        assert_eq!(set_status_key(&[l, r], false, 1), "status.le_no_audio");
        // No set: headphones, two of them, one earbud without a set
        assert_eq!(set_status_key(&[l | r], false, 0), "status.connected");
        assert_eq!(set_status_key(&[l | r, l | r], false, 0), "status.connected");
        assert_eq!(set_status_key(&[l], false, 0), "status.connected");
    }

    #[test]
    fn a_failed_rejoin_falls_back_to_whichever_earbud_is_there() {
        let (l, r) = (LeDeviceId(0), LeDeviceId(1));
        // The one that played first, without the member that failed
        assert_eq!(fall_back_to(&[l], &[l, r]), [l]);
        assert_eq!(fall_back_to(&[l], &[l]), [l]);
        // Swapped: the one that played went into its case meanwhile
        assert_eq!(fall_back_to(&[l], &[r]), [r]);
        assert_eq!(fall_back_to(&[l], &[]), []);
    }

    #[test]
    fn the_wait_for_members_is_short_only_when_a_late_one_can_join() {
        assert_eq!(member_wait(true, 2, SetUp::First), Some(MEMBER_WAIT));
        assert_eq!(member_wait(true, 2, SetUp::Rejoin), Some(MEMBER_WAIT));
        // The sound is back first, the other one joins
        assert_eq!(member_wait(true, 2, SetUp::Reconnect), Some(MEMBER_WAIT_RECONNECT));
        // Reconnecting off: nothing looks for it later
        assert_eq!(member_wait(false, 2, SetUp::First), None);
        assert_eq!(member_wait(false, 2, SetUp::Reconnect), None);
        // The first stream to a set: the other address is not known yet
        assert_eq!(member_wait(true, 1, SetUp::First), None);
    }

    #[test]
    fn an_earbud_that_left_a_while_ago_is_not_on_its_way_off() {
        let now = Instant::now();
        let Some(before) = now.checked_sub(JUST_LEFT) else { return };
        let left = [([1; 6], now), ([2; 6], before)];
        assert!(just_left(&left, &[1; 6]));
        assert!(!just_left(&left, &[2; 6]));
        assert!(!just_left(&left, &[3; 6]));
    }

    #[test]
    fn stats_line_as_v1() {
        let s = bt_host::LeStreamStats {
            sets_encoded: 490,
            sets_sent: 500,
            sdus_sent: 1000,
            bytes_sent: 155000,
            silence_sets: 10,
            underruns: 1,
            overflows: 2,
            send_errors: 0,
            prefill: 5,
            ..Default::default()
        };
        assert_eq!(
            le_stats_line(&s),
            "LE Audio: 500 SDU sets sent (1000 SDUs), 490 encoded, 10 silence, 1 underruns, 2 dropped, 0 send errors"
        );
    }

    #[test]
    fn preset_and_low_latency() {
        let (r, ll) = stream_request(&LeRequest::default());
        assert_eq!((r, ll), (LeStreamRequest::default(), false));
        // The profile's choice or the flag
        assert!(stream_request(&LeRequest { low_latency: true, ..LeRequest::default() }).1);
        // An explicit preset wins
        let le = LeRequest { preset: Some("48_2_2".into()), low_latency: true, ..LeRequest::default() };
        let (r, ll) = stream_request(&le);
        assert_eq!(r.preset_order, ["48_2_2"]);
        assert!(!ll);
        assert!(!r.conversational);
        // The headset microphone: the conversational configuration
        let (r, _) = stream_request(&LeRequest { headset_mic: bt_host::HeadsetMic::On, ..LeRequest::default() });
        assert!(r.conversational);
        // Auto: the same (the conversational stream keeps the media preset)
        let (r, _) = stream_request(&LeRequest { headset_mic: bt_host::HeadsetMic::Auto, ..LeRequest::default() });
        assert!(r.conversational);
        assert_eq!(r.source_rate, None);
        // The microphone's rate chosen
        let le = LeRequest { headset_mic: bt_host::HeadsetMic::On, mic_rate: Some(16000), ..LeRequest::default() };
        assert_eq!(stream_request(&le).0.source_rate, Some(16000));
    }

    #[test]
    fn the_microphone_target_is_checked_before_the_stream() {
        assert_eq!(le_mic_target(false, Some("CABLE"), &SourceRequest::TestTone), Ok(None));
        assert_eq!(
            le_mic_target(true, Some("out\\mic.wav"), &SourceRequest::TestTone),
            Ok(Some(MicTarget::Wav("out\\mic.wav".into())))
        );
        // The captured device: refused, as for a classic device
        let id = "{0.0.0.00000000}.{not-a-device}";
        let virt = SourceRequest::VirtualDevice { id: id.into(), name: "CABLE".into(), auto_switch: true };
        assert_eq!(le_mic_target(true, Some(id), &virt), Err(Msg::new("error.headset_mic_loop")));
    }

    #[test]
    fn the_decode_thread_pushes_into_the_voice_link_queue() {
        let (producer, mut consumer) = voice_link::pcm_channel(4);
        let mut out = ProducerOut(producer);
        out.push(&[1, 2, 3]);
        out.push(&[4, 5]);
        let mut got = [0i16; 4];
        assert_eq!(consumer.pop(&mut got), 4);
        assert_eq!(got, [1, 2, 3, 4]);
        // The one that did not fit is the queue's overflow
        assert_eq!(consumer.stats().overflow, 1);
    }

    #[test]
    fn a_wav_microphone_is_written_through_the_voice_link() {
        let path = std::env::temp_dir().join(format!("bluecaster-le-mic-{}.wav", std::process::id()));
        let (_producer, consumer) = rtrb::RingBuffer::new(8);
        let rx = LeRxHandle {
            consumer,
            streams: vec![bt_host::LeRxStream { device_id: 0, cis_id: 0, ase_id: 3, channels: 1, octets: 40 }],
            sample_rate: 16000,
            frame_us: 10000,
            preset: "16_2_1".into(),
            stats: Arc::default(),
        };
        let mic = LeMicPath::start(rx, &MicTarget::Wav(path.clone()), voice_link::MicDsp::new()).unwrap();
        assert_eq!(mic.rate(), 16000);
        let status = mic.status();
        assert_eq!((status.codec.as_str(), status.link.as_str(), status.rate), ("lc3", "le", 16000));
        assert!(status.mic_device_id.is_empty());
        assert_eq!(mic.stop().frames_out, 0);
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((&bytes[0..4], bytes.len()), (&b"RIFF"[..], 44));
    }

    #[test]
    fn app_capture_has_no_le_source() {
        assert!(LeAudioPath::capture_source(&SourceRequest::Application { exe: "x.exe".into() }).is_err());
        assert!(
            LeAudioPath::capture_source(&SourceRequest::VirtualDevice {
                id: String::new(),
                name: String::new(),
                auto_switch: false
            })
            .is_err()
        );
        assert!(matches!(LeAudioPath::capture_source(&SourceRequest::TestTone), Ok(Source::TestTone)));
        assert_eq!(
            LeAudioPath::capture_source(&SourceRequest::OutputDevice {
                id: "{speakers}".into(),
                name: String::new(),
                auto_switch: true,
            })
            .unwrap(),
            Source::RenderDevice { id: "{speakers}".into() }
        );
    }
}
