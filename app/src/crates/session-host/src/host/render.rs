//! Group `render`: PCM from the core to a renderer (core to agent), e.g.
//! the headset microphone's voice played to a Windows output. The core
//! creates each channel's ring and writes into it ([`RenderChannel::push`]
//! never blocks: an audio thread may call it); the renderer reads it
//! ([`crate::handlers::render::Renderer`]), in the agent or, with the
//! local host, on a thread of this process.
//!
//! Targets: [`RenderTarget::Discard`] (reads and counts: the transport's
//! tests), [`RenderTarget::Endpoint`] (a Windows render endpoint through
//! `voice_link`'s renderer: WASAPI, prefill, drift control, all beside the
//! device) and [`RenderTarget::Wav`] (a WAV file: the developer CLI and the
//! emulator tests).
//!
//! The headset microphone opens its output with [`open_voice`]
//! ([`VoiceOutput`]). With the local host that is `voice_link`'s renderer or
//! WAV writer in this process, as before the agent existed. With the agent
//! the decoded PCM still goes into a `voice_link` queue (the BTstack or
//! decode thread pushes without waiting), and a thread of the core
//! ("voice-agent-render") reads it, runs the microphone's processing and
//! tap there, and writes it into a render channel; when the agent is lost
//! that thread opens the channel again through the next agent.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio_capture::{Format, Link, SampleFormat};
use pcm_link::{Kind, LOST_UNKNOWN, Stat, Writer};
use serde::{Deserialize, Serialize};
use voice_link::{PcmConsumer, PcmProducer};

use crate::handlers::render::Renderer;
use crate::hub::{ChannelState, Hub, new_channel};
use crate::wire::{decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "render";

/// A render channel's ring: 1.3 s of 48 kHz float stereo
pub const RENDER_RING_BYTES: usize = 256 << 10;

/// Where a render channel's PCM goes
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderTarget {
    /// Read and counted, played nowhere
    Discard,
    /// A Windows render endpoint (`device_id` empty: the default output),
    /// mono 16-bit PCM: silence until `prefill_ms` is queued and on an
    /// underrun, a drift control against the device's clock
    Endpoint { device_id: String, prefill_ms: u32 },
    /// A mono 16-bit WAV file, created (truncated) at open
    Wav { path: PathBuf },
}

/// The counters of an [`RenderTarget::Endpoint`]'s renderer
/// (`voice_link::RenderStats`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputStats {
    pub frames_written: u64,
    pub silence_frames: u64,
    pub underruns: u64,
    pub overflow: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub queued: u64,
}

impl From<voice_link::RenderStats> for OutputStats {
    fn from(s: voice_link::RenderStats) -> OutputStats {
        OutputStats {
            frames_written: s.frames_written,
            silence_frames: s.silence_frames,
            underruns: s.underruns,
            overflow: s.overflow,
            dropped: s.dropped,
            duplicated: s.duplicated,
            queued: s.queued,
        }
    }
}

impl From<OutputStats> for voice_link::RenderStats {
    fn from(s: OutputStats) -> voice_link::RenderStats {
        voice_link::RenderStats {
            frames_written: s.frames_written,
            silence_frames: s.silence_frames,
            underruns: s.underruns,
            overflow: s.overflow,
            dropped: s.dropped,
            duplicated: s.duplicated,
            queued: s.queued,
        }
    }
}

/// What a renderer read so far
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderStats {
    pub records: u64,
    pub frames: u64,
    pub silence_frames: u64,
    pub discontinuities: u64,
    /// Frames lost in those discontinuities, where known
    pub lost_frames: u64,
    /// FNV-1a over the audio bytes in order
    pub checksum: u64,
    /// The output's own counters, when it has them (an endpoint)
    #[serde(default)]
    pub output: Option<OutputStats>,
    /// Why the output ended, once it did ([`RenderChannel::ended`])
    #[serde(default)]
    pub end_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Starts rendering the channel `name` (created by the core). Reply:
    /// null, or a failure with the output's full error text as its detail
    Open { channel: u64, name: String, format: Format, target: RenderTarget },
    /// Reply: [`RenderStats`]
    Stats { channel: u64 },
    /// Stops and closes a channel (sent without waiting, or waited for to
    /// know a file is finished). Reply: null
    Close { channel: u64 },
}

pub trait RenderHost {
    /// Opens a render channel of `format` to `target`. Blocks until the
    /// renderer runs: not for an audio thread (pushing is).
    fn open_render(&self, format: Format, target: RenderTarget) -> Result<RenderChannel, HostError>;

