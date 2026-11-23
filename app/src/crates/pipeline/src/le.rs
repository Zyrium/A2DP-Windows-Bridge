//! The LE Audio (experimental) encode thread (v1 Stream's encoding half,
//! le_audio_stream.cpp):
//!
//! capture thread --[`CaptureInput`] (the same SPSC byte ring as A2DP)-->
//! LE encode thread --[`SduSink`] (bt-host's `LeSduSink`, the jitter
//! buffer [`le_audio::sdu_queue::SduQueue`])--> BTstack thread (ISO)
//!
//! Any captured PCM is downmixed to stereo (ITU-R BS.775 for more than two
//! channels, as v1), resampled to the stream's LC3 rate if it differs
//! (linear interpolation; the capture is normally asked for that rate
//! already), cut into LC3 frames (10 or 7.5 ms) and encoded once per output
//! channel: Left / Right take that side, Mono the average of both. A plan
//! with rear channels ([`ChannelSource::RearLeft`] / `RearRight`, the
//! Auracast speaker broadcasts) takes the source's speakers by its channel
//! mask instead of the downmix: front left / right (with the centre and
//! LFE folded in) and rear left / right (back and side). The
//! frames of one interval make one SDU set: one SDU per CIS with a sink
//! direction in plan order ([`le_audio::plan::StreamPlan::sink_cis`]),
//! each holding its channels' frames in ascending Audio Location bit order
//! (BAP: the channels of a CIS's SDU are ordered by location).
//!
//! No allocation per round once a stream runs: the frame buffers are
//! preallocated and the sets are encoded into spare sets of the queue
//! ([`SduSink::spare`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bt_host::{LeSduSink, LinkStats};
use codec::lc3::{Lc3ChannelEncoder, Lc3Error, Lc3Params};
use le_audio::plan::{ChannelSource, StreamPlan};
use le_audio::sdu_queue::{SduQueue, SduSet};

use crate::{BufferPreset, CaptureInput, Control, InputFormat, SampleType};

/// Where the LE encode thread puts its SDU sets
pub trait SduSink: Send + 'static {
    /// One SDU per CIS for the next frame interval, in plan order
    fn push(&self, set: SduSet);

    /// A set to encode into (a sent one, given back by the sender), so that
    /// no round allocates; None: the encoder allocates one
    fn spare(&self) -> Option<SduSet> {
        None
    }
}

impl SduSink for LeSduSink {
    fn push(&self, set: SduSet) {
        LeSduSink::push(self, set);
    }

    fn spare(&self) -> Option<SduSet> {
        LeSduSink::spare(self)
    }
}

/// The jitter buffer itself (bt-host's SharedSduQueue; tests)
impl SduSink for Arc<Mutex<SduQueue>> {
    fn push(&self, set: SduSet) {
        self.lock().unwrap_or_else(|e| e.into_inner()).push(set);
    }

    fn spare(&self) -> Option<SduSet> {
        self.lock().unwrap_or_else(|e| e.into_inner()).spare()
    }
}

/// Where one output channel's LC3 frame goes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSlot {
    /// Index among the CIS with a sink direction (plan order) = SDU index in
    /// the set
    pub cis: usize,
    /// Byte offset of the frame in that SDU
    pub offset: usize,
    /// Audio location bit (0 = mono)
    pub allocation: u32,
    pub source: ChannelSource,
    /// Its LC3 configuration (its CIS's preset)
    pub params: Lc3Params,
    /// The layout's rate over its own (1: the same; 2: a 24 kHz BIS of a
    /// broadcast whose front runs at 48 kHz)
    pub decimate: usize,
}

/// The LC3 parameters and SDU layout of a stream plan
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SduLayout {
    /// The configuration at the plan's top rate (the encode thread's rate;
    /// a channel of a lower rate has its own in its slot)
    pub params: Lc3Params,
    /// PCM samples per frame and channel, at the top rate
    pub frame_samples: usize,
    /// SDU length per CIS (plan order): octets x channels
    pub sdu_lens: Vec<usize>,
    /// The output channels, one LC3 encoder each, CIS by CIS
    pub channels: Vec<ChannelSlot>,
}

impl SduLayout {
    pub fn from_plan(plan: &StreamPlan) -> Result<SduLayout, String> {
        let lc3 = |p: &le_audio::presets::StreamPreset| Lc3Params {
            sample_rate: p.sampling_hz,
            frame_us: u32::from(p.frame_us),
            octets: p.octets,
        };
        let top = plan.top_sampling_hz();
        let preset = plan.sink_cis().map(|c| plan.preset_of(c)).find(|p| p.sampling_hz == top).unwrap_or(&plan.preset);
        let params = lc3(preset);
        let frame_samples = params.frame_samples().map_err(|e| {
            format!("LC3 {} Hz, {} us, {} octets: {e}", params.sample_rate, params.frame_us, params.octets)
        })?;
        // The CIS with a sink direction only (a source-only CIS of a
        // conversational plan carries the microphone the other way)
        if plan.sink_cis().next().is_none() {
            return Err("no CIS in the stream plan".into());
        }
        let mut sdu_lens = Vec::with_capacity(plan.cis.len());
        let mut channels = Vec::with_capacity(plan.cis.iter().map(|c| c.channels.len()).sum());
        for (c, cis) in plan.sink_cis().enumerate() {
            let own = lc3(plan.preset_of(cis));
            if own.frame_us != params.frame_us || own.sample_rate == 0 || !top.is_multiple_of(own.sample_rate) {
                return Err(format!(
                    "LC3 {} Hz {} us next to {} Hz {} us in one stream",
                    own.sample_rate, own.frame_us, params.sample_rate, params.frame_us
                ));
            }
            own.frame_samples().map_err(|e| format!("LC3 {} Hz, {} octets: {e}", own.sample_rate, own.octets))?;
            let decimate = (top / own.sample_rate) as usize;
            let octets = usize::from(own.octets);
            // SDU order: ascending location bit (the plan lists them so;
            // sorted again here so the layout never depends on it)
            let mut order: Vec<_> = cis.channels.iter().collect();
            order.sort_by_key(|ch| ch.allocation);
            for (pos, ch) in order.into_iter().enumerate() {
                channels.push(ChannelSlot {
                    cis: c,
                    offset: pos * octets,
                    allocation: ch.allocation,
                    source: ch.source,
                    params: own,
                    decimate,
                });
            }
            sdu_lens.push(octets * cis.channels.len());
        }
        Ok(SduLayout { params, frame_samples, sdu_lens, channels })
    }

    /// A zeroed set of the right shape
    pub fn empty_set(&self) -> SduSet {
        self.sdu_lens.iter().map(|&n| vec![0; n]).collect()
    }

    fn fits(&self, set: &SduSet) -> bool {
        set.len() == self.sdu_lens.len() && set.iter().zip(&self.sdu_lens).all(|(s, &n)| s.len() == n)
    }
}

/// The silence set for bt-host's `le_start_stream` (`silence` closure):
/// one SDU per CIS, every channel an LC3 frame of digital silence (v1 Stream
/// encoded it once per stream). Zero bytes if the plan cannot be encoded
/// (then the stream fails in [`LePipeline::start`] anyway).
pub fn silence_set(plan: &StreamPlan) -> SduSet {
    let layout = match SduLayout::from_plan(plan) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("LE Audio: no silence SDUs: {e}");
            return plan.sink_cis().map(|c| vec![0; usize::from(plan.preset.octets) * c.channels.len()]).collect();
        }
    };
    let mut set = layout.empty_set();
    for ch in &layout.channels {
        let octets = usize::from(ch.params.octets);
        let mut frame = vec![0u8; octets];
        if let Err(e) = codec::lc3::silence_frame(ch.params, &mut frame) {
            tracing::error!("LE Audio: LC3 silence: {e}");
        }
        set[ch.cis][ch.offset..ch.offset + octets].copy_from_slice(&frame);
    }
    set
}

