//! The broadcast mixer (Auracast, developer mode), after MatrixMixer: the
//! output lanes (front left / right, rear left / right) are a matrix of the
//! input speakers, S' = M S, a column per input speaker and a row per
//! output. "Auto" computes the matrix from the input's speakers and a few
//! levels (centre, LFE, surround; optionally normalized so no row sums
//! above 1), else it is set cell by cell. Each output also has a delay (0
//! to [`MAX_DELAY_MS`], fractional: linear interpolation between samples)
//! and can be inverted.
//!
//! The control side sets [`MixSettings`] on a [`MixHandle`]; the encode
//! thread's [`Mixer`] takes them from its next block, without stopping the
//! broadcast (as the playback EQ). Nothing on the encode thread allocates
//! once the mixer is built.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::le::speaker::*;
use crate::le::{LANES, MAX_INPUT, MINUS_3DB, speakers};

/// The input columns: a speaker each
pub const INPUTS: usize = 8;
/// The speaker bits of the columns, in column order
pub const INPUT_SPEAKERS: [u32; INPUTS] =
    [FRONT_LEFT, FRONT_RIGHT, FRONT_CENTER, LOW_FREQUENCY, BACK_LEFT, BACK_RIGHT, SIDE_LEFT, SIDE_RIGHT];
/// Column names, as the UI shows them
pub const INPUT_NAMES: [&str; INPUTS] = ["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"];
/// Row names (the lanes)
pub const OUTPUT_NAMES: [&str; LANES] = ["FL", "FR", "RL", "RR"];
/// Longest delay of an output
pub const MAX_DELAY_MS: f32 = 500.0;
/// Highest gain of a cell, either sign (+12 dB)
pub const MAX_GAIN: f32 = 4.0;

/// A matrix: per output lane, the gain of each input column
pub type Matrix = [[f32; INPUTS]; LANES];

/// What the user set
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixSettings {
    /// The matrix computed from the levels below (else `matrix` as set)
    pub auto: bool,
    /// Auto: the centre into both fronts (dB)
    pub center_db: f32,
    /// Auto: the LFE into both fronts (dB)
    pub lfe_db: f32,
    /// Auto: the back and side speakers (dB): into the rear outputs, or
    /// (a stream without rear outputs) into the fronts at a further -3 dB
    pub surround_db: f32,
    /// Auto: each row scaled down so its gains sum to 1 at most (no
    /// overflow, everything a little quieter)
    pub normalize: bool,
    /// The matrix when not auto
    pub matrix: Matrix,
    /// Per output: its delay (ms)
    pub delay_ms: [f32; LANES],
    /// Per output: polarity inverted
    pub invert: [bool; LANES],
}

impl Default for MixSettings {
    fn default() -> Self {
        let mut s = MixSettings {
            auto: true,
            center_db: -3.0,
            lfe_db: -6.0,
            surround_db: 0.0,
            normalize: false,
            matrix: [[0.0; INPUTS]; LANES],
            delay_ms: [0.0; LANES],
            invert: [false; LANES],
        };
        s.matrix = auto_matrix(&[true; INPUTS], false, true, &s);
        s
    }
}