    /// Opens the headset microphone's output `target` (an endpoint or a
    /// WAV file) for mono 16-bit PCM at `rate` Hz. Err: the error text for
    /// the log (the output's own, as `voice_link` words it). Blocks until
    /// the output runs: not for the BTstack or an audio thread.
    fn open_voice(&self, target: &RenderTarget, rate: u32) -> Result<VoiceOutput, String>;
}

/// Opens a render channel with this process's host
pub fn open_render(format: Format, target: RenderTarget) -> Result<RenderChannel, HostError> {
    crate::host().open_render(format, target)
}

/// Opens the headset microphone's output with this process's host
pub fn open_voice(target: &RenderTarget, rate: u32) -> Result<VoiceOutput, String> {
    crate::host().open_voice(target, rate)
}

impl RenderHost for LocalHost {
    fn open_render(&self, format: Format, target: RenderTarget) -> Result<RenderChannel, HostError> {
        let (_, name) = new_channel();
        let link = Link::create(&name, RENDER_RING_BYTES).map_err(|e| HostError::Protocol(format!("ring: {e}")))?;
        let renderer = Renderer::start(link.clone(), format, &target).map_err(|e| HostError::Protocol(e.full))?;
        Ok(RenderChannel {
            writer: Writer::new(link),
            format,
            lost: Arc::new(AtomicBool::new(false)),
            inner: Inner::Local(renderer),
            closed: false,
        })
    }

    fn open_voice(&self, target: &RenderTarget, rate: u32) -> Result<VoiceOutput, String> {
        match target {
            RenderTarget::Endpoint { device_id, prefill_ms } => {
                voice_link::Renderer::open(device_id, rate, *prefill_ms)
                    .map(|r| VoiceOutput(Voice::Device(r)))
                    .map_err(|e| e.to_string())
            }
            RenderTarget::Wav { path } => voice_link::WavSink::create(path, rate)
                .map(|w| VoiceOutput(Voice::Wav(w)))
                .map_err(|e| format!("{}: {e}", path.display())),
            RenderTarget::Discard => Err("not a microphone output".into()),
        }
    }
}

impl RenderHost for RemoteHost {
    fn open_render(&self, format: Format, target: RenderTarget) -> Result<RenderChannel, HostError> {
        open_remote(self.hub(), format, target)
    }

    fn open_voice(&self, target: &RenderTarget, rate: u32) -> Result<VoiceOutput, String> {
        AgentVoice::open(self.hub(), target, rate).map(|a| VoiceOutput(Voice::Agent(a)))
    }
}

fn open_remote(hub: &Arc<Hub>, format: Format, target: RenderTarget) -> Result<RenderChannel, HostError> {
    // The channel is made for the agent the request goes to (in service
    // mode: handles duplicated into that agent)
    let (_, agent, id, link) = hub.request_with_link(GROUP, RENDER_RING_BYTES, |channel, name| {
        to_body(&Request::Open { channel, name, format, target })
    })?;
    let lost = Arc::new(AtomicBool::new(false));
    let state = Arc::new(ChannelState { generation: agent.generation, lost: lost.clone(), link: link.clone() });
    hub.register_channel(&state);
    tracing::debug!("render: channel {id} opened");
    Ok(RenderChannel {
        writer: Writer::new(link),
        format,
        lost,
        inner: Inner::Remote { hub: hub.clone(), id, state },
        closed: false,
    })
}

enum Inner {
    Local(Renderer),
    Remote { hub: Arc<Hub>, id: u64, state: Arc<ChannelState> },
}

/// The core's end of a render channel. Dropping it closes the channel.
pub struct RenderChannel {
    writer: Writer,
    format: Format,
    lost: Arc<AtomicBool>,
    inner: Inner,
    /// [`RenderChannel::close`] sent the close already
    closed: bool,
}

impl std::fmt::Debug for RenderChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderChannel").field("format", &self.format).field("lost", &self.lost()).finish()
    }
}

impl RenderChannel {
    pub fn format(&self) -> Format {
        self.format
    }

    /// Queues interleaved frames of [`RenderChannel::format`]. Never blocks
    /// or allocates; false (the frames dropped and counted) when the ring
    /// is full or the renderer is gone.
    pub fn push(&mut self, data: &[u8]) -> bool {
        let frames = data.len() / self.format.bytes_per_frame().max(1);
        !self.lost() && self.writer.push(Kind::Audio, frames as u32, 0, data)
    }