/// -3 dB (v1: 0.707107f)
pub(crate) const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Stereo from one input frame's channels (v1: mono on both sides; more
/// than two channels downmixed as ITU-R BS.775 in WASAPI's channel order FL
/// FR FC LFE BL BR SL SR, channels beyond eight ignored)
pub fn downmix(ch: &[f32]) -> (f32, f32) {
    let at = |i: usize| ch.get(i).copied().unwrap_or(0.0);
    match ch.len() {
        0 => (0.0, 0.0),
        1 => (ch[0], ch[0]),
        2 => (ch[0], ch[1]),
        _ => {
            let (c, lfe) = (at(2), at(3));
            let (bl, br, sl, sr) = (at(4), at(5), at(6), at(7));
            let l = ch[0] + MINUS_3DB * c + MINUS_3DB * (bl + sl) + 0.5 * lfe;
            let r = ch[1] + MINUS_3DB * c + MINUS_3DB * (br + sr) + 0.5 * lfe;
            (l, r)
        }
    }
}

/// Lanes the encoder fills per input frame: front left and right (the
/// stereo downmix, or the front speakers), rear left and right
pub const LANES: usize = 4;

/// Input channels the encoder reads (more are ignored)
pub(crate) const MAX_INPUT: usize = 8;

/// WAVEFORMATEXTENSIBLE dwChannelMask speaker bits
pub mod speaker {
    pub const FRONT_LEFT: u32 = 0x1;
    pub const FRONT_RIGHT: u32 = 0x2;
    pub const FRONT_CENTER: u32 = 0x4;
    pub const LOW_FREQUENCY: u32 = 0x8;
    pub const BACK_LEFT: u32 = 0x10;
    pub const BACK_RIGHT: u32 = 0x20;
    pub const FRONT_LEFT_OF_CENTER: u32 = 0x40;
    pub const FRONT_RIGHT_OF_CENTER: u32 = 0x80;
    pub const BACK_CENTER: u32 = 0x100;
    pub const SIDE_LEFT: u32 = 0x200;
    pub const SIDE_RIGHT: u32 = 0x400;
}

/// The usual mask of a channel count (the order [`downmix`] assumes), for
/// a source that states none or one that does not match its channels
fn default_mask(channels: usize) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        _ => 0x63F,
    }
}

/// The speaker of each interleaved input channel: the mask's bits in
/// ascending order (WAVEFORMATEXTENSIBLE), 0 for a channel it does not name
pub(crate) fn speakers(channels: usize, mask: u32) -> [u32; MAX_INPUT] {
    let mask = if mask.count_ones() as usize >= channels.min(MAX_INPUT) { mask } else { default_mask(channels) };
    let mut out = [0; MAX_INPUT];
    let bits = (0..32).map(|b| 1u32 << b).filter(|b| mask & b != 0);
    for (o, bit) in out.iter_mut().take(channels).zip(bits) {
        *o = bit;
    }
    out
}

/// Gains of the input channels for each lane, front left / right / rear
/// left / right, for a source of `channels` with the speakers of `mask`.
/// The centre goes to both fronts at -3 dB and the LFE at -6 dB (as the
/// downmix); back and side make the rear (-3 dB each when both are there).
/// A source without rear speakers plays its front on the rear too. Also
/// says whether it has rear speakers.
pub fn speaker_matrix(channels: usize, mask: u32) -> ([[f32; MAX_INPUT]; LANES], bool) {
    use speaker::*;
    let sp = speakers(channels, mask);
    let has = |bit: u32| sp.contains(&bit);
    let mut m = [[0.0f32; MAX_INPUT]; LANES];
    let mono = channels == 1;
    for (i, &bit) in sp.iter().enumerate().take(channels) {
        match bit {
            _ if mono => m.iter_mut().for_each(|lane| lane[i] = 1.0),
            FRONT_LEFT | FRONT_LEFT_OF_CENTER => m[0][i] = 1.0,
            FRONT_RIGHT | FRONT_RIGHT_OF_CENTER => m[1][i] = 1.0,
            FRONT_CENTER => (m[0][i], m[1][i]) = (MINUS_3DB, MINUS_3DB),
            LOW_FREQUENCY => (m[0][i], m[1][i]) = (0.5, 0.5),
            BACK_LEFT => m[2][i] = if has(SIDE_LEFT) { MINUS_3DB } else { 1.0 },
            SIDE_LEFT => m[2][i] = if has(BACK_LEFT) { MINUS_3DB } else { 1.0 },
            BACK_RIGHT => m[3][i] = if has(SIDE_RIGHT) { MINUS_3DB } else { 1.0 },
            SIDE_RIGHT => m[3][i] = if has(BACK_RIGHT) { MINUS_3DB } else { 1.0 },
            BACK_CENTER => (m[2][i], m[3][i]) = (MINUS_3DB, MINUS_3DB),
            _ => {}
        }
    }
    let rear = mono || [BACK_LEFT, BACK_RIGHT, SIDE_LEFT, SIDE_RIGHT, BACK_CENTER].iter().any(|&b| has(b));
    if !rear {
        (m[2], m[3]) = (m[0], m[1]);
    }
    (m, rear)
}

/// How the input channels make the lanes
#[derive(Debug, Clone, PartialEq)]
enum Mix {
    /// The fronts from [`downmix`], the rear lanes silent
    Stereo,
    /// The speakers, by [`speaker_matrix`]
    Speakers([[f32; MAX_INPUT]; LANES]),
}

impl Mix {
    /// Lanes processed (and EQ'd)
    fn lanes(&self) -> usize {
        match self {
            Mix::Stereo => 2,
            Mix::Speakers(_) => LANES,
        }
    }

    #[inline]
    fn apply(&self, ch: &[f32]) -> [f32; LANES] {
        match self {
            Mix::Stereo => {
                let (l, r) = downmix(ch);
                [l, r, 0.0, 0.0]
            }
            Mix::Speakers(m) => std::array::from_fn(|lane| ch.iter().zip(&m[lane]).map(|(x, g)| x * g).sum()),
        }
    }
}

/// Linear interpolation from the capture rate to the LC3 rate (v1
/// push_pcm). Each input sample yields the output samples between the
/// previous input sample (position 0) and this one (position 1).
#[derive(Debug, Clone, PartialEq)]
pub struct LinearResampler {
    /// Input samples per output sample
    step: f64,
    passthrough: bool,
    /// Fractional input position of the next output sample
    pos: f64,
    last: [f32; LANES],
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> LinearResampler {
        LinearResampler {
            step: f64::from(in_rate) / f64::from(out_rate.max(1)),
            passthrough: in_rate == out_rate,
            pos: 0.0,
            last: [0.0; LANES],
        }
    }

    pub fn passthrough(&self) -> bool {
        self.passthrough
    }

    /// One input sample pair; `out` gets the output sample pairs it yields
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, out: &mut impl FnMut(f32, f32)) {
        self.process_lanes(&[l, r, 0.0, 0.0], &mut |s| out(s[0], s[1]));
    }

    /// One input sample of every lane; `out` gets the output samples
    #[inline]
    pub fn process_lanes(&mut self, s: &[f32; LANES], out: &mut impl FnMut(&[f32; LANES])) {
        if self.passthrough {
            out(s);
            return;
        }
        let last = self.last;
        while self.pos < 1.0 {
            let a = self.pos as f32;
            out(&std::array::from_fn(|i| last[i] + (s[i] - last[i]) * a));
            self.pos += self.step;
        }
        self.pos -= 1.0;
        self.last = *s;
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.last = [0.0; LANES];
    }
}

/// Frames the LE path hands the playback EQ at a time
const EQ_BLOCK: usize = 512;

/// Float to 16-bit as v1 (clamped, lrintf: rounded to nearest, ties even)
#[inline]
fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round_ties_even() as i16
}

#[inline]
fn input_sample(raw: &[u8], i: usize, t: SampleType) -> f32 {
    match t {
        SampleType::F32 => f32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]]),
        SampleType::I16 => f32::from(i16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]])) / 32768.0,
        SampleType::I32 => {
            i32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]]) as f32 / 2_147_483_648.0
        }
    }
}

