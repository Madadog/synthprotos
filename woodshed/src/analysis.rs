//! Display data derived from a track: waveform peaks and a log-frequency
//! (pitch-axis) spectrogram computed on background threads.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use realfft::RealFftPlanner;

use crate::pitch::midi_to_freq;
use crate::track::Track;

// ---------------------------------------------------------------- waveform

/// Min/max pyramid of the mono mix, for drawing the waveform at any zoom.
pub struct Peaks {
    /// `levels[k]` holds (min, max) per block of `BASE * 4^k` frames.
    levels: Vec<Vec<[f32; 2]>>,
}

impl Peaks {
    const BASE: usize = 64;

    pub fn build(track: &Track) -> Peaks {
        let base: Vec<[f32; 2]> = track
            .samples
            .chunks(Self::BASE * 2)
            .map(|block| {
                block.chunks_exact(2).fold([f32::MAX, f32::MIN], |[lo, hi], f| {
                    let m = (f[0] + f[1]) * 0.5;
                    [lo.min(m), hi.max(m)]
                })
            })
            .collect();
        let mut levels = vec![base];
        while levels.last().unwrap().len() > 64 {
            let next = levels
                .last()
                .unwrap()
                .chunks(4)
                .map(|c| c.iter().fold([f32::MAX, f32::MIN], |[lo, hi], p| [lo.min(p[0]), hi.max(p[1])]))
                .collect();
            levels.push(next);
        }
        Peaks { levels }
    }

    /// (min, max) of the mono mix over frames `[a, b)`.
    pub fn range(&self, track: &Track, a: f64, b: f64) -> (f32, f32) {
        let n = track.frames() as i64;
        let a = (a.floor() as i64).clamp(0, n);
        let b = (b.ceil() as i64).clamp(a, n);
        if a == b {
            return (0.0, 0.0);
        }
        let len = (b - a) as usize;
        if len < Self::BASE * 2 {
            return (a..b).fold((f32::MAX, f32::MIN), |(lo, hi), i| {
                let m = track.mono(i);
                (lo.min(m), hi.max(m))
            });
        }
        // The coarsest level that still gives at least two blocks.
        let mut level = 0;
        let mut block = Self::BASE;
        while level + 1 < self.levels.len() && block * 8 <= len {
            level += 1;
            block *= 4;
        }
        let data = &self.levels[level];
        let i0 = (a as usize / block).min(data.len() - 1);
        let i1 = (b as usize).div_ceil(block).clamp(i0 + 1, data.len());
        data[i0..i1].iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[0]), hi.max(p[1])))
    }
}

// ------------------------------------------------------------- spectrogram

pub const ROWS_PER_SEMITONE: usize = 4;
/// Pitch range of the spectrogram in MIDI note numbers: just below A0 up to C9.
pub const PITCH_MIN: f64 = 20.5;
pub const PITCH_MAX: f64 = 120.5;
pub const ROWS: usize = ((PITCH_MAX - PITCH_MIN) as usize) * ROWS_PER_SEMITONE;
/// Stored levels span this many dB below full scale.
pub const DB_RANGE: f32 = 110.0;
/// Columns are computed (and become available) in blocks of this many.
pub const BLOCK: usize = 256;

/// Magnitudes in dB, quantised to bytes, on a log-frequency grid.
///
/// Three FFT sizes are combined so low notes get the frequency resolution
/// they need while high notes keep reasonable time resolution.
pub struct Spectrogram {
    pub sample_rate: u32,
    /// Frames between columns.
    pub hop: usize,
    pub cols: usize,
    /// Column-major: `data[col * ROWS + row]`, row 0 = lowest pitch.
    data: RwLock<Vec<u8>>,
    block_done: Vec<AtomicBool>,
    blocks_done: AtomicUsize,
    peak: AtomicU8,
    cancel: AtomicBool,
}

struct RowMap {
    fft: usize,
    lo: usize,
    hi: usize,
    frac: f32,
}

