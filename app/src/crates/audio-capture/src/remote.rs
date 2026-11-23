//! Capture in another process (the session agent): the core keeps its
//! [`Capture`] / [`Running`] API, the agent runs the real source.
//!
//! This crate does not know how the core reaches its agent. The core
//! installs a [`CaptureAgent`] ([`use_capture_agent`], crate
//! `session-host`), which opens the source in the agent and hands back an
//! [`AgentChannel`]: the channel's audio link (pcm-link: a ring in shared
//! memory and a wake event, created by the core), a start, and a flag that
//! goes up when the agent is lost. In the agent, a [`CaptureWorker`] opens
//! the same link, runs the source and writes what it delivers into the
//! ring; a pump thread in the core, at MMCSS "Audio" priority like a
//! capture thread, hands it to the core's [`Sink`].
//!
//! Liveness: the pump waits on the wake event and checks the lost flag
//! (the link sets both when the agent goes), so an agent that dies ends the
//! capture ([`EndReason::AgentLost`]) and the owner opens it again as for a
//! lost device.

use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub use pcm_link::Link;
use pcm_link::{Kind, LOST_UNKNOWN, Reader, Stat, Writer};
use serde::{Deserialize, Serialize};

use crate::{
    Capture, CaptureError, CaptureOptions, EndReason, Format, PanicGuard, Running, Shared, Sink, Source, register_mmcss,
};

/// Opens captures in the session agent (installed by the core)
pub trait CaptureAgent: Send + Sync + 'static {
    /// Opens `source` in the agent: the format it delivers, the speakers
    /// of its channels ([`Capture::channel_mask`]) and the channel to
    /// start. Blocks the calling thread until the agent answers (or its
    /// timeout): not for an audio thread.
    fn open(
        &self,
        source: &Source,
        options: &CaptureOptions,
    ) -> Result<(Format, u32, Box<dyn AgentChannel>), CaptureError>;
}

/// One capture channel in the agent. Dropping it closes the channel there
/// (without waiting for the agent).
pub trait AgentChannel: Send + Debug + 'static {
    /// The ring the agent writes into
    fn link(&self) -> Arc<Link>;
    /// Starts the agent's capture (blocks until the agent answers)
    fn start(&mut self) -> Result<(), CaptureError>;
    /// Goes up (and the link's wake event is set) when the agent is lost
    fn lost_flag(&self) -> Arc<AtomicBool>;
}

static AGENT: OnceLock<Arc<dyn CaptureAgent>> = OnceLock::new();

/// From now on every [`Capture::open`] of this process runs the source in
/// the session agent. Set once, at start (later calls are ignored).
pub fn use_capture_agent(agent: Arc<dyn CaptureAgent>) {
    let _ = AGENT.set(agent);
}

/// Whether captures run in an agent
pub fn capture_agent_in_use() -> bool {
    AGENT.get().is_some()
}

pub(crate) fn agent() -> Option<&'static Arc<dyn CaptureAgent>> {
    AGENT.get()
}

/// Ring size the core gives a capture channel: 2.7 s of 48 kHz float
/// stereo, 90 ms of 192 kHz 8-channel 32-bit. The pump drains it
/// continuously; it only absorbs scheduling.
pub const CAPTURE_RING_BYTES: usize = 1 << 20;
/// The pump's wait between checks of the stop flag
const PUMP_WAIT_MS: u32 = 100;
/// The agent's mirror of its capture's counters
const MONITOR_PERIOD: Duration = Duration::from_millis(20);

/// A [`CaptureError`] on the wire: the variants the core tells apart, the
/// rest as their text
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureWireError {
    NoDevice,
    DeviceNotFound(String),
    UnsupportedBuild(u32),
    ProcessNotFound(u32),
    Activation { hresult: u32, message: String },
    UnsupportedFormat(String),
    Other { hresult: Option<u32>, message: String },
}

impl From<&CaptureError> for CaptureWireError {
    fn from(e: &CaptureError) -> CaptureWireError {
        match e {
            CaptureError::NoDevice => CaptureWireError::NoDevice,
            CaptureError::DeviceNotFound(id) => CaptureWireError::DeviceNotFound(id.clone()),
            CaptureError::UnsupportedBuild(b) => CaptureWireError::UnsupportedBuild(*b),
            CaptureError::ProcessNotFound(p) => CaptureWireError::ProcessNotFound(*p),
            CaptureError::Activation { hresult, message } => {
                CaptureWireError::Activation { hresult: *hresult, message: message.clone() }
            }
            CaptureError::UnsupportedFormat(f) => CaptureWireError::UnsupportedFormat(f.clone()),
            CaptureError::Remote { hresult, message } => {
                CaptureWireError::Other { hresult: *hresult, message: message.clone() }
            }
            other => CaptureWireError::Other { hresult: other.hresult(), message: other.to_string() },
        }
    }
}