/// One channel's audio at a lower rate than the stream's top rate (a
/// matrix broadcast's 24 kHz rear while the front runs at 48 kHz): a
/// windowed-sinc low-pass below the new Nyquist frequency, then every
/// `factor`-th sample. At factor 1 the same delay only, so the channels
/// that keep the top rate stay in step with the decimated ones.
struct Decimator {
    factor: usize,
    taps: Vec<f32>,
    /// Older samples before the newest a tap starts at (the pure delay)
    skip: usize,
    /// The last [`Decimator::HISTORY`] input samples, then the frame's
    hist: Vec<f32>,
}

impl Decimator {
    /// FIR length (odd: its delay is a whole number of samples)
    const TAPS: usize = 63;
    const HISTORY: usize = Decimator::TAPS - 1;
    /// The filter's delay at the top rate (samples)
    const DELAY: usize = Decimator::HISTORY / 2;

    fn new(factor: usize, frame: usize) -> Decimator {
        use std::f64::consts::PI;
        let (taps, skip) = if factor <= 1 {
            (vec![1.0], Decimator::DELAY)
        } else {
            // Cut-off at 90 % of the new Nyquist frequency (cycles per
            // input sample), Blackman window, unity gain at DC
            let fc = 0.45 / factor as f64;
            let m = Decimator::HISTORY as f64;
            let mut taps: Vec<f32> = (0..Decimator::TAPS)
                .map(|i| {
                    let x = i as f64 - m / 2.0;
                    let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
                    let a = 2.0 * PI * i as f64 / m;
                    (sinc * (0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos())) as f32
                })
                .collect();
            let sum: f32 = taps.iter().sum();
            taps.iter_mut().for_each(|t| *t /= sum);
            (taps, 0)
        };
        Decimator { factor: factor.max(1), taps, skip, hist: vec![0.0; Decimator::HISTORY + frame] }
    }

    /// One frame of a lane into `out` (`input.len() / factor` samples)
    fn process(&mut self, input: &[f32], out: &mut [i16]) {
        let h = Decimator::HISTORY;
        self.hist[h..h + input.len()].copy_from_slice(input);
        for (m, o) in out.iter_mut().enumerate() {
            let newest = h + m * self.factor + self.factor - 1 - self.skip;
            let acc: f32 = self.taps.iter().enumerate().map(|(j, t)| t * self.hist[newest - j]).sum();
            *o = to_i16(acc);
        }
        self.hist.copy_within(input.len()..input.len() + h, 0);
    }

    fn reset(&mut self) {
        self.hist.fill(0.0);
    }
}

/// One LC3 frame being filled, and its encoding into a set
struct FrameEncoder {
    layout: SduLayout,
    encoders: Vec<Lc3ChannelEncoder>,
    /// Per channel, when the stream mixes rates: the decimation of a
    /// lower-rate channel, or the same delay for one at the top rate
    rates: Vec<Option<Decimator>>,
    /// Per lane, the frame's samples
    lanes: [Vec<f32>; LANES],
    /// A mono channel's frame (the lanes' left and right)
    mono: Vec<f32>,
    fill: usize,
    pcm16: Vec<i16>,
    sets: u64,
    errors: u64,
    clock: crate::EncodeClock,
    /// Every channel gets the mono mix (an earbud left alone while the
    /// other one is gone), whatever its plan says
    all_mono: Arc<AtomicBool>,
}

impl FrameEncoder {
    #[inline]
    fn add(&mut self, s: &[f32; LANES], sink: &dyn SduSink) {
        for (lane, &x) in self.lanes.iter_mut().zip(s) {
            lane[self.fill] = x;
        }
        self.fill += 1;
        if self.fill == self.layout.frame_samples {
            self.fill = 0;
            self.encode_frame(sink);
        }
    }

    fn encode_frame(&mut self, sink: &dyn SduSink) {
        let mut set = match sink.spare() {
            Some(s) if self.layout.fits(&s) => s,
            _ => self.layout.empty_set(),
        };
        let started = Instant::now();
        let ns = self.layout.frame_samples;
        let all_mono = self.all_mono.load(Ordering::Relaxed);
        let Self { layout, encoders, rates, lanes, mono, pcm16, errors, .. } = self;
        for ((slot, enc), rate) in layout.channels.iter().zip(encoders.iter_mut()).zip(rates.iter_mut()) {
            let [l, r, rl, rr] = &*lanes;
            let source = if all_mono { ChannelSource::Mono } else { slot.source };
            let lane: &[f32] = match source {
                ChannelSource::Left => l,
                ChannelSource::Right => r,
                ChannelSource::RearLeft => rl,
                ChannelSource::RearRight => rr,
                ChannelSource::Mono => {
                    for (m, (&l, &r)) in mono.iter_mut().zip(l.iter().zip(r)) {
                        *m = 0.5 * (l + r);
                    }
                    mono
                }
            };
            let pcm = &mut pcm16[..ns / slot.decimate];
            match rate {
                Some(d) => d.process(&lane[..ns], pcm),
                None => pcm.iter_mut().zip(lane).for_each(|(s, &x)| *s = to_i16(x)),
            }
            let octets = usize::from(slot.params.octets);
            let out = &mut set[slot.cis][slot.offset..slot.offset + octets];
            if let Err(e) = enc.encode(pcm, out) {
                encode_error(errors, e);
                out.fill(0);
            }
        }
        self.clock.add(started.elapsed(), u64::from(self.layout.params.frame_us) * 1000);
        self.sets += 1;
        tracing::trace!("LE encode thread: SDU set {} ({} SDUs)", self.sets, set.len());
        sink.push(set);
    }
}

#[cold]
fn encode_error(errors: &mut u64, e: Lc3Error) {
    *errors += 1;
    if *errors == 1 || errors.is_multiple_of(1000) {
        tracing::error!("LE encode thread: {e} ({errors} errors)");
    }
}

/// Downmix, resampling and LC3 encoding of a stream (no thread; the LE
/// encode thread drives it, tests call it directly)
pub struct LeEncoder {
    input: InputFormat,
    mix: Mix,
    /// The source's speakers (dwChannelMask; 0: the usual ones)
    mask: u32,
    /// The broadcast mixer, instead of `mix`, with delays and polarity
    mixer: Option<crate::mix::Mixer>,
    resampler: LinearResampler,
    frame: FrameEncoder,
    /// The playback EQ (at the capture rate, after the downmix), and room
    /// for a block of stereo audio for it
    eq: Option<crate::eq::EqProcessor>,
    eq_buf: Vec<f32>,
    eq_was_on: bool,
}