impl MixSettings {
    /// Held to the ranges: gains within ±[`MAX_GAIN`], delays within 0 to
    /// [`MAX_DELAY_MS`], levels within -60 to +12 dB, NaN as 0
    pub fn held(mut self) -> MixSettings {
        let fin = |x: f32, lo: f32, hi: f32, nan: f32| if x.is_finite() { x.clamp(lo, hi) } else { nan };
        self.center_db = fin(self.center_db, -60.0, 12.0, -3.0);
        self.lfe_db = fin(self.lfe_db, -60.0, 12.0, -6.0);
        self.surround_db = fin(self.surround_db, -60.0, 12.0, 0.0);
        for row in &mut self.matrix {
            for g in row.iter_mut() {
                *g = fin(*g, -MAX_GAIN, MAX_GAIN, 0.0);
            }
        }
        for d in &mut self.delay_ms {
            *d = fin(*d, 0.0, MAX_DELAY_MS, 0.0);
        }
        self
    }
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The column of a speaker bit (the front left / right of centre count as
/// front left / right); None for one without a column
pub fn speaker_column(bit: u32) -> Option<usize> {
    match bit {
        FRONT_LEFT_OF_CENTER => Some(0),
        FRONT_RIGHT_OF_CENTER => Some(1),
        b => INPUT_SPEAKERS.iter().position(|&s| s == b),
    }
}

/// The column of each interleaved input channel of a source of `channels`
/// with the speakers of `mask` (0: the usual ones of the count)
pub fn input_columns(channels: usize, mask: u32) -> [Option<usize>; MAX_INPUT] {
    let sp = speakers(channels, mask);
    std::array::from_fn(|i| if i < channels { speaker_column(sp[i]) } else { None })
}

/// The interleaved input channels that are the back centre speaker (6.1):
/// it has no column of its own and plays as the back left and right, at
/// -3 dB in each (as [`crate::le::speaker_matrix`] sends it to both rears)
pub fn back_center_inputs(channels: usize, mask: u32) -> [bool; MAX_INPUT] {
    let sp = speakers(channels, mask);
    std::array::from_fn(|i| i < channels && sp[i] == BACK_CENTER)
}

/// The speakers of the source that no column takes (they are not heard):
/// their mask bits
pub fn unused_speakers(channels: usize, mask: u32) -> u32 {
    let sp = speakers(channels, mask);
    sp.iter()
        .take(channels)
        .filter(|&&bit| bit != 0 && bit != BACK_CENTER && speaker_column(bit).is_none())
        .fold(0, |all, bit| all | bit)
}

/// The columns the source has (a back centre speaker counts as the back
/// left and right)
pub fn present_columns(channels: usize, mask: u32) -> [bool; INPUTS] {
    let mut present = [false; INPUTS];
    for c in input_columns(channels, mask).into_iter().flatten() {
        present[c] = true;
    }
    if back_center_inputs(channels, mask).contains(&true) {
        (present[4], present[5]) = (true, true);
    }
    present
}

/// The auto matrix for a source with the columns `present` (`mono`: a
/// single channel, played on every output) and a stream with or without
/// rear outputs. Fronts: front left / right as they are, the centre and the
/// LFE into both. Rear outputs: back and side make the rear (-3 dB each
/// when a source has both), a source without them plays its front on the
/// rear too. No rear outputs: back and side folded into the fronts at -3
/// dB (ITU-R BS.775, as the stereo downmix).
pub fn auto_matrix(present: &[bool; INPUTS], mono: bool, rear_out: bool, s: &MixSettings) -> Matrix {
    let mut m = [[0.0f32; INPUTS]; LANES];
    if mono {
        // The single channel's column is the centre's (the usual mask of
        // one channel); any present column plays everywhere
        for (c, _) in present.iter().enumerate().filter(|(_, p)| **p) {
            for lane in &mut m {
                lane[c] = 1.0;
            }
        }
        return m;
    }
    let (c, lfe, sur) = (db_to_lin(s.center_db), db_to_lin(s.lfe_db), db_to_lin(s.surround_db));
    m[0][0] = 1.0;
    m[1][1] = 1.0;
    (m[0][2], m[1][2]) = (c, c);
    (m[0][3], m[1][3]) = (lfe, lfe);
    let (bl, br, sl, sr) = (present[4], present[5], present[6], present[7]);
    if rear_out {
        let kl = if bl && sl { MINUS_3DB } else { 1.0 };
        let kr = if br && sr { MINUS_3DB } else { 1.0 };
        (m[2][4], m[2][6]) = (sur * kl, sur * kl);
        (m[3][5], m[3][7]) = (sur * kr, sur * kr);
        if !(bl || br || sl || sr) {
            (m[2], m[3]) = (m[0], m[1]);
        }
    } else {
        let k = MINUS_3DB * sur;
        (m[0][4], m[0][6]) = (k, k);
        (m[1][5], m[1][7]) = (k, k);
    }
    for row in &mut m {
        for (g, p) in row.iter_mut().zip(present) {
            if !p {
                *g = 0.0;
            }
        }
        if s.normalize {
            let sum: f32 = row.iter().map(|g| g.abs()).sum();
            if sum > 1.0 {
                row.iter_mut().for_each(|g| *g /= sum);
            }
        }
    }
    m
}

/// The matrix in force for a source (`channels`, `mask`) and a stream with
/// or without rear outputs: auto, or the one set (its columns the source
/// lacks at 0)
pub fn effective_matrix(channels: usize, mask: u32, rear_out: bool, s: &MixSettings) -> Matrix {
    let present = present_columns(channels, mask);
    if s.auto {
        return auto_matrix(&present, channels == 1, rear_out, s);
    }
    let mut m = s.matrix;
    for row in &mut m {
        for (g, p) in row.iter_mut().zip(&present) {
            if !p {
                *g = 0.0;
            }
        }
    }
    m
}

struct Shared {
    version: AtomicU64,
    settings: Mutex<MixSettings>,
}

/// The mixer settings of the broadcast running, shared by the control side
/// (which sets them) and the encode thread (which reads them)
#[derive(Clone)]
pub struct MixHandle(Arc<Shared>);

impl Default for MixHandle {
    fn default() -> Self {
        MixHandle(Arc::new(Shared { version: AtomicU64::new(0), settings: Mutex::new(MixSettings::default()) }))
    }
}

impl std::fmt::Debug for MixHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MixHandle").field(&self.get()).finish()
    }
}