impl From<CaptureWireError> for CaptureError {
    fn from(e: CaptureWireError) -> CaptureError {
        match e {
            CaptureWireError::NoDevice => CaptureError::NoDevice,
            CaptureWireError::DeviceNotFound(id) => CaptureError::DeviceNotFound(id),
            CaptureWireError::UnsupportedBuild(b) => CaptureError::UnsupportedBuild(b),
            CaptureWireError::ProcessNotFound(p) => CaptureError::ProcessNotFound(p),
            CaptureWireError::Activation { hresult, message } => CaptureError::Activation { hresult, message },
            CaptureWireError::UnsupportedFormat(f) => CaptureError::UnsupportedFormat(f),
            CaptureWireError::Other { hresult, message } => CaptureError::Remote { hresult, message },
        }
    }
}

/// [`Stat::EndCode`] values (0: running)
fn end_code(reason: Option<EndReason>) -> (u64, u64) {
    match reason {
        Some(EndReason::DeviceInvalidated) => (1, 0),
        Some(EndReason::ProcessExited) => (2, 0),
        Some(EndReason::ServiceStopped) => (3, 0),
        Some(EndReason::Error(hr)) => (4, u64::from(hr)),
        Some(EndReason::Panicked) => (5, 0),
        Some(EndReason::AgentLost) | None => (6, 0),
    }
}

fn end_reason(code: u64, detail: u64) -> EndReason {
    match code {
        1 => EndReason::DeviceInvalidated,
        2 => EndReason::ProcessExited,
        3 => EndReason::ServiceStopped,
        4 => EndReason::Error(detail as u32),
        5 => EndReason::Panicked,
        _ => EndReason::AgentLost,
    }
}

/// Starts what the agent opened: the agent's capture, and the pump to `sink`
pub(crate) fn start(
    mut channel: Box<dyn AgentChannel>,
    format: Format,
    sink: Box<dyn Sink>,
) -> Result<Running, CaptureError> {
    let reader = Reader::new(channel.link());
    channel.start()?;
    let (thread, shared) = start_pump(reader, channel.lost_flag(), format, sink)?;
    tracing::info!("capture agent: capturing");
    Ok(Running { format, shared, thread: Some(thread), remote: Some(channel) })
}

fn start_pump(
    reader: Reader,
    lost: Arc<AtomicBool>,
    format: Format,
    sink: Box<dyn Sink>,
) -> Result<(JoinHandle<()>, Arc<Shared>), CaptureError> {
    let shared = Arc::new(Shared::default());
    let thread_shared = shared.clone();
    let thread = thread::Builder::new()
        .name("capture-pump".into())
        .spawn(move || {
            let _guard = PanicGuard(thread_shared.clone());
            pump(reader, &lost, format, sink, &thread_shared);
        })
        .map_err(|e| CaptureError::Thread(e.to_string()))?;
    Ok((thread, shared))
}