impl LeEncoder {
    /// Encoders for the plan's channels, for audio arriving in `input`
    pub fn new(plan: &StreamPlan, input: InputFormat) -> Result<LeEncoder, String> {
        if input.channels == 0 || input.sample_rate == 0 {
            return Err(format!("capture format {} Hz, {} channels", input.sample_rate, input.channels));
        }
        let layout = SduLayout::from_plan(plan)?;
        let encoders = layout
            .channels
            .iter()
            .map(|slot| Lc3ChannelEncoder::new(slot.params))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                format!(
                    "LE Audio: LC3 encoder configuration failed ({} Hz, {} octets): {e}",
                    layout.params.sample_rate, layout.params.octets
                )
            })?;
        let ns = layout.frame_samples;
        let rate = layout.params.sample_rate;
        tracing::info!(
            "LE Audio: LC3 encoders ready: {rate} Hz, {} us frames ({ns} samples), {} octets, {} channel(s)",
            layout.params.frame_us,
            layout.params.octets,
            layout.channels.len()
        );
        if input.sample_rate != rate {
            tracing::info!("LE Audio: capture at {} Hz, LC3 at {rate} Hz (resampling)", input.sample_rate);
        }
        // Mixed rates: the lower-rate channels decimated, the others
        // delayed as much, so all stay in step
        let mixed = layout.channels.iter().any(|s| s.decimate > 1);
        let rates: Vec<Option<Decimator>> =
            layout.channels.iter().map(|s| mixed.then(|| Decimator::new(s.decimate, ns))).collect();
        if mixed {
            tracing::info!(
                "LE Audio: channels at {:?} Hz (decimated from {rate} Hz, {:.2} ms filter delay on all)",
                layout.channels.iter().map(|s| s.params.sample_rate).collect::<Vec<_>>(),
                Decimator::DELAY as f64 * 1000.0 / f64::from(rate)
            );
        }
        tracing::debug!(
            "LE Audio: input {:?}, {}, SDU lengths {:?} (plan cis ids {:?})",
            input,
            match input.channels {
                1 => "mono on both sides",
                2 => "stereo",
                _ => "downmixed to stereo (ITU-R BS.775)",
            },
            layout.sdu_lens,
            plan.cis.iter().map(|c| c.cis_id).collect::<Vec<_>>()
        );
        for ch in &layout.channels {
            tracing::debug!(
                "LE Audio: channel {:?} -> CIS #{} offset {}, location 0x{:08x}",
                ch.source,
                ch.cis,
                ch.offset,
                ch.allocation
            );
        }
        // The speakers one by one when the plan has rear channels; the
        // source's mask comes with [`LeEncoder::with_channel_mask`]
        let mix = if layout.channels.iter().any(|c| c.source.is_rear()) {
            Mix::Speakers(speaker_matrix(usize::from(input.channels), 0).0)
        } else {
            Mix::Stereo
        };
        Ok(LeEncoder {
            input,
            mix,
            mask: 0,
            mixer: None,
            resampler: LinearResampler::new(input.sample_rate, rate),
            frame: FrameEncoder {
                layout,
                encoders,
                rates,
                lanes: std::array::from_fn(|_| vec![0.0; ns]),
                mono: vec![0.0; ns],
                fill: 0,
                pcm16: vec![0; ns],
                sets: 0,
                errors: 0,
                clock: crate::EncodeClock::default(),
                all_mono: Arc::new(AtomicBool::new(false)),
            },
            eq: None,
            eq_buf: Vec::new(),
            eq_was_on: false,
        })
    }

    /// The speakers of the input channels (WAVEFORMATEXTENSIBLE
    /// dwChannelMask; 0 = the usual ones of the channel count). Matters only
    /// to a plan with rear channels. Before [`LeEncoder::with_eq`].
    pub fn with_channel_mask(mut self, mask: u32) -> LeEncoder {
        self.mask = mask;
        if let Mix::Speakers(_) = self.mix {
            let channels = usize::from(self.input.channels);
            let (m, rear) = speaker_matrix(channels, mask);
            tracing::info!(
                "LE Audio: speakers of the source (mask 0x{mask:x}, {channels} ch): {:x?}{}",
                &speakers(channels, mask)[..channels.min(MAX_INPUT)],
                if rear { "" } else { "; no rear speakers, the rear channels carry the front" }
            );
            self.mix = Mix::Speakers(m);
        }
        self
    }

    /// The broadcast mixer of `handle` instead of the fixed downmix /
    /// speaker matrix: its matrix, delays and polarity, followed while the
    /// stream runs. After [`LeEncoder::with_channel_mask`].
    pub fn with_mixer(mut self, handle: crate::mix::MixHandle) -> LeEncoder {
        let rear_out = matches!(self.mix, Mix::Speakers(_));
        let channels = usize::from(self.input.channels);
        tracing::info!(
            "LE Audio: broadcast mixer: {channels} ch (mask 0x{:x}), {} outputs",
            self.mask,
            if rear_out { 4 } else { 2 }
        );
        self.mixer = Some(crate::mix::Mixer::new(handle, channels, self.mask, rear_out, self.input.sample_rate));
        self
    }

    /// The playback EQ of `handle` applied to the audio, while it is on
    pub fn with_eq(mut self, handle: crate::eq::EqHandle) -> LeEncoder {
        let lanes = self.mix.lanes();
        self.eq = Some(crate::eq::EqProcessor::new(handle, self.input.sample_rate, lanes));
        self.eq_buf = vec![0.0; lanes * EQ_BLOCK];
        self
    }

    pub fn layout(&self) -> &SduLayout {
        &self.frame.layout
    }

    /// Turns every channel to the mono mix (true) or back to the plan's
    /// channels; [`LePipeline::set_mono`] for a running pipeline
    pub fn set_mono(&self, on: bool) {
        self.frame.all_mono.store(on, Ordering::Relaxed);
    }

    /// SDU sets encoded so far
    pub fn sets_encoded(&self) -> u64 {
        self.frame.sets
    }

    /// Captured audio (whole frames of the input format): every LC3 frame
    /// it completes goes to `sink` as one SDU set
    pub fn push(&mut self, raw: &[u8], sink: &dyn SduSink) {
        let in_ch = usize::from(self.input.channels);
        let t = self.input.sample;
        let used = in_ch.min(MAX_INPUT);
        let mut ch = [0.0f32; MAX_INPUT];
        let frames = raw.len() / (in_ch * t.bytes());
        let eq_on = self.eq.as_mut().is_some_and(crate::eq::EqProcessor::active);
        if eq_on != self.eq_was_on {
            self.eq_was_on = eq_on;
            tracing::debug!("LE encode thread: EQ {}", if eq_on { "on" } else { "off" });
        }
        if let Some(m) = self.mixer.as_mut() {
            m.refresh();
        }
        let Self { mix, mixer, resampler, frame, eq, eq_buf, .. } = self;
        let lanes = mix.lanes();
        match eq.as_mut() {
            Some(eq) if eq_on => {
                // In blocks: mixed to the lanes, through the EQ, then
                // resampled
                let mut f0 = 0;
                while f0 < frames {
                    let n = (frames - f0).min(EQ_BLOCK);
                    for (i, out) in eq_buf[..lanes * n].chunks_exact_mut(lanes).enumerate() {
                        for (c, v) in ch[..used].iter_mut().enumerate() {
                            *v = input_sample(raw, (f0 + i) * in_ch + c, t);
                        }
                        let s = match mixer {
                            Some(m) => m.apply(&ch[..used]),
                            None => mix.apply(&ch[..used]),
                        };
                        out.copy_from_slice(&s[..lanes]);
                    }
                    eq.process(&mut eq_buf[..lanes * n]);
                    for s in eq_buf[..lanes * n].chunks_exact(lanes) {
                        let mut all = [0.0; LANES];
                        all[..lanes].copy_from_slice(s);
                        if let Some(m) = mixer.as_mut() {
                            m.post(&mut all);
                        }
                        resampler.process_lanes(&all, &mut |s| frame.add(s, sink));
                    }
                    f0 += n;
                }
            }
            _ => {
                for f in 0..frames {
                    for (c, v) in ch[..used].iter_mut().enumerate() {
                        *v = input_sample(raw, f * in_ch + c, t);
                    }
                    let s = match mixer.as_mut() {
                        Some(m) => {
                            let mut s = m.apply(&ch[..used]);
                            m.post(&mut s);
                            s
                        }
                        None => mix.apply(&ch[..used]),
                    };
                    resampler.process_lanes(&s, &mut |s| frame.add(s, sink));
                }
            }
        }
    }

    /// Drops the partly filled frame and the resampler's history (buffered
    /// audio of an old stream)
    pub fn reset(&mut self) {
        self.frame.fill = 0;
        self.frame.rates.iter_mut().flatten().for_each(Decimator::reset);
        self.resampler.reset();
        if let Some(m) = self.mixer.as_mut() {
            m.reset();
        }
    }
}

/// Setup of the LE encode thread
pub struct LePipelineConfig {
    /// The stream (preset and CIS / channel layout)
    pub plan: StreamPlan,
    pub input: InputFormat,
    /// The speakers of the input channels (dwChannelMask; 0 = the usual
    /// ones of the channel count)
    pub channel_mask: u32,
    /// The broadcast mixer (Auracast); None: the fixed downmix / speakers
    pub mix: Option<crate::mix::MixHandle>,
    pub buffer: BufferPreset,
    /// The playback EQ, applied before encoding while it is on; None: none
    pub media_eq: Option<crate::eq::EqHandle>,
}

/// A running LE encode thread
pub struct LePipeline {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
    layout: SduLayout,
    all_mono: Arc<AtomicBool>,
}

