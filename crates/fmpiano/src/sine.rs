//! Sine lookup with linear interpolation, addressed by a 32-bit phase
//! accumulator (one full cycle = 2^32), the classic FM-synth oscillator core.

/// Phase units per radian (2^32 / 2π).
pub const PHASE_PER_RAD: f32 = 683_565_275.576_431_6;

const BITS: u32 = 11;
const N: usize = 1 << BITS;
const SHIFT: u32 = 32 - BITS;
const FRAC_MASK: u32 = (1 << SHIFT) - 1;
const FRAC_SCALE: f32 = 1.0 / (1u32 << SHIFT) as f32;

/// 2048-entry table storing (value, slope) pairs; worst-case error ≈ -118 dB.
pub struct SineTable {
    v: Box<[[f32; 2]; N]>,
}

impl Default for SineTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SineTable {
    pub fn new() -> Self {
        let mut v = Box::new([[0.0f32; 2]; N]);
        let step = core::f64::consts::TAU / N as f64;
        for (i, e) in v.iter_mut().enumerate() {
            let s0 = (i as f64 * step).sin();
            let s1 = ((i + 1) as f64 * step).sin();
            *e = [s0 as f32, (s1 - s0) as f32];
        }
        Self { v }
    }

    #[inline(always)]
    pub fn sin(&self, phase: u32) -> f32 {
        let i = (phase >> SHIFT) as usize & (N - 1);
        let f = (phase & FRAC_MASK) as f32 * FRAC_SCALE;
        let e = self.v[i];
        e[0] + e[1] * f
    }
}

/// Convert a phase offset in radians (any size) to phase units, wrapping.
#[inline(always)]
pub fn rad_to_phase(x: f32) -> u32 {
    (x * PHASE_PER_RAD) as i64 as u32
}

/// Phase increment per sample for a frequency in Hz.
#[inline]
pub fn phase_inc(freq: f64, sample_rate: f64) -> u32 {
    let cycles = (freq / sample_rate).rem_euclid(1.0);
    (cycles * 4_294_967_296.0) as u64 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_accuracy() {
        let t = SineTable::new();
        let mut worst = 0.0f64;
        for k in 0..100_000u64 {
            let ph = (k.wrapping_mul(2_654_435_761) & 0xffff_ffff) as u32;
            let x = ph as f64 / 4_294_967_296.0 * core::f64::consts::TAU;
            worst = worst.max((t.sin(ph) as f64 - x.sin()).abs());
        }
        assert!(worst < 3e-6, "worst error {worst}");
    }

    #[test]
    fn pm_wraps() {
        assert_eq!(rad_to_phase(0.0), 0);
        let half = rad_to_phase(core::f32::consts::PI);
        assert!((half as i64 - (1i64 << 31)).abs() < 512);
        // large negative offsets wrap instead of saturating
        let neg = rad_to_phase(-7.0 * core::f32::consts::PI);
        assert!((neg as i64 - (1i64 << 31)).abs() < 4096);
    }
}