impl Spectrogram {
    /// Allocate the spectrogram and start computing it in the background.
    /// `wake` is called whenever more columns become available.
    pub fn start(track: Arc<Track>, wake: impl Fn() + Send + Sync + 'static) -> Arc<Spectrogram> {
        let scale = (track.sample_rate as f64 / 48000.0).ceil().max(1.0) as usize;
        let scale = scale.next_power_of_two();
        let hop = 512 * scale;
        let cols = track.frames().div_ceil(hop).max(1);
        let blocks = cols.div_ceil(BLOCK);
        let spec = Arc::new(Spectrogram {
            sample_rate: track.sample_rate,
            hop,
            cols,
            data: RwLock::new(vec![0; cols * ROWS]),
            block_done: (0..blocks).map(|_| AtomicBool::new(false)).collect(),
            blocks_done: AtomicUsize::new(0),
            peak: AtomicU8::new(0),
            cancel: AtomicBool::new(false),
        });
        let next_block = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(wake);
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1)).clamp(1, 6);
        for _ in 0..threads {
            let (spec, track, next_block, wake) = (spec.clone(), track.clone(), next_block.clone(), wake.clone());
            std::thread::spawn(move || spec.worker(&track, scale, &next_block, &*wake));
        }
        spec
    }

    /// Stop the background computation (the track was replaced).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> f32 {
        self.blocks_done.load(Ordering::Relaxed) as f32 / self.block_done.len() as f32
    }

    pub fn is_complete(&self) -> bool {
        self.blocks_done.load(Ordering::Relaxed) == self.block_done.len()
    }

    /// Loudest stored value so far.
    pub fn peak(&self) -> u8 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Number of finished blocks among those overlapping columns `[c0, c1)`.
    pub fn blocks_ready(&self, c0: usize, c1: usize) -> usize {
        let b1 = c1.min(self.cols).div_ceil(BLOCK);
        (c0 / BLOCK..b1).filter(|&b| self.block_done[b].load(Ordering::Relaxed)).count()
    }

    /// Run `f` with read access to the column-major data.
    pub fn with_data<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(&self.data.read().unwrap())
    }

    fn worker(&self, track: &Track, scale: usize, next_block: &AtomicUsize, wake: &dyn Fn()) {
        let sr = track.sample_rate as f64;
        let sizes = [16384 * scale, 8192 * scale, 4096 * scale];
        let mut planner = RealFftPlanner::<f32>::new();
        let ffts: Vec<_> = sizes.iter().map(|&n| planner.plan_fft_forward(n)).collect();
        let windows: Vec<Vec<f32>> = sizes
            .iter()
            .map(|&n| (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect())
            .collect();
        // dB offset so that a full-scale sine reads 0 dB whatever the FFT size.
        let offsets: Vec<f32> = windows.iter().map(|w| 20.0 * (2.0 / w.iter().sum::<f32>()).log10()).collect();

        // Which FFT, and which of its bins, feed each pitch row. Around the
        // band edges (250 Hz and 1 kHz) rows cross-fade between two FFT sizes
        // over an octave, so the change of resolution leaves no visible seam.
        let half_row = 0.5 / ROWS_PER_SEMITONE as f64;
        let map = |fft: usize, pitch: f64| -> Option<RowMap> {
            let f = midi_to_freq(pitch);
            let bin = sr / sizes[fft] as f64;
            let (k_lo, k_hi) = (midi_to_freq(pitch - half_row) / bin, midi_to_freq(pitch + half_row) / bin);
            if k_hi >= (sizes[fft] / 2 - 1) as f64 {
                return None; // above Nyquist
            }
            Some(if k_hi - k_lo < 1.0 {
                // Row narrower than a bin: interpolate between neighbours.
                let k = f / bin;
                RowMap { fft, lo: k as usize, hi: k as usize + 1, frac: k.fract() as f32 }
            } else {
                // Row spans several bins: take the strongest.
                RowMap { fft, lo: k_lo.ceil() as usize, hi: k_hi.floor() as usize, frac: -1.0 }
            })
        };
        let rows: Vec<Option<(RowMap, RowMap, f32)>> = (0..ROWS)
            .map(|r| {
                let pitch = PITCH_MIN + (r as f64 + 0.5) / ROWS_PER_SEMITONE as f64;
                // Position on a scale where 0, 1, 2 are the three FFT sizes.
                let octaves = (midi_to_freq(pitch) / 250.0).log2();
                let band = (octaves.clamp(-0.5, 0.5) + 0.5) + ((octaves - 2.0).clamp(-0.5, 0.5) + 0.5);
                let low = (band.floor() as usize).min(sizes.len() - 2);
                Some((map(low, pitch)?, map(low + 1, pitch)?, (band - low as f64) as f32))
            })
            .collect();
        let used = |i: usize| rows.iter().flatten().flat_map(|(a, b, _)| [a, b]).filter(move |m| m.fft == i);
        let bins_needed: Vec<usize> = (0..sizes.len()).map(|i| used(i).map(|m| m.hi + 1).max().unwrap_or(0)).collect();

        let mut inputs: Vec<Vec<f32>> = ffts.iter().map(|f| f.make_input_vec()).collect();
        let mut outputs: Vec<_> = ffts.iter().map(|f| f.make_output_vec()).collect();
        let mut scratch: Vec<_> = ffts.iter().map(|f| f.make_scratch_vec()).collect();
        let mut power: Vec<Vec<f32>> = bins_needed.iter().map(|&n| vec![0.0; n]).collect();
        let mut block_out = vec![0u8; BLOCK * ROWS];

        loop {
            let block = next_block.fetch_add(1, Ordering::Relaxed);
            if block >= self.block_done.len() || self.cancel.load(Ordering::Relaxed) {
                return;
            }
            let c0 = block * BLOCK;
            let c1 = (c0 + BLOCK).min(self.cols);
            let mut peak = 0u8;
            for col in c0..c1 {
                let centre = (col * self.hop) as i64;
                for i in 0..sizes.len() {
                    let first = centre - (sizes[i] / 2) as i64;
                    for (j, (x, w)) in inputs[i].iter_mut().zip(&windows[i]).enumerate() {
                        *x = track.mono(first + j as i64) * w;
                    }
                    ffts[i].process_with_scratch(&mut inputs[i], &mut outputs[i], &mut scratch[i]).unwrap();
                    for (p, c) in power[i].iter_mut().zip(&outputs[i]) {
                        *p = c.norm_sqr();
                    }
                }
                let out = &mut block_out[(col - c0) * ROWS..][..ROWS];
                let level = |m: &RowMap| -> f32 {
                    let p = &power[m.fft];
                    let pw = if m.frac >= 0.0 {
                        p[m.lo] + (p[m.hi] - p[m.lo]) * m.frac
                    } else {
                        p[m.lo..=m.hi].iter().fold(0.0f32, |a, &b| a.max(b))
                    };
                    10.0 * pw.max(1e-20).log10() + offsets[m.fft]
                };
                for (value, row) in out.iter_mut().zip(&rows) {
                    *value = match row {
                        None => 0,
                        Some((low, high, mix)) => {
                            let db = match *mix {
                                m if m <= 0.0 => level(low),
                                m if m >= 1.0 => level(high),
                                m => level(low) * (1.0 - m) + level(high) * m,
                            };
                            ((db + DB_RANGE) / DB_RANGE * 255.0).clamp(0.0, 255.0) as u8
                        }
                    };
                    peak = peak.max(*value);
                }
            }
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            self.data.write().unwrap()[c0 * ROWS..c1 * ROWS].copy_from_slice(&block_out[..(c1 - c0) * ROWS]);
            self.peak.fetch_max(peak, Ordering::Relaxed);
            self.block_done[block].store(true, Ordering::Relaxed);
            self.blocks_done.fetch_add(1, Ordering::Relaxed);
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, amp: f32, secs: f64, sr: u32) -> Arc<Track> {
        let n = (secs * sr as f64) as usize;
        let mono: Vec<f32> =
            (0..n).map(|i| (std::f64::consts::TAU * freq * i as f64 / sr as f64).sin() as f32 * amp).collect();
        Arc::new(Track::from_mono(sr, &mono))
    }

    fn wait(spec: &Spectrogram) {
        while !spec.is_complete() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn sine_peaks_on_its_pitch_row_at_full_scale() {
        // One note per FFT band: A2 (110 Hz), A4 (440 Hz), A6 (1760 Hz).
        for midi in [45.0, 69.0, 93.0] {
            let track = sine(midi_to_freq(midi), 1.0, 2.0, 44100);
            let spec = Spectrogram::start(track, || {});
            wait(&spec);
            let col = spec.cols / 2;
            let column: Vec<u8> = spec.with_data(|d| d[col * ROWS..(col + 1) * ROWS].to_vec());
            let (row, &value) = column.iter().enumerate().max_by_key(|(_, v)| **v).unwrap();
            let pitch = PITCH_MIN + (row as f64 + 0.5) / ROWS_PER_SEMITONE as f64;
            assert!((pitch - midi).abs() <= 0.25, "peak at pitch {pitch}, expected {midi}");
            // A full-scale sine is 0 dB = 255, within a dB or so.
            assert!(value >= 250, "peak level {value} for midi {midi}");
            assert_eq!(spec.peak(), column.iter().copied().max().unwrap().max(spec.peak()));
        }
    }

    #[test]
    fn peaks_match_brute_force() {
        let track = sine(3.0, 0.8, 4.0, 8000);
        let peaks = Peaks::build(&track);
        for (a, b) in [(0.0, 32000.0), (100.0, 150.0), (1234.0, 9000.0), (31000.0, 40000.0), (5.5, 5.6)] {
            let (lo, hi) = peaks.range(&track, a, b);
            let (ia, ib) = (a.floor() as i64, (b.ceil() as i64).min(32000));
            let want_lo = (ia..ib).map(|i| track.mono(i)).fold(f32::MAX, f32::min);
            let want_hi = (ia..ib).map(|i| track.mono(i)).fold(f32::MIN, f32::max);
            // The pyramid may include up to one extra block at each end.
            assert!(lo <= want_lo + 1e-6 && lo >= want_lo - 0.2, "min over {a}..{b}: {lo} vs {want_lo}");
            assert!(hi >= want_hi - 1e-6 && hi <= want_hi + 0.2, "max over {a}..{b}: {hi} vs {want_hi}");
        }
    }
}
