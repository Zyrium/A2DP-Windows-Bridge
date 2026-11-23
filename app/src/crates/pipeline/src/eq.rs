//! The playback EQ: a headphone's parametric EQ applied to the captured audio
//! before it is encoded (A2DP and LE Audio).
//!
//! The control side sets an [`EqHandle`]; the encode thread's [`EqProcessor`]
//! picks the change up at the start of its next block (no lock waited for,
//! no allocation) and keeps the filters' state, so a point dragged while
//! audio plays does not click. Boosts would clip: the processor lowers the
//! level by the curve's highest boost (the preamp) and a limiter catches
//! what still goes over. Off, or with no band that changes anything, the
//! encode thread does not call it at all and the audio stays bit for bit
//! what it was without an EQ.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dsp::biquad::{Biquad, Coeffs};
use dsp::dynamics::{Limiter, db_to_lin};
use dsp::{BandType, EqBand, MAX_EQ_POINTS};

/// Lowest and highest frequency of a point (Hz)
pub const FREQ_MIN: f32 = 20.0;
pub const FREQ_MAX: f32 = 20_000.0;
/// Largest boost or cut of a point (dB)
pub const GAIN_LIMIT: f32 = 12.0;
/// Narrowest and widest Q
pub const Q_MIN: f32 = 0.1;
pub const Q_MAX: f32 = 12.0;

/// Peak level the limiter holds the EQ'd audio under (dBFS)
pub const CEILING_DB: f32 = -0.3;

/// Frames processed at a time (the scratch buffers' length)
const CHUNK: usize = 256;
/// Most channels processed (the A2DP encoders take mono or stereo, the LE
/// path up to four speakers)
const MAX_CHANNELS: usize = 4;
/// Points of the response scanned for its highest boost, log spaced
const SCAN_POINTS: usize = 192;

/// One EQ as set: on or off, and up to [`MAX_EQ_POINTS`] points. Copy, so
/// it crosses to the encode thread without an allocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    bands: [EqBand; MAX_EQ_POINTS],
    count: usize,
}

impl Default for EqSettings {
    fn default() -> Self {
        EqSettings::OFF
    }
}

impl EqSettings {
    pub const OFF: EqSettings = EqSettings {
        enabled: false,
        bands: [EqBand { frequency: 1000.0, gain: 0.0, q: 1.0, band_type: BandType::Peaking }; MAX_EQ_POINTS],
        count: 0,
    };

    /// `bands` held to the ranges above (points past [`MAX_EQ_POINTS`] and
    /// values that are not numbers are dropped)
    pub fn new(enabled: bool, bands: &[EqBand]) -> EqSettings {
        let mut s = EqSettings { enabled, ..EqSettings::OFF };
        for b in bands.iter().filter(|b| b.frequency.is_finite() && b.gain.is_finite() && b.q.is_finite()) {
            if s.count == MAX_EQ_POINTS {
                break;
            }
            s.bands[s.count] = sanitize(b);
            s.count += 1;
        }
        s
    }

    pub fn bands(&self) -> &[EqBand] {
        &self.bands[..self.count]
    }
}

fn sanitize(b: &EqBand) -> EqBand {
    let gain = match b.band_type {
        BandType::HighPass | BandType::LowPass => 0.0,
        _ => b.gain.clamp(-GAIN_LIMIT, GAIN_LIMIT),
    };
    EqBand {
        frequency: b.frequency.clamp(FREQ_MIN, FREQ_MAX),
        gain,
        q: b.q.clamp(Q_MIN, Q_MAX),
        band_type: b.band_type,
    }
}

struct Shared {
    /// Bumped by every [`EqHandle::set`]
    version: AtomicU64,
    settings: Mutex<EqSettings>,
}

/// The EQ of the stream running, shared by the control side (which sets it)
/// and the encode thread (which reads it)
#[derive(Clone)]
pub struct EqHandle(Arc<Shared>);

impl Default for EqHandle {
    fn default() -> Self {
        EqHandle(Arc::new(Shared { version: AtomicU64::new(0), settings: Mutex::new(EqSettings::OFF) }))
    }
}

impl std::fmt::Debug for EqHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EqHandle").field(&self.get()).finish()
    }
}

impl EqHandle {
    /// New settings: the encode thread takes them from its next block
    pub fn set(&self, settings: EqSettings) {
        let mut s = self.0.settings.lock().unwrap_or_else(|e| e.into_inner());
        if *s != settings {
            *s = settings;
            self.0.version.fetch_add(1, Ordering::Release);
        }
    }