/// The core's pump: ring records to the sink, the agent's end state to
/// [`Shared`], until stopped or the agent ends
fn pump(mut reader: Reader, lost: &AtomicBool, format: Format, mut sink: Box<dyn Sink>, shared: &Shared) {
    let _mmcss = register_mmcss();
    let link = reader.link().clone();
    let bytes_per_frame = format.bytes_per_frame();
    let mut last_packet: Option<Instant> = None;
    let mut corrupt = 0u64;
    let mut agent_gone = false;
    loop {
        // The end state first, then the records: everything written before
        // the end is delivered
        let end = link.stat(Stat::EndCode).load(Ordering::Acquire);
        loop {
            let record = match reader.pop() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(pcm_link::Corrupt) => {
                    corrupt += 1;
                    if corrupt == 1 {
                        tracing::warn!("capture agent: unreadable audio record, skipped");
                    }
                    sink.discontinuity(None);
                    shared.discontinuities.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let frames = record.frames as usize;
            if matches!(record.kind, Kind::Audio | Kind::Silence) {
                let now = Instant::now();
                if let Some(last) = last_packet {
                    let gap = now.duration_since(last).as_micros() as u64;
                    shared.packet_gap_us.fetch_max(gap, Ordering::Relaxed);
                }
                last_packet = Some(now);
            }
            match record.kind {
                Kind::Audio if record.len == frames * bytes_per_frame => {
                    sink.audio(reader.payload(&record), frames, format);
                    shared.frames.fetch_add(frames as u64, Ordering::Relaxed);
                }
                Kind::Audio => {
                    corrupt += 1;
                    sink.discontinuity(Some(frames as u64));
                    shared.discontinuities.fetch_add(1, Ordering::Relaxed);
                    shared.lost_frames.fetch_add(frames as u64, Ordering::Relaxed);
                }
                Kind::Silence => {
                    sink.silence(frames, format);
                    shared.silence_frames.fetch_add(frames as u64, Ordering::Relaxed);
                }
                Kind::Discontinuity => {
                    let lost = (record.lost != LOST_UNKNOWN).then_some(record.lost);
                    sink.discontinuity(lost);
                    shared.discontinuities.fetch_add(1, Ordering::Relaxed);
                    shared.lost_frames.fetch_add(lost.unwrap_or(0), Ordering::Relaxed);
                }
            }
        }
        shared.filled_frames.store(link.stat(Stat::SourceFilledFrames).load(Ordering::Relaxed), Ordering::Relaxed);
        let source_gap = link.stat(Stat::SourcePacketGapUs).swap(0, Ordering::Relaxed);
        shared.packet_gap_us.fetch_max(source_gap, Ordering::Relaxed);
        if end != 0 {
            let reason = end_reason(end, link.stat(Stat::EndDetail).load(Ordering::Relaxed));
            tracing::info!("capture agent: the capture ended ({reason:?})");
            shared.end(reason);
            break;
        }
        if agent_gone {
            let dropped = link.stat(Stat::DroppedRecords).load(Ordering::Relaxed);
            tracing::warn!("capture agent: the agent exited, capture lost ({dropped} record(s) dropped before)");
            shared.end(EndReason::AgentLost);
            break;
        }
        if shared.stop.load(Ordering::Acquire) {
            let dropped = link.stat(Stat::DroppedRecords).load(Ordering::Relaxed);
            let dropped_frames = link.stat(Stat::DroppedFrames).load(Ordering::Relaxed);
            tracing::debug!(
                "capture agent: pump stopped, {} record(s) read, {dropped} dropped ({dropped_frames} frames), {corrupt} unreadable",
                link.stat(Stat::ReaderRecords).load(Ordering::Relaxed)
            );
            break;
        }
        link.wait(PUMP_WAIT_MS);
        if lost.load(Ordering::Acquire) {
            // One more round for what it wrote before it went
            agent_gone = true;
        }
    }
}

/// The agent's sink: records into the ring. A record that does not fit is
/// dropped; a discontinuity with the frames lost goes ahead of the next one
/// that fits.
struct RingSink {
    writer: Writer,
    lost: Option<u64>,
}

impl RingSink {
    fn lose(&mut self, frames: u64) {
        *self.lost.get_or_insert(0) += frames;
    }

    fn flush_loss(&mut self) {
        if let Some(lost) = self.lost
            && self.writer.push(Kind::Discontinuity, 0, lost, &[])
        {
            self.lost = None;
        }
    }
}

impl Sink for RingSink {
    fn audio(&mut self, data: &[u8], frames: usize, _format: Format) {
        self.flush_loss();
        if !self.writer.push(Kind::Audio, frames as u32, 0, data) {
            self.lose(frames as u64);
        }
    }

    fn silence(&mut self, frames: usize, _format: Format) {
        self.flush_loss();
        if !self.writer.push(Kind::Silence, frames as u32, 0, &[]) {
            self.lose(frames as u64);
        }
    }

    fn discontinuity(&mut self, lost_frames: Option<u64>) {
        self.flush_loss();
        if !self.writer.push(Kind::Discontinuity, 0, lost_frames.unwrap_or(LOST_UNKNOWN), &[]) {
            self.lose(lost_frames.unwrap_or(0));
        }
    }
}

/// The agent's watch over its running capture: mirrors the counters into
/// the link and publishes the end
struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    fn start(running: Running, link: Arc<Link>) -> std::io::Result<Monitor> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::Builder::new().name("capture-agent-monitor".into()).spawn(move || {
            let mut ended = false;
            while !thread_stop.load(Ordering::Acquire) {
                link.stat(Stat::SourceFrames).store(running.frames(), Ordering::Relaxed);
                link.stat(Stat::SourceSilenceFrames).store(running.silence_frames(), Ordering::Relaxed);
                link.stat(Stat::SourceFilledFrames).store(running.filled_frames(), Ordering::Relaxed);
                link.stat(Stat::SourceDiscontinuities).store(running.discontinuities(), Ordering::Relaxed);
                link.stat(Stat::SourceLostFrames).store(running.lost_frames(), Ordering::Relaxed);
                let gap = running.take_packet_gap().as_micros() as u64;
                link.stat(Stat::SourcePacketGapUs).fetch_max(gap, Ordering::Relaxed);
                if !ended && running.ended() {
                    ended = true;
                    let reason = running.end_reason();
                    let (code, detail) = end_code(reason);
                    link.stat(Stat::EndDetail).store(detail, Ordering::Relaxed);
                    link.stat(Stat::EndCode).store(code, Ordering::Release);
                    link.wake();
                    tracing::info!("capture agent: the capture ended ({reason:?})");
                }
                thread::sleep(MONITOR_PERIOD);
            }
            // Dropping `running` stops and joins the capture thread
        })?;
        Ok(Monitor { stop, thread: Some(thread) })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Whether `channel` is a name the core gives a channel, or the handles of
/// one passed without a name (pcm_link::handle_channel)
pub fn valid_channel(channel: &str) -> bool {
    if pcm_link::parse_handle_channel(channel).is_some() {
        return true;
    }
    !channel.is_empty()
        && channel.len() <= 200
        && channel.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'\\' | b'_' | b'-'))
}