    /// Queues `frames` frames of silence
    pub fn push_silence(&mut self, frames: u32) -> bool {
        !self.lost() && self.writer.push(Kind::Silence, frames, 0, &[])
    }

    /// Marks a gap in what was pushed (`lost`: its frames, when known)
    pub fn push_discontinuity(&mut self, lost: Option<u64>) -> bool {
        !self.lost() && self.writer.push(Kind::Discontinuity, 0, lost.unwrap_or(LOST_UNKNOWN), &[])
    }

    /// The agent that rendered it went away: nothing pushed plays any more.
    /// Open a new channel (it goes to the next agent).
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// The renderer stopped by itself (its output went); why:
    /// [`RenderStats::end_error`]
    pub fn ended(&self) -> bool {
        self.writer.link().stat(Stat::EndCode).load(Ordering::Acquire) != 0
    }

    /// Records the ring had no room for
    pub fn dropped(&self) -> u64 {
        self.writer.link().stat(Stat::DroppedRecords).load(Ordering::Relaxed)
    }

    /// What the renderer read so far (a request to the agent: not for an
    /// audio thread)
    pub fn stats(&self) -> Result<RenderStats, HostError> {
        match &self.inner {
            Inner::Remote { hub, .. } => self.stats_within(hub.request_timeout()),
            Inner::Local(_) => self.stats_within(Duration::ZERO),
        }
    }

    /// [`RenderChannel::stats`], waiting at most `timeout` for the agent
    pub fn stats_within(&self, timeout: Duration) -> Result<RenderStats, HostError> {
        match &self.inner {
            Inner::Local(r) => Ok(r.stats()),
            Inner::Remote { hub, id, state } => {
                if self.lost() {
                    return Err(HostError::AgentLost);
                }
                let request = Request::Stats { channel: *id };
                let (reply, agent) = hub.request_on(GROUP, to_body(&request), timeout)?;
                if agent.generation != state.generation {
                    return Err(HostError::AgentLost);
                }
                decode_reply(reply)
            }
        }
    }

    /// Closes the channel and waits up to `timeout` until the renderer has
    /// read everything pushed and stopped (a WAV file is then complete).
    /// Dropping it closes without waiting.
    pub fn close(mut self, timeout: Duration) {
        if let Inner::Remote { hub, id, state } = &self.inner
            && !self.lost()
        {
            self.closed = true;
            let request = to_body(&Request::Close { channel: *id });
            if hub.agent().is_some_and(|a| a.generation == state.generation)
                && let Err(e) = hub.request_on(GROUP, request, timeout)
            {
                tracing::debug!("render: channel {id} not closed in time ({e})");
            }
        }
    }
}

impl Drop for RenderChannel {
    fn drop(&mut self) {
        if let Inner::Remote { hub, id, state } = &self.inner
            && !self.closed
        {
            hub.notify(state.generation, GROUP, to_body(&Request::Close { channel: *id }));
        }
    }
}

/// The headset microphone's output ([`open_voice`]): mono 16-bit PCM at
/// the voice rate pushed through its [`PcmProducer`]
pub struct VoiceOutput(Voice);

enum Voice {
    /// In this process: the renderer (prefill, drift control)
    Device(voice_link::Renderer),
    /// In this process: the WAV writer
    Wav(voice_link::WavSink),
    /// In the session agent, fed from here
    Agent(AgentVoice),
}

impl std::fmt::Debug for VoiceOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.0 {
            Voice::Device(_) => "device",
            Voice::Wav(_) => "wav",
            Voice::Agent(_) => "agent",
        };
        f.debug_struct("VoiceOutput").field("kind", &kind).finish()
    }
}

impl VoiceOutput {
    /// Its producer: lock-free, for the BTstack or a decode thread (once;
    /// later calls get one that only counts or None, as the output's own)
    pub fn producer(&mut self) -> Option<PcmProducer> {
        match &mut self.0 {
            Voice::Device(r) => Some(r.producer()),
            Voice::Wav(w) => w.producer(),
            Voice::Agent(a) => a.producer.take(),
        }
    }

    /// The output went away or failed (the device removed, a session agent
    /// that cannot open it again): why. Does not wait.
    pub fn ended(&self) -> Option<String> {
        match &self.0 {
            Voice::Device(r) if r.ended() => Some(r.end_error().unwrap_or("ended").to_string()),
            Voice::Agent(a) => a.shared.ended.get().cloned(),
            _ => None,
        }
    }