impl LePipeline {
    /// Starts the LE encode thread; feed it through the returned
    /// [`CaptureInput`] as the A2DP pipeline
    pub fn start(
        config: LePipelineConfig,
        sink: impl SduSink,
        stats: Arc<LinkStats>,
    ) -> Result<(LePipeline, CaptureInput), String> {
        let LePipelineConfig { plan, input, channel_mask, mix, buffer, media_eq } = config;
        let mut encoder = LeEncoder::new(&plan, input)?.with_channel_mask(channel_mask);
        if let Some(h) = mix {
            encoder = encoder.with_mixer(h);
        }
        if let Some(h) = media_eq {
            encoder = encoder.with_eq(h);
        }
        let layout = encoder.layout().clone();
        let all_mono = encoder.frame.all_mono.clone();
        let ring = buffer.ring_bytes(&input);
        tracing::debug!(
            "LE pipeline: start, buffer {buffer:?}: capture ring {ring} bytes ({} ms)",
            (ring / input.frame_bytes().max(1)) as u64 * 1000 / u64::from(input.sample_rate.max(1))
        );
        let (producer, consumer) = rtrb::RingBuffer::new(ring);
        let control = Control::new();
        let thread = {
            let control = control.clone();
            let stats = stats.clone();
            std::thread::Builder::new()
                .name("le-encode".into())
                .spawn(move || le_encode_thread(encoder, consumer, sink, &stats, &control))
                .map_err(|e| format!("spawning the LE encode thread: {e}"))?
        };
        let capture = CaptureInput {
            producer,
            encode_thread: thread.thread().clone(),
            stats,
            control: control.clone(),
            frame_bytes: input.frame_bytes(),
        };
        Ok((LePipeline { control, thread: Some(thread), layout, all_mono }, capture))
    }

    pub fn layout(&self) -> &SduLayout {
        &self.layout
    }

    /// Every channel gets the mono mix (true: the other earbud of a set went
    /// away) or the plan's channels again (false), from the next frame on
    pub fn set_mono(&self, on: bool) {
        if self.all_mono.swap(on, Ordering::Relaxed) != on {
            tracing::info!("LE pipeline: {}", if on { "mono on every channel" } else { "the planned channels again" });
        }
    }