/// The agent's side of one capture channel: the source, opened and then
/// running into the channel's ring. Dropping it stops the source.
pub struct CaptureWorker {
    link: Arc<Link>,
    opened: Option<Capture>,
    monitor: Option<Monitor>,
}

impl Debug for CaptureWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureWorker").field("running", &self.monitor.is_some()).finish()
    }
}

impl CaptureWorker {
    /// Opens the ring `channel` the core created and `source` (in this
    /// process, never through an agent): the format it delivers and the
    /// speakers of its channels ([`Capture::channel_mask`])
    pub fn open(
        channel: &str,
        source: &Source,
        options: &CaptureOptions,
    ) -> Result<(CaptureWorker, Format, u32), CaptureError> {
        if !valid_channel(channel) {
            return Err(CaptureError::Agent("not a channel name".into()));
        }
        let link = Link::open(channel).map_err(|e| CaptureError::Agent(format!("cannot open the audio link: {e}")))?;
        let capture = Capture::open_local(source, options)?;
        let (format, channel_mask) = (capture.format(), capture.channel_mask());
        Ok((CaptureWorker { link, opened: Some(capture), monitor: None }, format, channel_mask))
    }

    /// Starts the source into the ring
    pub fn start(&mut self) -> Result<(), CaptureError> {
        let capture = self.opened.take().ok_or_else(|| CaptureError::Agent("start before open".into()))?;
        let sink = RingSink { writer: Writer::new(self.link.clone()), lost: None };
        let running = capture.start(Box::new(sink))?;
        let monitor = Monitor::start(running, self.link.clone()).map_err(|e| CaptureError::Thread(e.to_string()))?;
        self.monitor = Some(monitor);
        Ok(())
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        drop(self.monitor.take());
        drop(self.opened.take());
        let dropped = self.link.stat(Stat::DroppedRecords).load(Ordering::Relaxed);
        tracing::debug!(
            "capture agent: channel closed, {} record(s) written, {dropped} dropped",
            self.link.stat(Stat::WriterRecords).load(Ordering::Relaxed)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SampleFormat;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;

    static TEST_SEQ: AtomicU32 = AtomicU32::new(0);

    fn channel() -> String {
        format!(r"Local\BlueCaster_test_agent_{}_{}", std::process::id(), TEST_SEQ.fetch_add(1, Ordering::Relaxed))
    }

    #[derive(Default)]
    struct Got {
        frames: usize,
        silence: usize,
        gaps: Vec<Option<u64>>,
        first: Vec<u8>,
        blocks: usize,
    }

    struct TestSink(Arc<Mutex<Got>>);

    impl Sink for TestSink {
        fn audio(&mut self, data: &[u8], frames: usize, format: Format) {
            assert_eq!(data.len(), frames * format.bytes_per_frame());
            let mut g = self.0.lock().unwrap();
            if g.blocks == 0 {
                g.first = data.to_vec();
            }
            g.blocks += 1;
            g.frames += frames;
        }
        fn silence(&mut self, frames: usize, _format: Format) {
            self.0.lock().unwrap().silence += frames;
        }
        fn discontinuity(&mut self, lost: Option<u64>) {
            self.0.lock().unwrap().gaps.push(lost);
        }
    }

    fn running_of(reader: Reader, lost: &Arc<AtomicBool>, format: Format, got: &Arc<Mutex<Got>>) -> Running {
        let (thread, shared) = start_pump(reader, lost.clone(), format, Box::new(TestSink(got.clone()))).unwrap();
        Running { format, shared, thread: Some(thread), remote: None }
    }

    fn wait_until(what: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !what() {
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_test_tone_crosses_the_link() {
        let ch = channel();
        let link = Link::create(&ch, CAPTURE_RING_BYTES).unwrap();
        let opts = CaptureOptions { preferred_rate: Some(44_100), fill_silence: false };
        let (mut worker, format, mask) = CaptureWorker::open(&ch, &Source::TestTone, &opts).unwrap();
        assert_eq!(mask, 0x3, "the test tone is front left and right");
        assert_eq!(format, Format { sample_rate: 44_100, channels: 2, sample_format: SampleFormat::F32 });
        let got = Arc::new(Mutex::new(Got::default()));
        let lost = Arc::new(AtomicBool::new(false));
        let running = running_of(Reader::new(link.clone()), &lost, format, &got);
        worker.start().unwrap();
        thread::sleep(Duration::from_millis(500));
        assert!(!running.ended());
        let frames = running.frames();
        // The tone's 10 ms blocks, as many as in process
        assert!(frames >= 441 * 35 && frames.is_multiple_of(441), "{frames} frames");
        assert!(running.take_packet_gap() > Duration::ZERO);
        drop(worker);
        running.stop();
        let g = got.lock().unwrap();
        assert_eq!(&g.first[..8], &[0u8; 8], "the tone starts at sin(0)");
        assert!(g.frames as u64 >= frames);
        assert_eq!((g.silence, g.gaps.len()), (0, 0));
    }

    #[test]
    fn a_lost_agent_ends_the_capture() {
        let link = Link::create(&channel(), 64 << 10).unwrap();
        let mut writer = Writer::new(link.clone());
        let format = Format { sample_rate: 48_000, channels: 2, sample_format: SampleFormat::I16 };
        let got = Arc::new(Mutex::new(Got::default()));
        let lost = Arc::new(AtomicBool::new(false));
        let running = running_of(Reader::new(link.clone()), &lost, format, &got);
        assert!(writer.push(Kind::Audio, 4, 0, &[1; 16]));
        wait_until(|| running.frames() == 4);
        // Written just before it went: still delivered
        assert!(writer.push(Kind::Silence, 480, 0, &[]));
        lost.store(true, Ordering::Release);
        link.wake();
        wait_until(|| running.ended());
        assert_eq!(running.end_reason(), Some(EndReason::AgentLost));
        assert_eq!(running.silence_frames(), 480);
    }

    #[test]
    fn the_agents_end_reason_and_counters_reach_the_core() {
        let link = Link::create(&channel(), 64 << 10).unwrap();
        let mut writer = Writer::new(link.clone());
        let format = Format { sample_rate: 48_000, channels: 2, sample_format: SampleFormat::F32 };
        let got = Arc::new(Mutex::new(Got::default()));
        let lost = Arc::new(AtomicBool::new(false));
        let running = running_of(Reader::new(link.clone()), &lost, format, &got);
        assert!(writer.push(Kind::Discontinuity, 0, 96, &[]));
        assert!(writer.push(Kind::Discontinuity, 0, LOST_UNKNOWN, &[]));
        // A payload that does not match its frame count is not handed on
        assert!(writer.push(Kind::Audio, 3, 0, &[0; 8]));
        link.stat(Stat::SourceFilledFrames).store(960, Ordering::Relaxed);
        link.stat(Stat::SourcePacketGapUs).store(25_000, Ordering::Relaxed);
        let (code, detail) = end_code(Some(EndReason::Error(0x8889_0004)));
        link.stat(Stat::EndDetail).store(detail, Ordering::Relaxed);
        link.stat(Stat::EndCode).store(code, Ordering::Release);
        link.wake();
        wait_until(|| running.ended());
        assert_eq!(running.end_reason(), Some(EndReason::Error(0x8889_0004)));
        assert_eq!(running.discontinuities(), 3);
        assert_eq!(running.lost_frames(), 96 + 3);
        assert_eq!(running.filled_frames(), 960);
        assert!(running.take_packet_gap() >= Duration::from_millis(25));
        assert_eq!(running.frames(), 0);
        assert_eq!(got.lock().unwrap().gaps, [Some(96), None, Some(3)]);
    }

    #[test]
    fn a_full_ring_turns_into_a_discontinuity() {
        // Nobody reads: the sink drops, then reports the loss ahead of the
        // next record that fits
        let link = Link::create(&channel(), 3584).unwrap();
        let mut sink = RingSink { writer: Writer::new(link.clone()), lost: None };
        let format = Format { sample_rate: 48_000, channels: 2, sample_format: SampleFormat::I16 };
        let block = [0u8; 1000];
        for _ in 0..5 {
            sink.audio(&block, 250, format);
        }
        // Three fit; the loss of the fourth fits as a discontinuity ahead
        // of the fifth, which is lost too
        assert_eq!(sink.lost, Some(250));
        let mut reader = Reader::new(link.clone());
        let mut kinds = Vec::new();
        while let Some(r) = reader.pop().unwrap() {
            kinds.push((r.kind, r.lost));
        }
        assert_eq!(kinds, [(Kind::Audio, 0), (Kind::Audio, 0), (Kind::Audio, 0), (Kind::Discontinuity, 250)]);
        sink.silence(480, format);
        let a = reader.pop().unwrap().unwrap();
        assert_eq!((a.kind, a.lost), (Kind::Discontinuity, 250));
        assert_eq!(reader.pop().unwrap().unwrap().kind, Kind::Silence);
        assert_eq!(link.stat(Stat::DroppedFrames).load(Ordering::Relaxed), 500);
    }

    #[test]
    fn open_errors_are_the_sources() {
        let ch = channel();
        let _link = Link::create(&ch, 64 << 10).unwrap();
        let source = Source::RenderDevice { id: String::new() };
        let err = CaptureWorker::open(&ch, &source, &CaptureOptions::default()).unwrap_err();
        assert!(matches!(err, CaptureError::DeviceNotFound(_)), "{err}");
        // A channel the core did not create
        let err = CaptureWorker::open(&channel(), &Source::TestTone, &CaptureOptions::default()).unwrap_err();
        assert!(matches!(err, CaptureError::Agent(_)), "{err}");
        assert!(CaptureWorker::open("x/../y", &Source::TestTone, &CaptureOptions::default()).is_err());
        // Started twice
        let (mut worker, ..) = CaptureWorker::open(&ch, &Source::TestTone, &CaptureOptions::default()).unwrap();
        worker.start().unwrap();
        assert!(matches!(worker.start(), Err(CaptureError::Agent(_))));
    }

    #[test]
    fn errors_and_end_reasons_round_trip() {
        let errors = [
            CaptureError::NoDevice,
            CaptureError::DeviceNotFound("x".into()),
            CaptureError::UnsupportedBuild(18_000),
            CaptureError::ProcessNotFound(8),
            CaptureError::Activation { hresult: 0x8000_4005, message: "m".into() },
            CaptureError::UnsupportedFormat("f".into()),
            CaptureError::windows("GetBuffer", 0x8889_0004u32 as i32, "gone".into()),
            CaptureError::Timeout("start"),
        ];
        for e in &errors {
            let json = serde_json::to_string(&CaptureWireError::from(e)).unwrap();
            let back: CaptureError = serde_json::from_str::<CaptureWireError>(&json).unwrap().into();
            assert_eq!(back.to_string(), e.to_string());
            assert_eq!(back.hresult(), e.hresult());
        }
        for r in [
            EndReason::DeviceInvalidated,
            EndReason::ProcessExited,
            EndReason::ServiceStopped,
            EndReason::Error(0x8889_0004),
            EndReason::Panicked,
            EndReason::AgentLost,
        ] {
            let (code, detail) = end_code(Some(r));
            assert_ne!(code, 0);
            assert_eq!(end_reason(code, detail), r);
        }
    }

    #[test]
    fn channel_names() {
        assert!(valid_channel(r"Local\BlueCaster_Pcm-dev1-1234-0"));
        assert!(!valid_channel(""));
        assert!(!valid_channel(r"Local\a b"));
        assert!(!valid_channel("x/../y"));
        assert!(valid_channel("#1234.1240"));
        assert!(!valid_channel("#12.34.56"));
    }
}