    /// The renderer's counters (None: a file). In the agent: as last read
    /// (about once a second), over every agent that rendered it, with
    /// what could not be handed to an agent counted as overflow. Does not
    /// wait.
    pub fn stats(&self) -> Option<voice_link::RenderStats> {
        match &self.0 {
            Voice::Device(r) => Some(r.stats()),
            Voice::Wav(_) => None,
            Voice::Agent(a) => a.endpoint.then(|| *lock(&a.shared.stats)),
        }
    }

    /// Rendered in the session agent
    pub fn in_agent(&self) -> bool {
        matches!(self.0, Voice::Agent(_))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The core's queue in front of the agent: room for the time an agent
/// takes to answer and for the thread's rounds
const VOICE_QUEUE_MS: usize = 200;
/// How often the thread moves the queue into the channel
const PUMP_EVERY: Duration = Duration::from_millis(5);
/// How often it reads the renderer's counters, and how long it waits
const STATS_EVERY: Duration = Duration::from_secs(1);
const STATS_TIMEOUT: Duration = Duration::from_millis(250);
/// After a lost agent: between tries to open the output again, and how
/// long each try waits for an agent (so a stop is seen soon)
const REOPEN_EVERY: Duration = Duration::from_secs(1);
const REOPEN_WAIT: Duration = Duration::from_millis(500);
/// How long the end waits for the agent to finish the output
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct VoiceShared {
    stop: AtomicBool,
    ended: OnceLock<String>,
    stats: Mutex<voice_link::RenderStats>,
    reopened: AtomicU64,
}

/// The microphone output in the session agent
struct AgentVoice {
    shared: Arc<VoiceShared>,
    producer: Option<PcmProducer>,
    /// An endpoint (with counters), not a file
    endpoint: bool,
    thread: Option<JoinHandle<()>>,
}

/// Errors worth another try with the next agent
fn retryable(e: &HostError) -> bool {
    matches!(e, HostError::AgentLost | HostError::NoAgent | HostError::Timeout)
}

/// The text for the log of a failed open: the output's own error (the
/// failure's detail), as the in-process output words it
fn open_error(e: &HostError) -> String {
    match e {
        HostError::Remote(w) => w.detail_as::<String>().unwrap_or_else(|| w.message.clone()),
        other => other.to_string(),
    }
}

impl AgentVoice {
    fn open(hub: &Arc<Hub>, target: &RenderTarget, rate: u32) -> Result<AgentVoice, String> {
        if rate == 0 || matches!(target, RenderTarget::Discard) {
            return Err("not a microphone output".into());
        }
        let target = match target {
            // The agent's working directory may be another
            RenderTarget::Wav { path } => {
                RenderTarget::Wav { path: std::path::absolute(path).unwrap_or_else(|_| path.clone()) }
            }
            other => other.clone(),
        };
        let format = Format { sample_rate: rate, channels: 1, sample_format: SampleFormat::I16 };
        let channel = open_remote(hub, format, target.clone()).map_err(|e| open_error(&e))?;
        let (producer, queue) = voice_link::pcm_channel((rate as usize * VOICE_QUEUE_MS / 1000).max(1));
        let shared = Arc::new(VoiceShared::default());
        let endpoint = matches!(target, RenderTarget::Endpoint { .. });
        let now = Instant::now();
        let pump = Pump {
            hub: hub.clone(),
            target,
            format,
            queue,
            channel: Some(channel),
            shared: shared.clone(),
            base: voice_link::RenderStats::default(),
            current: voice_link::RenderStats::default(),
            refused: 0,
            discarded: 0,
            next_stats: now + STATS_EVERY,
            next_open: now,
            buf: vec![0; (rate as usize / 10).max(1)],
            bytes: Vec::new(),
        };
        let thread = std::thread::Builder::new()
            .name("voice-agent-render".into())
            .spawn(move || pump.run())
            .map_err(|e| format!("cannot start the render thread: {e}"))?;
        tracing::info!("voice render: the microphone output runs in the session agent ({rate} Hz)");
        Ok(AgentVoice { shared, producer: Some(producer), endpoint, thread: Some(thread) })
    }
}

impl Drop for AgentVoice {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The thread between the core's queue and the agent's channel
struct Pump {
    hub: Arc<Hub>,
    target: RenderTarget,
    format: Format,
    /// Read here: the microphone's processing and tap run on what is read
    queue: PcmConsumer,
    channel: Option<RenderChannel>,
    shared: Arc<VoiceShared>,
    /// The counters of the channels before this one, and of this one
    base: voice_link::RenderStats,
    current: voice_link::RenderStats,
    /// Frames the ring had no room for, and frames read while no agent
    /// rendered them
    refused: u64,
    discarded: u64,
    next_stats: Instant,
    next_open: Instant,
    buf: Vec<i16>,
    bytes: Vec<u8>,
}

fn add(a: voice_link::RenderStats, b: voice_link::RenderStats) -> voice_link::RenderStats {
    voice_link::RenderStats {
        frames_written: a.frames_written + b.frames_written,
        silence_frames: a.silence_frames + b.silence_frames,
        underruns: a.underruns + b.underruns,
        overflow: a.overflow + b.overflow,
        dropped: a.dropped + b.dropped,
        duplicated: a.duplicated + b.duplicated,
        queued: b.queued,
    }
}

impl Pump {
    fn run(mut self) {
        loop {
            let stopping = self.shared.stop.load(Ordering::Acquire);
            self.forward();
            if stopping {
                break;
            }
            self.check();
            std::thread::sleep(PUMP_EVERY);
        }
        if let Some(ch) = self.channel.take() {
            if !ch.lost() {
                self.current_stats(&ch);
            }
            ch.close(CLOSE_TIMEOUT);
        }
        self.publish();
        let s = *lock(&self.shared.stats);
        tracing::debug!(
            "voice render: in the session agent: {} frames, {} underruns, {} overflow, {} reopen(s)",
            s.frames_written,
            s.underruns,
            s.overflow,
            self.shared.reopened.load(Ordering::Relaxed)
        );
    }

    /// Everything queued into the channel (or counted, with none)
    fn forward(&mut self) {
        loop {
            let n = self.queue.pop(&mut self.buf);
            if n == 0 {
                return;
            }
            match &mut self.channel {
                Some(ch) if !ch.lost() => {
                    self.bytes.clear();
                    self.bytes.extend(self.buf[..n].iter().flat_map(|s| s.to_le_bytes()));
                    if !ch.push(&self.bytes) {
                        self.refused += n as u64;
                    }
                }
                _ => self.discarded += n as u64,
            }
            if n < self.buf.len() {
                return;
            }
        }
    }

    /// The renderer's counters into `current`; false if it did not answer
    fn current_stats(&mut self, ch: &RenderChannel) -> bool {
        match ch.stats_within(STATS_TIMEOUT) {
            Ok(s) => {
                if let Some(o) = s.output {
                    self.current = o.into();
                }
                true
            }
            Err(_) => false,
        }
    }

    fn check(&mut self) {
        let now = Instant::now();
        if let Some(ch) = self.channel.take() {
            if ch.lost() {
                self.base = add(self.base, self.current);
                self.current = voice_link::RenderStats::default();
                self.next_open = now;
                tracing::warn!("voice render: the session agent was lost, opening the microphone output again");
            } else {
                if ch.ended() && self.shared.ended.get().is_none() {
                    let why = ch.stats_within(STATS_TIMEOUT).ok().and_then(|s| s.end_error);
                    let _ = self.shared.ended.set(why.unwrap_or_else(|| "the microphone output ended".into()));
                } else if now >= self.next_stats {
                    self.next_stats = now + STATS_EVERY;
                    self.current_stats(&ch);
                }
                self.channel = Some(ch);
            }
        }
        if self.channel.is_none() && self.shared.ended.get().is_none() && now >= self.next_open {
            self.reopen();
        }
        self.publish();
    }

    fn reopen(&mut self) {
        // No agent yet: the next round waits again (the wait paces it)
        if self.hub.wait_agent(REOPEN_WAIT).is_none() {
            return;
        }
        self.next_open = Instant::now() + REOPEN_EVERY;
        match open_remote(&self.hub, self.format, self.target.clone()) {
            Ok(ch) => {
                // What queued while no agent rendered is stale: the new
                // renderer starts from now
                loop {
                    let n = self.queue.pop(&mut self.buf);
                    self.discarded += n as u64;
                    if n < self.buf.len() {
                        break;
                    }
                }
                self.channel = Some(ch);
                let n = self.shared.reopened.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(
                    "voice render: the microphone output runs again in the session agent ({n} reopen(s), {} \
                     frames lost)",
                    self.discarded
                );
            }
            Err(e) if retryable(&e) => tracing::debug!("voice render: opening again failed ({e}), trying again"),
            Err(e) => {
                tracing::warn!("voice render: the microphone output does not open again");
                let _ = self.shared.ended.set(open_error(&e));
            }
        }
    }

    fn publish(&mut self) {
        let mut s = add(self.base, self.current);
        s.overflow += self.queue.stats().overflow + self.refused + self.discarded;
        *lock(&self.shared.stats) = s;
    }
}