impl MixHandle {
    /// New settings (held to the ranges): the encode thread takes them from
    /// its next block
    pub fn set(&self, settings: MixSettings) {
        let settings = settings.held();
        let mut s = self.0.settings.lock().unwrap_or_else(|e| e.into_inner());
        if *s != settings {
            *s = settings;
            self.0.version.fetch_add(1, Ordering::Release);
        }
    }

    pub fn get(&self) -> MixSettings {
        *self.0.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }

    fn try_get(&self) -> Option<MixSettings> {
        match self.0.settings.try_lock() {
            Ok(s) => Some(*s),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(*e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// The mixer on the encode thread, for one source format and stream
pub struct Mixer {
    handle: MixHandle,
    seen: u64,
    channels: usize,
    mask: u32,
    rear_out: bool,
    columns: [Option<usize>; MAX_INPUT],
    /// The input channels that are the back centre speaker
    back_center: [bool; MAX_INPUT],
    rate: f32,
    /// Per lane, the gain of each interleaved input channel
    gains: [[f32; MAX_INPUT]; LANES],
    /// Per lane: delay in samples, polarity
    delay: [f32; LANES],
    sign: [f32; LANES],
    /// Per lane, the delay line (capacity `cap`), written at `pos`
    lines: [Vec<f32>; LANES],
    pos: usize,
    cap: usize,
}

impl Mixer {
    /// For a source of `channels` with the speakers of `mask` at `rate`
    /// (Hz), a stream with or without rear outputs. Allocates.
    pub fn new(handle: MixHandle, channels: usize, mask: u32, rear_out: bool, rate: u32) -> Mixer {
        let cap = (f64::from(MAX_DELAY_MS) / 1000.0 * f64::from(rate.max(1))).ceil() as usize + 2;
        let unused = unused_speakers(channels, mask);
        if unused != 0 {
            tracing::warn!("Auracast mixer: no column for the source's speakers 0x{unused:x}, they are not heard");
        }
        let mut m = Mixer {
            handle,
            seen: u64::MAX,
            channels,
            mask,
            rear_out,
            columns: input_columns(channels, mask),
            back_center: back_center_inputs(channels, mask),
            rate: rate.max(1) as f32,
            gains: [[0.0; MAX_INPUT]; LANES],
            delay: [0.0; LANES],
            sign: [1.0; LANES],
            lines: std::array::from_fn(|_| vec![0.0; cap]),
            pos: 0,
            cap,
        };
        m.refresh();
        m
    }

    /// Takes changed settings (unless the control side holds them now)
    pub fn refresh(&mut self) {
        let version = self.handle.version();
        if version == self.seen {
            return;
        }
        let Some(s) = self.handle.try_get() else { return };
        self.seen = version;
        let m = effective_matrix(self.channels, self.mask, self.rear_out, &s);
        for (lane, row) in self.gains.iter_mut().zip(&m) {
            for ((g, col), back_center) in lane.iter_mut().zip(&self.columns).zip(self.back_center) {
                *g = if back_center { MINUS_3DB * (row[4] + row[5]) } else { col.map_or(0.0, |c| row[c]) };
            }
        }
        for l in 0..LANES {
            self.delay[l] = (s.delay_ms[l] / 1000.0 * self.rate).min((self.cap - 2) as f32);
            self.sign[l] = if s.invert[l] { -1.0 } else { 1.0 };
        }
        tracing::debug!(
            "Auracast mixer: {} matrix {:?}, delays {:?} ms, inverted {:?}",
            if s.auto { "auto" } else { "manual" },
            m,
            s.delay_ms,
            s.invert
        );
    }

    /// The lanes of one input frame (its channels, interleaved order)
    #[inline]
    pub fn apply(&self, ch: &[f32]) -> [f32; LANES] {
        std::array::from_fn(|lane| ch.iter().zip(&self.gains[lane]).map(|(x, g)| x * g).sum())
    }

    /// Delay and polarity of one frame of lanes, in place
    #[inline]
    pub fn post(&mut self, s: &mut [f32; LANES]) {
        let (pos, cap) = (self.pos, self.cap);
        for (l, x) in s.iter_mut().enumerate() {
            let line = &mut self.lines[l];
            line[pos] = *x;
            let d = self.delay[l];
            let out = if d <= 0.0 {
                *x
            } else {
                let di = d as usize;
                let fr = d - di as f32;
                let a = line[(pos + cap - di) % cap];
                let b = line[(pos + cap - di - 1) % cap];
                a + (b - a) * fr
            };
            *x = out * self.sign[l];
        }
        self.pos = (pos + 1) % cap;
    }

    /// Drops the delayed audio (an old stream's)
    pub fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    #[test]
    fn auto_matches_the_downmix_and_the_speaker_matrix() {
        let s = MixSettings::default();
        // 5.1 to stereo: as the downmix (centre -3 dB, LFE -6 dB, back -3 dB)
        let m = effective_matrix(6, 0x3F, false, &s);
        let f = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let lanes = |m: &Matrix, f: &[f32], cols: &[usize]| -> [f32; LANES] {
            std::array::from_fn(|l| f.iter().zip(cols).map(|(x, &c)| x * m[l][c]).sum())
        };
        let cols = [0, 1, 2, 3, 4, 5];
        let (dl, dr) = crate::le::downmix(&f);
        let out = lanes(&m, &f, &cols);
        assert!(close(out[0], dl) && close(out[1], dr), "{out:?} vs {dl} {dr}");
        assert_eq!((out[2], out[3]), (0.0, 0.0));
        // 5.1 to four outputs: as speaker_matrix
        let m = effective_matrix(6, 0x3F, true, &s);
        let (sm, _) = crate::le::speaker_matrix(6, 0x3F);
        for l in 0..LANES {
            for i in 0..6 {
                assert!(close(m[l][cols[i]], sm[l][i]), "lane {l} input {i}");
            }
        }
        // Stereo source, rear outputs: the rear plays the front
        let m = effective_matrix(2, 0x3, true, &s);
        assert_eq!((m[2], m[3]), (m[0], m[1]));
        // Mono: everywhere
        let m = effective_matrix(1, 0x4, true, &s);
        assert!(m.iter().all(|row| row[2] == 1.0));
    }

    #[test]
    fn levels_normalize_and_manual() {
        let mut s = MixSettings { center_db: 0.0, lfe_db: -60.0, normalize: true, ..MixSettings::default() };
        let m = effective_matrix(6, 0x3F, true, &s);
        // FL + FC at 0 dB + LFE at -60 dB, normalized
        let sum: f32 = m[0].iter().map(|g| g.abs()).sum();
        assert!(close(sum, 1.0));
        assert!(close(m[0][0], m[0][2]));
        // Manual: as set, columns the source lacks dropped
        s.auto = false;
        s.matrix = [[0.0; INPUTS]; LANES];
        s.matrix[0][1] = 1.0; // FR into the front left
        s.matrix[3][7] = 1.0; // SR into the rear right: a 5.1 back source lacks it
        let m = effective_matrix(6, 0x3F, true, &s);
        assert_eq!(m[0][1], 1.0);
        assert_eq!(m[3][7], 0.0);
        assert_eq!(present_columns(6, 0x60F), [true, true, true, true, false, false, true, true]);
    }

    #[test]
    fn a_back_center_speaker_plays_as_the_back_left_and_right() {
        // 6.1: FL FR FC LFE BC SL SR
        let mask = 0x70F;
        assert_eq!(present_columns(7, mask), [true, true, true, true, true, true, true, true]);
        assert_eq!(back_center_inputs(7, mask)[..7], [false, false, false, false, true, false, false]);
        assert_eq!(unused_speakers(7, mask), 0);
        let mx = Mixer::new(MixHandle::default(), 7, mask, true, 48000);
        // The back centre alone: on both rears, -3 dB for being split and
        // -3 dB more beside the sides
        let out = mx.apply(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!((out[0], out[1]), (0.0, 0.0));
        assert!(close(out[2], 0.5) && close(out[3], 0.5), "{out:?}");
        // The side left: the rear left only
        let out = mx.apply(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(close(out[2], MINUS_3DB) && out[3] == 0.0, "{out:?}");
        // Without rear outputs: folded into both fronts
        let mx = Mixer::new(MixHandle::default(), 7, mask, false, 48000);
        let out = mx.apply(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert!(close(out[0], 0.5) && close(out[1], 0.5) && out[2] == 0.0, "{out:?}");
        // A top speaker has no column
        assert_eq!(unused_speakers(3, 0x1003), 0x1000);
    }

    #[test]
    fn held_to_the_ranges() {
        let mut s = MixSettings::default();
        s.matrix[0][0] = 100.0;
        s.matrix[1][1] = f32::NAN;
        s.delay_ms = [-1.0, 1e6, f32::INFINITY, 2.5];
        let h = s.held();
        assert_eq!((h.matrix[0][0], h.matrix[1][1]), (MAX_GAIN, 0.0));
        assert_eq!(h.delay_ms, [0.0, MAX_DELAY_MS, 0.0, 2.5]);
    }

    #[test]
    fn mixer_follows_the_handle_and_delays_fractionally() {
        let handle = MixHandle::default();
        let mut mx = Mixer::new(handle.clone(), 4, 0x33, true, 1000);
        // Quad, auto: the four speakers as they are
        assert_eq!(mx.apply(&[0.1, 0.2, 0.3, 0.4]), [0.1, 0.2, 0.3, 0.4]);
        // Swap left and right, delay the rear right 2.5 samples (2.5 ms at
        // 1 kHz), invert the front right
        let mut s = handle.get();
        s.auto = false;
        s.matrix = [[0.0; INPUTS]; LANES];
        (s.matrix[0][1], s.matrix[1][0], s.matrix[2][4], s.matrix[3][5]) = (1.0, 1.0, 1.0, 1.0);
        s.delay_ms[3] = 2.5;
        s.invert[1] = true;
        handle.set(s);
        mx.refresh();
        assert_eq!(mx.apply(&[0.1, 0.2, 0.3, 0.4]), [0.2, 0.1, 0.3, 0.4]);
        // An impulse on every lane, then silence
        let mut out = Vec::new();
        for n in 0..6 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let mut lanes = [x; LANES];
            mx.post(&mut lanes);
            out.push(lanes);
        }
        assert_eq!(out[0][..3], [1.0, -1.0, 1.0]);
        let rr: Vec<f32> = out.iter().map(|l| l[3]).collect();
        // 2.5 samples: half at 2, half at 3
        assert_eq!(rr, [0.0, 0.0, 0.5, 0.5, 0.0, 0.0]);
        mx.reset();
        let mut lanes = [0.0; LANES];
        mx.post(&mut lanes);
        assert_eq!(lanes, [0.0; LANES]);
    }
}
