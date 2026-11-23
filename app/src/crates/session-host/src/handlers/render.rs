//! Group `render` in the agent: one [`Renderer`] per open channel, all
//! stopped when the connection ends. The local host runs the same
//! [`Renderer`] in the core's own process.
//!
//! The outputs: [`RenderTarget::Discard`]; [`RenderTarget::Endpoint`],
//! `voice_link`'s renderer (its own thread "voice-render" with the COM
//! apartment, MMCSS, the prefill and the drift control beside WASAPI), fed
//! through its lock-free queue; [`RenderTarget::Wav`], `voice_link`'s WAV
//! writer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use audio_capture::{Format, Link, SampleFormat};
use pcm_link::{Kind, LOST_UNKNOWN, Reader, Stat};
use serde_json::Value;
use voice_link::{PcmProducer, RenderError};

use crate::host::render::{OutputStats, RenderStats, RenderTarget, Request};
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler};

/// Channels one core may have open at once
const MAX_CHANNELS: usize = 8;
/// The renderer's wait between checks of its stop flag
const WAIT_MS: u32 = 20;
/// [`Stat::EndCode`] of a channel whose output ended by itself
const END_OUTPUT: u64 = 1;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a of `data`, continued from `hash`
pub(crate) fn fnv1a(mut hash: u64, data: &[u8]) -> u64 {
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// An output that did not open
#[derive(Debug)]
pub(crate) struct OpenError {
    /// For the log and the wire's message: no device IDs or paths
    pub message: String,
    /// The output's own error text (the failure's detail)
    pub full: String,
}

impl OpenError {
    fn both(text: String) -> OpenError {
        OpenError { message: text.clone(), full: text }
    }
}

/// What the PCM is handed to
trait Output: Send {
    fn audio(&mut self, data: &[u8], frames: u32);
    fn silence(&mut self, frames: u32);
    fn discontinuity(&mut self, lost: Option<u64>);
    /// Its own counters
    fn stats(&self) -> Option<OutputStats> {
        None
    }
    /// It stopped by itself: why
    fn ended(&self) -> Option<String> {
        None
    }
}

struct Discard;

impl Output for Discard {
    fn audio(&mut self, _data: &[u8], _frames: u32) {}
    fn silence(&mut self, _frames: u32) {}
    fn discontinuity(&mut self, _lost: Option<u64>) {}
}

/// Mono 16-bit PCM into a `voice_link` queue (the renderer's or the WAV
/// writer's)
struct VoiceQueue {
    producer: PcmProducer,
    pcm: Vec<i16>,
}

impl VoiceQueue {
    fn audio(&mut self, data: &[u8]) {
        self.pcm.clear();
        self.pcm.extend(data.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
        // What does not fit counts as the queue's overflow
        self.producer.push(&self.pcm);
    }

    fn silence(&mut self, frames: u32) {
        self.pcm.clear();
        self.pcm.resize(frames as usize, 0);
        self.producer.push(&self.pcm);
    }
}

struct EndpointOut {
    // The queue first: the renderer stops after its producer is gone
    queue: VoiceQueue,
    renderer: voice_link::Renderer,
}

impl Output for EndpointOut {
    fn audio(&mut self, data: &[u8], _frames: u32) {
        self.queue.audio(data);
    }
    fn silence(&mut self, frames: u32) {
        self.queue.silence(frames);
    }
    // The renderer writes silence when its queue runs dry and primes again
    fn discontinuity(&mut self, _lost: Option<u64>) {}
    fn stats(&self) -> Option<OutputStats> {
        Some(self.renderer.stats().into())
    }
    fn ended(&self) -> Option<String> {
        self.renderer.ended().then(|| self.renderer.end_error().unwrap_or("ended").to_string())
    }
}

struct WavOut {
    queue: VoiceQueue,
    _sink: voice_link::WavSink,
}

impl Output for WavOut {
    fn audio(&mut self, data: &[u8], _frames: u32) {
        self.queue.audio(data);
    }
    fn silence(&mut self, frames: u32) {
        self.queue.silence(frames);
    }
    fn discontinuity(&mut self, _lost: Option<u64>) {}
}

/// A render error's text without the device ID it may hold
fn endpoint_error(e: &RenderError) -> String {
    match e {
        RenderError::DeviceNotFound(_) => "audio output device not found".into(),
        other => other.to_string(),
    }
}

fn output_for(target: &RenderTarget, format: Format) -> Result<Box<dyn Output>, OpenError> {
    let voice = |what: &str| {
        if format.channels == 1 && format.sample_format == SampleFormat::I16 && format.sample_rate > 0 {
            Ok(format.sample_rate)
        } else {
            Err(OpenError::both(format!("{what} takes mono 16-bit PCM")))
        }
    };
    match target {
        RenderTarget::Discard => Ok(Box::new(Discard)),
        RenderTarget::Endpoint { device_id, prefill_ms } => {
            let rate = voice("an endpoint")?;
            let renderer = voice_link::Renderer::open(device_id, rate, *prefill_ms)
                .map_err(|e| OpenError { message: endpoint_error(&e), full: e.to_string() })?;
            let producer = renderer.producer();
            Ok(Box::new(EndpointOut { queue: VoiceQueue { producer, pcm: Vec::new() }, renderer }))
        }
        RenderTarget::Wav { path } => {
            let rate = voice("a WAV file")?;
            let mut sink = voice_link::WavSink::create(path, rate).map_err(|e| OpenError {
                message: format!("the WAV file was not created ({:?})", e.kind()),
                full: format!("{}: {e}", path.display()),
            })?;
            let producer = sink.producer().ok_or_else(|| OpenError::both("the WAV file has no queue".into()))?;
            Ok(Box::new(WavOut { queue: VoiceQueue { producer, pcm: Vec::new() }, _sink: sink }))
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    records: AtomicU64,
    frames: AtomicU64,
    silence_frames: AtomicU64,
    discontinuities: AtomicU64,
    lost_frames: AtomicU64,
    checksum: AtomicU64,
    /// The output's own counters, as the thread last saw them
    output: Mutex<Option<OutputStats>>,
    /// Why the output stopped by itself
    end_error: OnceLock<String>,
}

/// Reads a render channel's ring on its own thread and hands it to the
/// target's output. Dropping it stops the thread.
#[derive(Debug)]
pub struct Renderer {
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<()>>,
}

impl Renderer {
    pub(crate) fn start(link: Arc<Link>, format: Format, target: &RenderTarget) -> Result<Renderer, OpenError> {
        let mut output = output_for(target, format)?;
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        counters.checksum.store(FNV_OFFSET, Ordering::Relaxed);
        let (s, c) = (stop.clone(), counters.clone());
        let thread = std::thread::Builder::new().name("render-channel".into()).spawn(move || {
            let mut reader = Reader::new(link.clone());
            let mut checksum = FNV_OFFSET;
            loop {
                let stopping = s.load(Ordering::Acquire);
                loop {
                    let record = match reader.pop() {
                        Ok(Some(r)) => r,
                        Ok(None) => break,
                        Err(pcm_link::Corrupt) => {
                            c.discontinuities.fetch_add(1, Ordering::Relaxed);
                            output.discontinuity(None);
                            continue;
                        }
                    };
                    match record.kind {
                        Kind::Audio => {
                            let data = reader.payload(&record);
                            checksum = fnv1a(checksum, data);
                            c.checksum.store(checksum, Ordering::Relaxed);
                            output.audio(data, record.frames);
                            c.frames.fetch_add(u64::from(record.frames), Ordering::Relaxed);
                        }
                        Kind::Silence => {
                            output.silence(record.frames);
                            c.silence_frames.fetch_add(u64::from(record.frames), Ordering::Relaxed);
                        }
                        Kind::Discontinuity => {
                            let lost = (record.lost != LOST_UNKNOWN).then_some(record.lost);
                            output.discontinuity(lost);
                            c.discontinuities.fetch_add(1, Ordering::Relaxed);
                            c.lost_frames.fetch_add(lost.unwrap_or(0), Ordering::Relaxed);
                        }
                    }
                    // After the record's own counter: stats that show the
                    // record count show its frames too
                    c.records.fetch_add(1, Ordering::Release);
                }
                if let Some(o) = output.stats() {
                    *c.output.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
                }
                if c.end_error.get().is_none()
                    && let Some(why) = output.ended()
                {
                    let _ = c.end_error.set(why);
                    link.stat(Stat::EndCode).store(END_OUTPUT, Ordering::Release);
                    tracing::info!("render: a channel's output stopped");
                }
                if stopping {
                    break;
                }
                link.wait(WAIT_MS);
            }
        });
        let thread = thread.map_err(|e| OpenError::both(format!("cannot start the render thread: {e}")))?;
        Ok(Renderer { stop, counters, thread: Some(thread) })
    }

    pub(crate) fn stats(&self) -> RenderStats {
        let c = &self.counters;
        let records = c.records.load(Ordering::Acquire);
        RenderStats {
            records,
            frames: c.frames.load(Ordering::Relaxed),
            silence_frames: c.silence_frames.load(Ordering::Relaxed),
            discontinuities: c.discontinuities.load(Ordering::Relaxed),
            lost_frames: c.lost_frames.load(Ordering::Relaxed),
            checksum: c.checksum.load(Ordering::Relaxed),
            output: *c.output.lock().unwrap_or_else(|e| e.into_inner()),
            end_error: c.end_error.get().cloned(),
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Renders::default())
}

#[derive(Default)]
struct Renders {
    channels: HashMap<u64, Renderer>,
}

impl Handler for Renders {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::Open { channel, name, format, target } => {
                if self.channels.len() >= MAX_CHANNELS && !self.channels.contains_key(&channel) {
                    return Err(WireError::new(ErrorCode::Busy, "too many render channels"));
                }
                if !audio_capture::valid_channel(&name) {
                    return Err(WireError::new(ErrorCode::BadRequest, "not a channel name"));
                }
                self.channels.remove(&channel);
                let link = Link::open(&name).map_err(|e| WireError::new(ErrorCode::Failed, format!("ring: {e}")))?;
                let renderer = Renderer::start(link, format, &target).map_err(|e| {
                    tracing::warn!("render: a channel's output did not open ({})", e.message);
                    WireError::failed(e.message, &e.full)
                })?;
                self.channels.insert(channel, renderer);
                tracing::info!("render: channel opened ({} open)", self.channels.len());
                reply(())
            }
            Request::Stats { channel } => match self.channels.get(&channel) {
                Some(r) => reply(r.stats()),
                None => Err(WireError::new(ErrorCode::Failed, "no such channel")),
            },
            Request::Close { channel } => {
                if let Some(r) = self.channels.remove(&channel) {
                    let s = r.stats();
                    drop(r);
                    tracing::info!(
                        "render: channel closed, {} record(s) read ({} open)",
                        s.records,
                        self.channels.len()
                    );
                }
                reply(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalHost;
    use crate::host::render::RenderHost;
    use audio_capture::SampleFormat;
    use std::time::{Duration, Instant};

    const FORMAT: Format = Format { sample_rate: 16_000, channels: 1, sample_format: SampleFormat::I16 };

    fn wait_for(what: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !what() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_local_channel_renders_what_is_pushed() {
        let mut ch = LocalHost.open_render(FORMAT, RenderTarget::Discard).unwrap();
        let mut sum = FNV_OFFSET;
        for i in 0..50u8 {
            let block = [i; 320];
            sum = fnv1a(sum, &block);
            assert!(ch.push(&block));
        }
        assert!(ch.push_silence(160));
        assert!(ch.push_discontinuity(Some(32)));
        wait_for(|| ch.stats().unwrap().records == 52);
        let s = ch.stats().unwrap();
        assert_eq!((s.frames, s.silence_frames, s.discontinuities, s.lost_frames), (50 * 160, 160, 1, 32));
        assert_eq!(s.checksum, sum);
        assert!(!ch.lost() && !ch.ended());
    }

    #[test]
    fn the_handler_serves_channels() {
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let link = Link::create(&name, 64 << 10).unwrap();
        let open = Request::Open { channel: 3, name, format: FORMAT, target: RenderTarget::Discard };
        h.request(serde_json::to_value(open).unwrap()).unwrap();
        let mut w = pcm_link::Writer::new(link);
        assert!(w.push(Kind::Audio, 2, 0, &[1, 2, 3, 4]));
        let stats = |h: &mut Box<dyn Handler>| -> RenderStats {
            serde_json::from_value(h.request(serde_json::to_value(Request::Stats { channel: 3 }).unwrap()).unwrap())
                .unwrap()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while stats(&mut h).records == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(stats(&mut h).checksum, fnv1a(FNV_OFFSET, &[1, 2, 3, 4]));
        h.request(serde_json::to_value(Request::Close { channel: 3 }).unwrap()).unwrap();
        let err = h.request(serde_json::to_value(Request::Stats { channel: 3 }).unwrap()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Failed);
        let bad = Request::Open { channel: 4, name: "x/y".into(), format: FORMAT, target: RenderTarget::Discard };
        assert_eq!(h.request(serde_json::to_value(bad).unwrap()).unwrap_err().code, ErrorCode::BadRequest);
    }

    fn open(
        h: &mut Box<dyn Handler>,
        channel: u64,
        name: &str,
        format: Format,
        target: RenderTarget,
    ) -> Result<Value, WireError> {
        h.request(serde_json::to_value(Request::Open { channel, name: name.into(), format, target }).unwrap())
    }

    #[test]
    fn a_wav_target_gets_every_sample_and_is_finished_at_close() {
        let path = std::env::temp_dir().join(format!("session-host-render-{}.wav", std::process::id()));
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let link = Link::create(&name, 64 << 10).unwrap();
        open(&mut h, 1, &name, FORMAT, RenderTarget::Wav { path: path.clone() }).unwrap();
        let mut w = pcm_link::Writer::new(link);
        let pcm: Vec<i16> = (0..1600).map(|i| (i * 13) as i16).collect();
        let mut bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert!(w.push(Kind::Audio, 1600, 0, &bytes));
        assert!(w.push(Kind::Silence, 160, 0, &[]));
        assert!(w.push(Kind::Discontinuity, 0, LOST_UNKNOWN, &[]));
        // Closed at once: what was pushed is still written
        h.request(serde_json::to_value(Request::Close { channel: 1 }).unwrap()).unwrap();
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        bytes.extend([0; 320]);
        assert_eq!(&data[44..], &bytes[..]);
        assert_eq!(u32::from_le_bytes(data[24..28].try_into().unwrap()), 16_000);
    }

    #[test]
    fn a_missing_endpoint_fails_with_its_error_and_no_id_in_the_message() {
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let _link = Link::create(&name, 64 << 10).unwrap();
        let id = "{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}";
        let target = RenderTarget::Endpoint { device_id: id.into(), prefill_ms: 40 };
        let err = open(&mut h, 1, &name, FORMAT, target).unwrap_err();
        assert_eq!(err.code, ErrorCode::Failed);
        assert!(!err.message.contains(id), "{}", err.message);
        let full = err.detail_as::<String>().unwrap();
        assert_eq!(full, RenderError::DeviceNotFound(id.into()).to_string());
    }

    #[test]
    fn voice_targets_take_mono_16_bit_only() {
        let mut h = new(AgentContext::detached());
        let (_, name) = crate::hub::new_channel();
        let _link = Link::create(&name, 64 << 10).unwrap();
        let stereo = Format { channels: 2, ..FORMAT };
        let path = std::env::temp_dir().join(format!("session-host-render-stereo-{}.wav", std::process::id()));
        let err = open(&mut h, 1, &name, stereo, RenderTarget::Wav { path: path.clone() }).unwrap_err();
        assert_eq!(err.message, "a WAV file takes mono 16-bit PCM");
        assert!(!path.exists());
        let target = RenderTarget::Endpoint { device_id: String::new(), prefill_ms: 40 };
        let err = open(&mut h, 1, &name, stereo, target).unwrap_err();
        assert_eq!(err.message, "an endpoint takes mono 16-bit PCM");
    }

    #[test]
    fn stats_of_an_older_core_still_decode() {
        let old = serde_json::json!({"records": 1, "frames": 2, "silence_frames": 0, "discontinuities": 0,
            "lost_frames": 0, "checksum": 5});
        let s: RenderStats = serde_json::from_value(old).unwrap();
        assert_eq!((s.records, s.output, s.end_error), (1, None, None));
    }
}