    /// Drops audio buffered for the old stream (after the stream was set up
    /// again)
    pub fn reset(&self) {
        tracing::debug!("LE pipeline: reset requested");
        self.control.reset.store(true, Ordering::Release);
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    /// Stops and joins the encode thread
    pub fn stop(mut self) {
        self.stop_thread();
    }

    fn stop_thread(&mut self) {
        if let Some(t) = self.thread.take() {
            self.control.stop.store(true, Ordering::Release);
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl Drop for LePipeline {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

fn le_encode_thread(
    mut encoder: LeEncoder,
    mut ring: rtrb::Consumer<u8>,
    sink: impl SduSink,
    stats: &LinkStats,
    control: &Control,
) {
    let _priority = crate::thread_priority();
    let frame_bytes = encoder.input.frame_bytes();
    // ~10 ms rounds, at least 128 frames (as the A2DP encode thread)
    let round_frames = (encoder.input.sample_rate as usize / 100).max(128);
    let mut read_buf = vec![0u8; round_frames * frame_bytes];
    let mut overflow_report = Instant::now();
    tracing::debug!(
        "LE encode thread: running, rounds of {round_frames} frames, priority hook {}",
        _priority.is_some()
    );
    // Debug summary window: start, rounds, frames, rounds behind, sets
    // encoded and errors at its start
    let mut window = (Instant::now(), 0u64, 0u64, 0u64, encoder.frame.sets, encoder.frame.errors);
    while !control.stop.load(Ordering::Acquire) {
        if control.reset.swap(false, Ordering::AcqRel) {
            tracing::debug!("LE encode thread: reset, dropping {} buffered bytes", ring.slots());
            let n = ring.slots();
            if let Ok(chunk) = ring.read_chunk(n) {
                chunk.commit_all();
            }
            encoder.reset();
        }
        control.report_overflows("LE encode thread", &mut overflow_report);
        if window.0.elapsed() >= crate::SUMMARY_INTERVAL {
            tracing::debug!(
                "LE encode thread: last {:.1} s: {} rounds, {} frames, {} behind rounds, {} SDU sets, {} encode \
                 errors; {} sets in total",
                window.0.elapsed().as_secs_f64(),
                window.1,
                window.2,
                window.3,
                encoder.frame.sets - window.4,
                encoder.frame.errors - window.5,
                encoder.frame.sets
            );
            window = (Instant::now(), 0, 0, 0, encoder.frame.sets, encoder.frame.errors);
        }
        let available = ring.slots() / frame_bytes;
        if available == 0 {
            std::thread::park_timeout(Duration::from_millis(10));
            continue;
        }
        window.1 += 1;
        window.2 += available.min(round_frames) as u64;
        window.3 += u64::from(available > round_frames);
        let bytes = available.min(round_frames) * frame_bytes;
        if let Ok(chunk) = ring.read_chunk(bytes) {
            let (a, b) = chunk.as_slices();
            read_buf[..a.len()].copy_from_slice(a);
            read_buf[a.len()..bytes].copy_from_slice(b);
            chunk.commit_all();
        }
        encoder.push(&read_buf[..bytes], &sink);
        encoder.frame.clock.flush(stats);
    }
    tracing::debug!(
        "LE encode thread: stopped after {} SDU sets, {} encode errors",
        encoder.frame.sets,
        encoder.frame.errors
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use le_audio::defs::{LOCATION_FRONT_LEFT, LOCATION_FRONT_RIGHT};
    use le_audio::plan::{ChannelPlan, CisPlan};
    use le_audio::presets::StreamPreset;

    pub(crate) fn preset(rate: u32, frame_us: u16, octets: u16) -> StreamPreset {
        StreamPreset {
            name: "test".into(),
            sampling_hz: rate,
            frame_us,
            octets,
            target_latency: 2,
            framing: 0,
            rtn: 2,
            max_transport_latency_ms: 10,
            presentation_delay_us: 40000,
        }
    }

    fn cis(id: u8, channels: &[(u32, ChannelSource)]) -> CisPlan {
        CisPlan {
            device_id: u32::from(id),
            ase_id: id,
            cis_id: id,
            acl: 0x40 + u16::from(id),
            cis_handle: 0x60 + u16::from(id),
            channels: channels.iter().map(|&(allocation, source)| ChannelPlan { allocation, source }).collect(),
            source_ase: None,
            preset: None,
        }
    }

    fn plan(preset: StreamPreset, cis: Vec<CisPlan>) -> StreamPlan {
        StreamPlan {
            preset,
            cig_id: 1,
            phy: 2,
            requested_contexts: 4,
            contexts: 4,
            presentation_delay_us: 40000,
            cis,
            source: None,
            source_note: String::new(),
        }
    }

    const L: (u32, ChannelSource) = (LOCATION_FRONT_LEFT, ChannelSource::Left);
    const R: (u32, ChannelSource) = (LOCATION_FRONT_RIGHT, ChannelSource::Right);
    const MONO: (u32, ChannelSource) = (0, ChannelSource::Mono);

    fn f32_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn downmix_mono_stereo_and_surround() {
        assert_eq!(downmix(&[0.25]), (0.25, 0.25));
        assert_eq!(downmix(&[0.25, -0.5]), (0.25, -0.5));
        // 5.1: FL FR FC LFE BL BR
        let (l, r) = downmix(&[0.1, 0.2, 0.3, 0.4, 0.05, 0.06]);
        assert!((l - (0.1 + MINUS_3DB * 0.3 + MINUS_3DB * 0.05 + 0.5 * 0.4)).abs() < 1e-6);
        assert!((r - (0.2 + MINUS_3DB * 0.3 + MINUS_3DB * 0.06 + 0.5 * 0.4)).abs() < 1e-6);
        // 7.1 adds SL / SR to the back channels
        let (l, r) = downmix(&[0.0, 0.0, 0.0, 0.0, 0.1, 0.2, 0.3, 0.4]);
        assert!((l - MINUS_3DB * 0.4).abs() < 1e-6);
        assert!((r - MINUS_3DB * 0.6).abs() < 1e-6);
        // 3.0: FC only
        let (l, r) = downmix(&[0.0, 0.0, 0.5]);
        assert!((l - 0.3535534).abs() < 1e-6 && (r - 0.3535534).abs() < 1e-6);
    }

    #[test]
    fn resample_passthrough_and_linear() {
        let mut rs = LinearResampler::new(48000, 48000);
        let mut out = Vec::new();
        rs.process(0.5, -0.5, &mut |l, r| out.push((l, r)));
        assert_eq!(out, vec![(0.5, -0.5)]);

        // 2:1 up: two outputs per input, between the previous input and
        // this one (positions 0 and 0.5)
        let mut rs = LinearResampler::new(24000, 48000);
        out.clear();
        for x in [1.0, 2.0, 3.0] {
            rs.process(x, -x, &mut |l, r| out.push((l, r)));
        }
        assert_eq!(out, vec![(0.0, 0.0), (0.5, -0.5), (1.0, -1.0), (1.5, -1.5), (2.0, -2.0), (2.5, -2.5)]);

        // 2:1 down: every other input
        let mut rs = LinearResampler::new(96000, 48000);
        out.clear();
        for x in [1.0, 2.0, 3.0, 4.0, 5.0] {
            rs.process(x, x, &mut |l, _| out.push((l, 0.0)));
        }
        assert_eq!(out.iter().map(|o| o.0).collect::<Vec<_>>(), vec![0.0, 2.0, 4.0]);
    }

    #[test]
    fn resample_44100_to_48000_keeps_the_rate_and_the_tone() {
        let mut rs = LinearResampler::new(44100, 48000);
        let mut out = Vec::new();
        for n in 0..44100 {
            let x = (std::f64::consts::TAU * 1000.0 * n as f64 / 44100.0).sin() as f32 * 0.5;
            rs.process(x, x, &mut |l, _| out.push(l));
        }
        assert!((47999..=48001).contains(&out.len()), "{} samples", out.len());
        // The output is the tone at 48 kHz, one input sample late
        let delay = 48000.0 / 44100.0;
        let mut err = 0.0f64;
        for (n, &y) in out.iter().enumerate().skip(10) {
            let t = (n as f64 - delay) / 48000.0;
            let want = (std::f64::consts::TAU * 1000.0 * t).sin() * 0.5;
            err = err.max((y as f64 - want).abs());
        }
        assert!(err < 0.01, "max error {err}");
    }

    #[test]
    fn layout_orders_channels_by_location_per_cis() {
        // One 2-channel CIS listed right-first, then one mono CIS, then two
        // 1-channel CIS
        let p = plan(preset(48000, 10000, 100), vec![cis(1, &[R, L]), cis(2, &[MONO]), cis(3, &[R]), cis(4, &[L])]);
        let layout = SduLayout::from_plan(&p).unwrap();
        assert_eq!(layout.sdu_lens, vec![200, 100, 100, 100]);
        assert_eq!(layout.frame_samples, 480);
        let slots: Vec<_> = layout.channels.iter().map(|c| (c.cis, c.offset, c.allocation, c.source)).collect();
        assert_eq!(
            slots,
            vec![
                (0, 0, LOCATION_FRONT_LEFT, ChannelSource::Left),
                (0, 100, LOCATION_FRONT_RIGHT, ChannelSource::Right),
                (1, 0, 0, ChannelSource::Mono),
                (2, 0, LOCATION_FRONT_RIGHT, ChannelSource::Right),
                (3, 0, LOCATION_FRONT_LEFT, ChannelSource::Left),
            ]
        );
        assert!(SduLayout::from_plan(&plan(preset(44100, 10000, 100), vec![cis(1, &[L])])).is_err());
        assert!(SduLayout::from_plan(&plan(preset(48000, 10000, 100), vec![])).is_err());
        assert!(SduLayout::from_plan(&plan(preset(48000, 10000, 100), vec![cis(1, &[])])).is_err());
    }

    /// Collects the pushed sets
    #[derive(Clone, Default)]
    struct Collect(Arc<Mutex<Vec<SduSet>>>);

    impl SduSink for Collect {
        fn push(&self, set: SduSet) {
            self.0.lock().unwrap().push(set);
        }
    }

    /// Mono on: the left and the right CIS carry the mono mix (as a mono
    /// channel's own encoder gives it), off again: their own channels
    #[test]
    fn mono_switch_gives_every_channel_the_mix() {
        let rate = 48000;
        let p = plan(preset(rate, 10000, 100), vec![cis(1, &[L]), cis(2, &[R])]);
        let input = InputFormat { sample_rate: rate, channels: 2, sample: SampleType::F32 };
        let encode = |mono: bool| {
            let mut enc = LeEncoder::new(&p, input).unwrap();
            enc.set_mono(mono);
            let sink = Collect::default();
            let pcm: Vec<f32> = (0..480).flat_map(|n| [(n as f32 * 0.13).sin() * 0.5, 0.0]).collect();
            enc.push(&f32_bytes(&pcm), &sink);
            let sets = sink.0.lock().unwrap().clone();
            assert_eq!(sets.len(), 1);
            sets[0].clone()
        };
        let mono = encode(true);
        assert_eq!(mono[0], mono[1], "both earbuds get the same mix");
        let mut reference =
            Lc3ChannelEncoder::new(Lc3Params { sample_rate: rate, frame_us: 10000, octets: 100 }).unwrap();
        let m: Vec<i16> = (0..480).map(|n| to_i16(0.5 * (n as f32 * 0.13).sin() * 0.5)).collect();
        let mut fm = [0u8; 100];
        reference.encode(&m, &mut fm).unwrap();
        assert_eq!(mono[0], fm.to_vec());
        let stereo = encode(false);
        assert_ne!(stereo[0], stereo[1], "a tone on the left, silence on the right");
    }

    /// Each CIS's SDU carries its channels' frames in location order: what
    /// encoding the left / right / mono audio by hand gives
    /// The lanes of one input frame through the matrix of `mask`
    fn lanes(channels: usize, mask: u32, frame: &[f32]) -> [f32; LANES] {
        Mix::Speakers(speaker_matrix(channels, mask).0).apply(frame)
    }

    #[test]
    fn a_rear_at_half_the_rate_is_decimated_in_step_with_the_front() {
        let rl = (0, ChannelSource::RearLeft);
        let mut rear = cis(3, &[rl]);
        rear.preset = Some(preset(24000, 10000, 60));
        let p = plan(preset(48000, 10000, 120), vec![cis(1, &[L]), cis(2, &[R]), rear]);
        let layout = SduLayout::from_plan(&p).unwrap();
        assert_eq!((layout.params.sample_rate, layout.frame_samples), (48000, 480));
        assert_eq!(layout.sdu_lens, [120, 120, 60]);
        let decim: Vec<_> = layout.channels.iter().map(|s| (s.params.sample_rate, s.decimate)).collect();
        assert_eq!(decim, [(48000, 1), (48000, 1), (24000, 2)]);
        assert_eq!(silence_set(&p).iter().map(Vec::len).collect::<Vec<_>>(), [120, 120, 60]);
        // Another frame duration cannot share the stream
        let mut odd = p.clone();
        odd.cis[2].preset = Some(preset(24000, 7500, 45));
        assert!(SduLayout::from_plan(&odd).is_err());

        // The encoder: sets of 120 / 120 / 60 octets
        let input = InputFormat { sample_rate: 48000, channels: 2, sample: SampleType::F32 };
        let mut enc = LeEncoder::new(&p, input).unwrap();
        let sink = Collect::default();
        let tone: Vec<f32> = (0..4800).flat_map(|i| [(i as f32 * 0.13).sin() * 0.3, 0.0]).collect();
        enc.push(&f32_bytes(&tone), &sink);
        let sets = sink.0.lock().unwrap();
        assert_eq!(sets.len(), 10);
        assert!(sets.iter().all(|s| s.iter().map(Vec::len).eq([120, 120, 60])));
    }

    #[test]
    fn the_decimator_passes_the_band_and_stops_above_it_with_the_delay_of_the_others() {
        let ns = 480;
        let level = |hz: f64, factor: usize| {
            let mut d = Decimator::new(factor, ns);
            let mut out = vec![0i16; ns / factor];
            let mut peak = 0i16;
            for f in 0..20 {
                let frame: Vec<f32> = (0..ns)
                    .map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * (f * ns + i) as f64 / 48000.0).sin()) as f32)
                    .collect();
                d.process(&frame, &mut out);
                if f >= 2 {
                    peak = peak.max(out.iter().copied().map(i16::abs).max().unwrap());
                }
            }
            f64::from(peak) / (0.5 * 32767.0)
        };
        assert!((level(1000.0, 2) - 1.0).abs() < 0.02, "1 kHz passes");
        assert!((level(9000.0, 2) - 1.0).abs() < 0.05, "9 kHz passes");
        assert!(level(18000.0, 2) < 0.01, "18 kHz (would alias) is stopped");
        // An impulse: both paths delay it by the same 31 samples at 48 kHz
        let mut imp = vec![0.0f32; ns];
        imp[100] = 1.0;
        let mut full = Decimator::new(1, ns);
        let mut out = vec![0i16; ns];
        full.process(&imp, &mut out);
        assert_eq!(out.iter().position(|&s| s != 0), Some(100 + Decimator::DELAY));
        let mut half = Decimator::new(2, ns);
        let mut out = vec![0i16; ns / 2];
        half.process(&imp, &mut out);
        let peak = out.iter().enumerate().max_by_key(|(_, s)| s.abs()).unwrap().0;
        // Output m holds input 2m+1 delayed: the peak lands where 131 does
        assert!((2 * peak + 1).abs_diff(100 + Decimator::DELAY) <= 1, "peak at {peak}");
    }

    #[test]
    fn speakers_by_the_channel_mask() {
        let h = MINUS_3DB;
        // 5.1 (back): FL FR FC LFE BL BR
        let f = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let l = lanes(6, 0x3F, &f);
        assert!((l[0] - (0.1 + h * 0.3 + 0.5 * 0.4)).abs() < 1e-6);
        assert!((l[1] - (0.2 + h * 0.3 + 0.5 * 0.4)).abs() < 1e-6);
        assert_eq!((l[2], l[3]), (0.5, 0.6));
        // 5.1 (side): the same places are SL / SR
        assert_eq!(&lanes(6, 0x60F, &f)[2..], &[0.5, 0.6]);
        // 7.1: back and side both make the rear
        let f = [0.0, 0.0, 0.0, 0.0, 0.1, 0.2, 0.3, 0.4];
        let l = lanes(8, 0x63F, &f);
        assert!((l[2] - h * (0.1 + 0.3)).abs() < 1e-6 && (l[3] - h * (0.2 + 0.4)).abs() < 1e-6);
        assert_eq!((l[0], l[1]), (0.0, 0.0));
        // Quad
        assert_eq!(lanes(4, 0x33, &[0.1, 0.2, 0.3, 0.4]), [0.1, 0.2, 0.3, 0.4]);
        // Stereo: no rear, the rear plays the front
        assert_eq!(lanes(2, 0x3, &[0.1, 0.2]), [0.1, 0.2, 0.1, 0.2]);
        assert!(!speaker_matrix(2, 0x3).1 && speaker_matrix(6, 0x3F).1);
        // Mono: everywhere
        assert_eq!(lanes(1, 0x4, &[0.5]), [0.5; 4]);
        // No mask, or one that does not match: the usual one of the count
        assert_eq!(lanes(6, 0, &[0.0, 0.0, 0.0, 0.0, 0.5, 0.6])[2..], [0.5, 0.6]);
        assert_eq!(lanes(4, 0x3, &[0.1, 0.2, 0.3, 0.4]), [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn rear_channels_take_the_speakers_and_the_stereo_plan_the_downmix() {
        let rate = 24000;
        let rl = (0, ChannelSource::RearLeft);
        let rr = (0, ChannelSource::RearRight);
        let quad = plan(preset(rate, 10000, 60), vec![cis(1, &[L]), cis(1, &[R]), cis(1, &[rl]), cis(1, &[rr])]);
        let input = InputFormat { sample_rate: rate, channels: 6, sample: SampleType::F32 };
        let enc = LeEncoder::new(&quad, input).unwrap().with_channel_mask(0x60F);
        assert_eq!(enc.mix, Mix::Speakers(speaker_matrix(6, 0x60F).0));
        assert_eq!(enc.mix.lanes(), 4);
        let stereo = plan(preset(rate, 10000, 60), vec![cis(1, &[L, R])]);
        let enc = LeEncoder::new(&stereo, input).unwrap().with_channel_mask(0x60F);
        assert_eq!(enc.mix, Mix::Stereo);

        // Each rear SDU carries its own speaker: a tone on side left only
        let mut enc = LeEncoder::new(&quad, input).unwrap().with_channel_mask(0x60F);
        let sink = Collect::default();
        let tone: Vec<f32> = (0..2 * 240).map(|n| (n as f32 * 0.13).sin() * 0.5).collect();
        let pcm: Vec<f32> = tone.iter().flat_map(|&x| [0.0, 0.0, 0.0, 0.0, x, 0.0]).collect();
        enc.push(&f32_bytes(&pcm), &sink);
        let sets = sink.0.lock().unwrap();
        assert_eq!(sets.len(), 2);
        let params = Lc3Params { sample_rate: rate, frame_us: 10000, octets: 60 };
        let mut silent = Lc3ChannelEncoder::new(params).unwrap();
        let mut left = Lc3ChannelEncoder::new(params).unwrap();
        let mut quiet = [0u8; 60];
        silent.encode(&[0; 240], &mut quiet).unwrap();
        let mut toned = [0u8; 60];
        let l: Vec<i16> = tone[..240].iter().map(|&x| to_i16(x)).collect();
        left.encode(&l, &mut toned).unwrap();
        assert_eq!(sets[0].len(), 4);
        assert_eq!(&sets[0][2][..], &toned, "rear left");
        assert_eq!(&sets[0][3][..], &quiet, "rear right");
        assert_eq!(&sets[0][0][..], &quiet, "front left");
    }

    #[test]
    fn sdus_carry_the_channels_in_location_order() {
        let rate = 48000;
        let p = plan(preset(rate, 10000, 100), vec![cis(1, &[R, L]), cis(2, &[MONO])]);
        let input = InputFormat { sample_rate: rate, channels: 2, sample: SampleType::F32 };
        let mut enc = LeEncoder::new(&p, input).unwrap();
        let sink = Collect::default();
        // Left: a tone, right: silence, for 3 frames and a bit
        let frames = 3 * 480 + 100;
        let tone: Vec<f32> = (0..frames).map(|n| (n as f32 * 0.13).sin() * 0.5).collect();
        let pcm: Vec<f32> = tone.iter().flat_map(|&l| [l, 0.0]).collect();
        // In uneven pieces, as capture delivers them
        let bytes = f32_bytes(&pcm);
        for piece in bytes.chunks(8 * 333) {
            enc.push(piece, &sink);
        }
        assert_eq!(enc.sets_encoded(), 3);
        let sets = sink.0.lock().unwrap();
        assert_eq!(sets.len(), 3);

        // Reference: the same frames encoded per channel directly
        let params = Lc3Params { sample_rate: rate, frame_us: 10000, octets: 100 };
        let mut left = Lc3ChannelEncoder::new(params).unwrap();
        let mut right = Lc3ChannelEncoder::new(params).unwrap();
        let mut mono = Lc3ChannelEncoder::new(params).unwrap();
        for (f, set) in sets.iter().enumerate() {
            let frame = &tone[f * 480..(f + 1) * 480];
            let l: Vec<i16> = frame.iter().map(|&x| to_i16(x)).collect();
            let m: Vec<i16> = frame.iter().map(|&x| to_i16(0.5 * x)).collect();
            let (mut fl, mut fr, mut fm) = ([0u8; 100], [0u8; 100], [0u8; 100]);
            left.encode(&l, &mut fl).unwrap();
            right.encode(&[0; 480], &mut fr).unwrap();
            mono.encode(&m, &mut fm).unwrap();
            assert_eq!(set.len(), 2);
            assert_eq!(&set[0][..100], &fl, "frame {f}: front left first");
            assert_eq!(&set[0][100..], &fr, "frame {f}: front right second");
            assert_eq!(&set[1][..], &fm, "frame {f}: mono CIS");
            assert_ne!(fl, fr);
        }
    }

    #[test]
    fn integer_input_and_reset() {
        let p = plan(preset(16000, 7500, 30), vec![cis(1, &[MONO])]);
        let input = InputFormat { sample_rate: 16000, channels: 1, sample: SampleType::I16 };
        let mut enc = LeEncoder::new(&p, input).unwrap();
        let sink = Collect::default();
        assert_eq!(enc.layout().frame_samples, 120);
        enc.push(&[0u8; 2 * 100], &sink);
        enc.reset();
        enc.push(&[0u8; 2 * 100], &sink);
        assert_eq!(enc.sets_encoded(), 0, "the reset dropped the first 100 samples");
        enc.push(&[0u8; 2 * 20], &sink);
        assert_eq!(enc.sets_encoded(), 1);
        assert_eq!(sink.0.lock().unwrap()[0], silence_set(&p));
        assert!(LeEncoder::new(&p, InputFormat { channels: 0, ..input }).is_err());
    }

    #[test]
    fn i16_and_i32_scale_like_float() {
        assert_eq!(input_sample(&16384i16.to_le_bytes(), 0, SampleType::I16), 0.5);
        assert_eq!(input_sample(&(-0x4000_0000i32).to_le_bytes(), 0, SampleType::I32), -0.5);
        assert_eq!(input_sample(&0.25f32.to_le_bytes(), 0, SampleType::F32), 0.25);
        assert_eq!(to_i16(2.0), 32767);
        assert_eq!(to_i16(-2.0), -32767);
        assert_eq!(to_i16(0.5), 16384); // 16383.5: ties to even
        assert_eq!(to_i16(1.5 / 32767.0), 2); // 1.5: ties to even
    }

    #[test]
    fn spare_sets_are_used() {
        let p = plan(preset(48000, 10000, 100), vec![cis(1, &[L]), cis(2, &[R])]);
        let input = InputFormat { sample_rate: 48000, channels: 2, sample: SampleType::F32 };
        let mut enc = LeEncoder::new(&p, input).unwrap();
        let queue = Arc::new(Mutex::new(SduQueue::new()));
        queue.lock().unwrap().configure(2, silence_set(&p));
        let spares_before = std::iter::from_fn(|| queue.lock().unwrap().spare()).count();
        queue.lock().unwrap().configure(2, silence_set(&p));
        enc.push(&f32_bytes(&vec![0.1; 2 * 480 * 2]), &queue);
        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.stats().sets_pushed, 2);
        drop(q);
        let spares_after = std::iter::from_fn(|| queue.lock().unwrap().spare()).count();
        assert_eq!(spares_after, spares_before - 2);
    }

    #[test]
    fn silence_set_has_one_sdu_per_cis() {
        let p = plan(preset(48000, 7500, 75), vec![cis(1, &[L, R]), cis(2, &[MONO])]);
        let s = silence_set(&p);
        assert_eq!(s.iter().map(Vec::len).collect::<Vec<_>>(), vec![150, 75]);
        let mut frame = [0u8; 75];
        codec::lc3::silence_frame(Lc3Params { sample_rate: 48000, frame_us: 7500, octets: 75 }, &mut frame).unwrap();
        assert_eq!(&s[0][..75], &frame);
        assert_eq!(&s[0][75..], &frame);
        assert_eq!(&s[1][..], &frame);
        // An unusable plan still gives one (zero) SDU per CIS
        let bad = plan(preset(44100, 10000, 100), vec![cis(1, &[L, R])]);
        assert_eq!(silence_set(&bad), vec![vec![0u8; 200]]);
    }

    #[test]
    fn a_source_only_cis_gets_no_sdu() {
        // A conversational plan: CIS 2 carries a microphone only
        let mic = cis(2, &[]);
        let p = plan(preset(48000, 10000, 100), vec![cis(1, &[L, R]), mic, cis(3, &[MONO])]);
        let layout = SduLayout::from_plan(&p).unwrap();
        assert_eq!(layout.sdu_lens, [200, 100]);
        assert_eq!(layout.channels.iter().map(|c| c.cis).collect::<Vec<_>>(), [0, 0, 1]);
        assert_eq!(silence_set(&p).len(), 2);
        // Microphones only: nothing to send
        let p = plan(preset(48000, 10000, 100), vec![cis(2, &[])]);
        assert!(SduLayout::from_plan(&p).is_err());
    }

    /// The thread: captured audio in, SDU sets out, stop joins
    #[test]
    fn encode_thread_runs() {
        let p = plan(preset(48000, 7500, 75), vec![cis(1, &[L, R])]);
        let input = InputFormat { sample_rate: 48000, channels: 2, sample: SampleType::I16 };
        let queue = Arc::new(Mutex::new(SduQueue::new()));
        queue.lock().unwrap().configure(2, silence_set(&p));
        let stats = Arc::new(LinkStats::default());
        let config = LePipelineConfig {
            plan: p,
            input,
            channel_mask: 0,
            mix: None,
            buffer: BufferPreset::Standard,
            media_eq: None,
        };
        let (pipeline, mut capture) = LePipeline::start(config, queue.clone(), stats.clone()).unwrap();
        assert_eq!(pipeline.layout().sdu_lens, vec![150]);
        // 4 frames of 7.5 ms
        capture.push(&vec![0u8; 4 * 360 * 4]);
        let start = std::time::Instant::now();
        while queue.lock().unwrap().len() < 4 && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        pipeline.stop();
        assert_eq!(queue.lock().unwrap().len(), 4);
        assert_eq!(stats.capture_frames.load(Ordering::Relaxed), 4 * 360);
        // Each set timed against its 7.5 ms of audio
        let enc = stats.encode.totals();
        assert_eq!((enc.calls, enc.audio_ns), (4, 4 * 7_500_000));
        assert!(enc.busy_ns > 0);
        let p = plan(preset(44100, 7500, 75), vec![cis(1, &[L])]);
        let config = LePipelineConfig {
            plan: p,
            input,
            channel_mask: 0,
            mix: None,
            buffer: BufferPreset::Standard,
            media_eq: None,
        };
        assert!(LePipeline::start(config, queue, stats).is_err());
    }

    /// The EQ off or flat: the SDUs are bit for bit those without an EQ;
    /// on, they differ
    #[test]
    fn eq_off_is_bit_identical_and_on_changes_the_sdus() {
        let p = plan(preset(48000, 10000, 100), vec![cis(1, &[L, R])]);
        let input = InputFormat { sample_rate: 44100, channels: 2, sample: SampleType::I16 };
        let audio: Vec<u8> =
            (0..4410 * 2).flat_map(|n| (((n as f32 * 0.07).sin() * 30000.0) as i16).to_le_bytes()).collect();
        let run = |eq: Option<crate::eq::EqHandle>| {
            let mut enc = LeEncoder::new(&p, input).unwrap();
            if let Some(h) = eq {
                enc = enc.with_eq(h);
            }
            let sink = Collect::default();
            for piece in audio.chunks(4 * 441) {
                enc.push(piece, &sink);
            }
            sink.0.lock().unwrap().clone()
        };
        let plain = run(None);
        assert_eq!(plain.len(), 10);
        let off = crate::eq::EqHandle::default();
        off.set(crate::eq::EqSettings::new(false, &[dsp::EqBand::peaking(500.0, 9.0, 1.0)]));
        assert_eq!(run(Some(off)), plain);
        let flat = crate::eq::EqHandle::default();
        flat.set(crate::eq::EqSettings::new(true, &[dsp::EqBand::peaking(500.0, 0.0, 1.0)]));
        assert_eq!(run(Some(flat)), plain);
        let on = crate::eq::EqHandle::default();
        on.set(crate::eq::EqSettings::new(true, &[dsp::EqBand::peaking(500.0, 9.0, 1.0)]));
        assert_ne!(run(Some(on)), plain);
    }
}