    pub fn get(&self) -> EqSettings {
        *self.0.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }

    /// The settings unless the control side holds the lock right now
    fn try_get(&self) -> Option<EqSettings> {
        match self.0.settings.try_lock() {
            Ok(s) => Some(*s),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(*e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// The highest boost of `bands`' response at `sample_rate` (dB, 0 when it
/// only cuts), scanned over the audible range and at each point's own
/// frequency (a peak's top)
pub fn max_boost_db(bands: &[EqBand], sample_rate: f64) -> f64 {
    let coeffs: [Option<Coeffs>; MAX_EQ_POINTS] =
        std::array::from_fn(|i| bands.get(i).and_then(|b| Coeffs::for_band(b, sample_rate)));
    let response = |f: f64| coeffs.iter().flatten().map(|c| c.response_db(f, sample_rate)).sum::<f64>();
    let top = (f64::from(FREQ_MAX)).min(sample_rate * 0.49);
    let lo = f64::from(FREQ_MIN);
    let mut max = 0.0f64;
    for i in 0..SCAN_POINTS {
        let f = lo * (top / lo).powf(i as f64 / (SCAN_POINTS - 1) as f64);
        max = max.max(response(f));
    }
    for b in bands.iter().take(MAX_EQ_POINTS) {
        let f = f64::from(b.frequency);
        if f < sample_rate * 0.49 {
            max = max.max(response(f));
        }
    }
    max
}

/// The EQ on the encode thread: filters, preamp and limiter for one stream
/// format. Construct it before the stream runs (it allocates); after that
/// nothing on it allocates.
pub struct EqProcessor {
    handle: EqHandle,
    seen: u64,
    sample_rate: f64,
    channels: usize,
    /// Per channel, the bands that change anything, in order
    filters: [[Biquad; MAX_EQ_POINTS]; MAX_CHANNELS],
    used: usize,
    /// Gain before the filters (linear), and what the last block ended at:
    /// a change ramps over one block
    preamp: f32,
    preamp_now: f32,
    limiters: [Limiter; MAX_CHANNELS],
    scratch: [[f32; CHUNK]; MAX_CHANNELS],
    active: bool,
}

impl EqProcessor {
    /// For interleaved audio of `channels` (1 to 4) at `sample_rate`
    pub fn new(handle: EqHandle, sample_rate: u32, channels: usize) -> EqProcessor {
        let rate = sample_rate.max(1) as f32;
        let mut p = EqProcessor {
            handle,
            // Picks the settings up on the first block
            seen: u64::MAX,
            sample_rate: f64::from(sample_rate.max(1)),
            channels: channels.clamp(1, MAX_CHANNELS),
            filters: [[Biquad::default(); MAX_EQ_POINTS]; MAX_CHANNELS],
            used: 0,
            preamp: 1.0,
            preamp_now: 1.0,
            limiters: std::array::from_fn(|_| Limiter::new(rate)),
            scratch: [[0.0; CHUNK]; MAX_CHANNELS],
            active: false,
        };
        p.refresh();
        p
    }

    /// Whether the EQ changes the audio now: false while off or flat (then
    /// the caller leaves the audio as it is). Takes changed settings first.
    pub fn active(&mut self) -> bool {
        self.refresh();
        self.active
    }

    /// The preamp in force (dB), for tests and logs
    pub fn preamp_db(&self) -> f32 {
        20.0 * self.preamp.log10()
    }

    fn refresh(&mut self) {
        let version = self.handle.version();
        if version == self.seen {
            return;
        }
        // The control side is writing: the next block takes it
        let Some(s) = self.handle.try_get() else { return };
        self.seen = version;
        let was_active = self.active;
        let mut used = 0;
        for b in s.bands() {
            if let Some(c) = Coeffs::for_band(b, self.sample_rate) {
                // The same slot keeps its state when only the values change
                for ch in &mut self.filters {
                    ch[used].set(c);
                }
                used += 1;
            }
        }
        // Slots no longer used start from silence if used again
        for ch in &mut self.filters {
            for f in &mut ch[used..] {
                f.reset();
            }
        }
        self.used = used;
        self.active = s.enabled && used > 0;
        self.preamp = if self.active { db_to_lin(-(max_boost_db(s.bands(), self.sample_rate) as f32)) } else { 1.0 };
        if self.active && !was_active {
            // Turned on (again): nothing left over from the last time
            for ch in &mut self.filters {
                for f in ch.iter_mut() {
                    f.reset();
                }
            }
            self.preamp_now = self.preamp;
            for (lim, buf) in self.limiters.iter_mut().zip(&mut self.scratch) {
                buf.fill(0.0);
                lim.process(buf, CEILING_DB);
            }
        }
    }

    /// Interleaved audio of the processor's channels, in place. Call only
    /// while [`EqProcessor::active`].
    pub fn process(&mut self, buf: &mut [f32]) {
        let ch = self.channels;
        for block in buf.chunks_mut(CHUNK * ch) {
            let n = block.len() / ch;
            for (i, frame) in block.chunks_exact(ch).enumerate() {
                for (c, &s) in frame.iter().enumerate() {
                    self.scratch[c][i] = s;
                }
            }
            let (from, to) = (self.preamp_now, self.preamp);
            let step = (to - from) / n as f32;
            for c in 0..ch {
                let filters = &mut self.filters[c][..self.used];
                for (i, s) in self.scratch[c][..n].iter_mut().enumerate() {
                    let g = if step == 0.0 { to } else { from + step * (i + 1) as f32 };
                    let mut x = f64::from(*s * g);
                    for f in filters.iter_mut() {
                        x = f.process(x);
                    }
                    *s = x as f32;
                }
                self.limiters[c].process(&mut self.scratch[c][..n], CEILING_DB);
            }
            self.preamp_now = to;
            for (i, frame) in block.chunks_exact_mut(ch).enumerate() {
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = self.scratch[c][i];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone(rate: f64, freq: f64, amp: f64, frames: usize, channels: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let v = (amp * (2.0 * PI * freq * n as f64 / rate).sin()) as f32;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    fn peak(buf: &[f32], channels: usize, channel: usize, skip: usize) -> f32 {
        buf.chunks_exact(channels).skip(skip).map(|f| f[channel].abs()).fold(0.0, f32::max)
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    #[test]
    fn off_and_flat_are_inactive() {
        let h = EqHandle::default();
        let mut p = EqProcessor::new(h.clone(), 48_000, 2);
        assert!(!p.active());
        h.set(EqSettings::new(false, &[EqBand::peaking(1000.0, 6.0, 1.0)]));
        assert!(!p.active());
        // On, but nothing that changes the audio
        h.set(EqSettings::new(true, &[EqBand::peaking(1000.0, 0.0, 1.0)]));
        assert!(!p.active());
        // The UI's ten flat points (a new EQ, Reset): no filter, no preamp, no limiter
        let flat: Vec<EqBand> = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0]
            .iter()
            .map(|&f| EqBand::peaking(f, 0.0, 1.41))
            .collect();
        h.set(EqSettings::new(true, &flat));
        assert!(!p.active());
        h.set(EqSettings::new(true, &[EqBand::peaking(1000.0, 6.0, 1.0)]));
        assert!(p.active());
    }

    /// A peaking point raises a tone at its centre by its gain, at every
    /// rate a stream runs at; the preamp takes the same off again, so the
    /// tone comes out at its level and the rest of the spectrum lower
    #[test]
    fn peaking_point_raises_its_centre() {
        for rate in [44_100u32, 48_000, 96_000] {
            let r = f64::from(rate);
            let h = EqHandle::default();
            h.set(EqSettings::new(true, &[EqBand::peaking(1000.0, 6.0, 1.4)]));
            let mut p = EqProcessor::new(h, rate, 2);
            assert!(p.active());
            assert!((p.preamp_db() + 6.0).abs() < 0.05, "{rate}: preamp {}", p.preamp_db());
            let frames = rate as usize;
            let mut at = tone(r, 1000.0, 0.25, frames, 2);
            p.process(&mut at);
            let gain = db(peak(&at, 2, 0, frames / 2) / 0.25) - p.preamp_db();
            assert!((gain - 6.0).abs() < 0.1, "{rate} Hz: {gain:.2} dB at the centre");
            // Far from the point: unchanged but for the preamp
            let mut off = tone(r, 60.0, 0.25, frames, 2);
            p.process(&mut off);
            let gain = db(peak(&off, 2, 1, frames / 2) / 0.25) - p.preamp_db();
            assert!(gain.abs() < 0.3, "{rate} Hz: {gain:.2} dB away from the point");
        }
    }

    #[test]
    fn full_scale_stays_under_0_dbfs() {
        for rate in [44_100u32, 48_000, 96_000] {
            let h = EqHandle::default();
            // Boosts that add up, and a cut whose phase raises peaks
            h.set(EqSettings::new(
                true,
                &[
                    EqBand { frequency: 80.0, gain: 12.0, q: 0.7, band_type: BandType::LowShelf },
                    EqBand::peaking(100.0, 12.0, 0.5),
                    EqBand::peaking(3000.0, -9.0, 4.0),
                    EqBand { frequency: 8000.0, gain: 12.0, q: 1.0, band_type: BandType::HighShelf },
                ],
            ));
            let mut p = EqProcessor::new(h, rate, 2);
            let r = f64::from(rate);
            // Full-scale square-ish wave plus clipped noise: every frequency
            let mut buf: Vec<f32> = (0..rate as usize)
                .flat_map(|n| {
                    let sq = if (n / 37) % 2 == 0 { 1.0f32 } else { -1.0 };
                    let s = (2.0 * PI * 90.0 * n as f64 / r).sin() as f32;
                    [sq, s.signum()]
                })
                .collect();
            p.process(&mut buf);
            let max = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(max <= db_to_lin(CEILING_DB) + 1e-6, "{rate} Hz: peak {max}");
            assert!(max < 1.0);
        }
    }

    #[test]
    fn stereo_channels_are_independent() {
        let h = EqHandle::default();
        h.set(EqSettings::new(true, &[EqBand::peaking(2000.0, 9.0, 2.0)]));
        let mut p = EqProcessor::new(h, 48_000, 2);
        // Left: a tone at the point, right: silence
        let mut buf: Vec<f32> = tone(48_000.0, 2000.0, 0.2, 48_000, 1).into_iter().flat_map(|v| [v, 0.0]).collect();
        p.process(&mut buf);
        assert!(peak(&buf, 2, 0, 24_000) > 0.15);
        assert_eq!(peak(&buf, 2, 1, 0), 0.0);
        // Right alone carries the tone now: the left's filter state does not leak
        let mut buf: Vec<f32> = tone(48_000.0, 2000.0, 0.2, 48_000, 1).into_iter().flat_map(|v| [0.0, v]).collect();
        p.process(&mut buf);
        assert!(peak(&buf, 2, 0, 4_800) < 1e-3, "{}", peak(&buf, 2, 0, 4_800));
        assert!(peak(&buf, 2, 1, 24_000) > 0.15);
    }

    #[test]
    fn live_change_keeps_running_without_a_jump() {
        let h = EqHandle::default();
        h.set(EqSettings::new(true, &[EqBand::peaking(1000.0, 3.0, 1.0)]));
        let mut p = EqProcessor::new(h.clone(), 48_000, 1);
        let src = tone(48_000.0, 440.0, 0.3, 9_600, 1);
        let mut out = Vec::new();
        for (i, block) in src.chunks(480).enumerate() {
            if i == 10 {
                h.set(EqSettings::new(true, &[EqBand::peaking(1100.0, 4.0, 1.0)]));
            }
            let mut b = block.to_vec();
            assert!(p.active());
            p.process(&mut b);
            out.extend(b);
        }
        // No sample to sample step bigger than the tone itself makes
        let max_step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let tone_step = (2.0 * PI * 440.0 / 48_000.0) as f32 * 0.3 * 1.5;
        assert!(max_step < tone_step, "{max_step} vs {tone_step}");
    }

    /// The response the UI draws (src/ui/src/lib/eq.test.ts holds the same
    /// table): both sides compute the same curve
    #[test]
    fn response_table_shared_with_the_ui() {
        let bands = [
            EqBand { frequency: 60.0, gain: 4.5, q: 0.7, band_type: BandType::LowShelf },
            EqBand::peaking(250.0, -3.0, 1.4),
            EqBand::peaking(2500.0, 6.0, 0.8),
            EqBand { frequency: 9000.0, gain: -5.0, q: 1.0, band_type: BandType::HighShelf },
            EqBand { frequency: 30.0, gain: 0.0, q: 0.71, band_type: BandType::HighPass },
            EqBand { frequency: 18000.0, gain: 0.0, q: 0.9, band_type: BandType::LowPass },
        ];
        let table: [(f64, f64, f64); 9] = [
            (44_100.0, 20.0, -3.366949397),
            (44_100.0, 2500.0, 5.976168683),
            (44_100.0, 19000.0, -9.579220886),
            (48_000.0, 45.0, 2.593260591),
            (48_000.0, 250.0, -2.884547240),
            (48_000.0, 1000.0, 1.484084636),
            (48_000.0, 12000.0, -3.400823786),
            (96_000.0, 100.0, 0.204033936),
            (96_000.0, 6000.0, 1.171083516),
        ];
        for (rate, f, want) in table {
            let got = dsp::biquad::eq_response_db(&bands, f, rate);
            assert!((got - want).abs() < 1e-6, "{rate} Hz rate, {f} Hz: {got}");
        }
    }

    /// Gain (dB) from `inp` to `out` at `freq`, channel `c` of `ch`: one DFT
    /// bin over the second half of a second of audio (the filters settled;
    /// every whole multiple of 2 Hz is its own bin)
    fn gain_at(inp: &[f32], out: &[f32], ch: usize, c: usize, freq: f64, rate: f64) -> f64 {
        let frames = inp.len() / ch;
        let bin = |x: &[f32]| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, f) in x.chunks_exact(ch).enumerate().skip(frames / 2) {
                let w = 2.0 * PI * freq * n as f64 / rate;
                re += f64::from(f[c]) * w.cos();
                im -= f64::from(f[c]) * w.sin();
            }
            re.hypot(im)
        };
        20.0 * (bin(out) / bin(inp)).log10()
    }

    /// One point at ±1 dB, measured through the processor at every rate a
    /// stream runs at, mono and stereo: the touched band follows the
    /// designed bell and the rest of the spectrum stays flat (but for the
    /// preamp), up to the top of the audio band, on each channel
    #[test]
    fn one_point_leaves_the_rest_flat() {
        let freqs = [50.0, 200.0, 700.0, 1000.0, 1400.0, 5000.0, 10_000.0, 15_000.0, 19_000.0];
        for rate in [16_000u32, 44_100, 48_000, 96_000] {
            let r = f64::from(rate);
            let tones: Vec<f64> = freqs.iter().copied().filter(|&f| f < r * 0.45).collect();
            for ch in [1usize, 2] {
                for gain in [1.0f32, -1.0] {
                    let band = [EqBand::peaking(1000.0, gain, 1.41)];
                    let h = EqHandle::default();
                    h.set(EqSettings::new(true, &band));
                    let mut p = EqProcessor::new(h, rate, ch);
                    assert!(p.active());
                    let preamp = f64::from(p.preamp_db());
                    assert!((preamp + f64::from(gain.max(0.0))).abs() < 0.01, "{rate} Hz: preamp {preamp:.3}");
                    // All tones at once, in another phase on each channel
                    let inp: Vec<f32> = (0..rate as usize)
                        .flat_map(|n| {
                            let tones = &tones;
                            (0..ch).map(move |c| {
                                tones
                                    .iter()
                                    .enumerate()
                                    .map(|(k, &f)| 0.05 * (2.0 * PI * f * n as f64 / r + (k + 3 * c) as f64).sin())
                                    .sum::<f64>() as f32
                            })
                        })
                        .collect();
                    let mut out = inp.clone();
                    for block in out.chunks_mut(480 * ch) {
                        p.process(block);
                    }
                    for c in 0..ch {
                        for &f in &tones {
                            let got = gain_at(&inp, &out, ch, c, f, r);
                            let want = dsp::biquad::eq_response_db(&band, f, r) + preamp;
                            assert!(
                                (got - want).abs() < 0.05,
                                "{rate} Hz, {ch} ch, {gain:+} dB point: {got:+.3} dB at {f} Hz (designed {want:+.3}), channel {c}"
                            );
                            // Two octaves and more from the point: flat but for the preamp
                            if !(250.0..=4000.0).contains(&f) {
                                assert!((got - preamp).abs() < 0.1, "{rate} Hz: {got:+.3} dB at {f} Hz");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn settings_are_held_to_the_ranges() {
        let many = [EqBand::peaking(1e6, 99.0, 0.0); 20];
        let s = EqSettings::new(true, &many);
        assert_eq!(s.bands().len(), MAX_EQ_POINTS);
        assert_eq!(s.bands()[0], EqBand::peaking(FREQ_MAX, GAIN_LIMIT, Q_MIN));
        let s = EqSettings::new(true, &[EqBand::peaking(f32::NAN, 1.0, 1.0)]);
        assert!(s.bands().is_empty());
        let hp = EqBand { frequency: 80.0, gain: 6.0, q: 0.7, band_type: BandType::HighPass };
        assert_eq!(EqSettings::new(true, &[hp]).bands()[0].gain, 0.0);
    }

    #[test]
    fn max_boost_finds_the_peak_and_ignores_cuts() {
        let b = [EqBand::peaking(1234.0, 7.5, 6.0)];
        assert!((max_boost_db(&b, 48_000.0) - 7.5).abs() < 0.01);
        assert!(max_boost_db(&[EqBand::peaking(1000.0, -6.0, 1.0)], 48_000.0) < 1e-9);
    }
}
