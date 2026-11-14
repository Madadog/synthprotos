//! The idealised piano.
//!
//! The fitted engine reproduces one recorded instrument key by key, quirks
//! included: the recordings were made every minor third, so each recorded key
//! has its own hammer, voicing and microphone colour, and the keys in between
//! are blends of two of them.  This engine instead describes every key with
//! the same smooth physical laws, calibrated on what the fitted model learnt
//! directly on the recordings (`tools/ideal/calibrate.py`):
//!
//! * stiff-string partials `f_n = n f0 sqrt(1 + B n²)`, with smooth curves for
//!   B and the stretched tuning;
//! * initial partial amplitudes = level(key, velocity) × hammer contact
//!   low-pass (shorter, i.e. brighter, for harder strikes) × soundboard
//!   radiation (one curve for all keys: weak below its low corner, which
//!   thins the bass fundamentals) × string slope n^-γ × strike-point comb
//!   `|sin(π n β)|` (β = 1/8.5, the notches the recordings show in the bass);
//! * two-stage decay per partial: aftersound rate `b1 + b3 f²` (Bank &
//!   Välimäki's loss model), a faster prompt sound, and the aftersound share.
//!
//! At note-on that partial description is turned into FM stacks: stacks are
//! placed with the same in-tune layout rule as the fit, each partial goes to
//! its nearest stack, each stack gets its share's power and an FM index from
//! the share's spread (an FM spectrum's variance in partials is I²/2), the
//! overlap between neighbouring stacks is corrected, and the index decays so
//! that the outer partials die faster.  Everything is a continuous function of
//! key, velocity and the voicing controls.

#![allow(clippy::needless_range_loop)] // index-heavy numerics read better with ranges

use crate::model::{
    assemble, partial, velocity_layer, KnockModel, StackEnv, Tone, VoiceShape, VoiceSpec,
    VOICE_STACKS,
};

// -------------------------------------------------- calibrated smooth laws
// tools/ideal/calibration.json: one joint fit to the partial envelopes of the
// recordings (30 keys x 5 velocity layers, ERB bands, 7.3 dB weighted rms).
// Polynomials in u = (key - 64) / 44, lowest order first.
const LEVEL: [f64; 4] = [-12.7431, -1.71124, 3.17615, 6.48239];
const LEVEL_VEL: [f64; 3] = [1.1007, -0.305502, -0.485892];
/// Curvature of level over dB(velocity): + dv2 * V² / 20 dB, V = 20 log10 v.
const LEVEL_VEL2: f64 = 0.38059;
const CONTACT: [f64; 3] = [-8.34804, -0.173658, 0.601235];
const CONTACT_VEL: [f64; 2] = [0.78472, 0.0389392];
/// Curvature of ln(contact time) over ln(velocity).
const CONTACT_VEL2: f64 = -0.0840629;
const HAMMER_Q: [f64; 2] = [4.00396, -1.14045];
const GAMMA: [f64; 2] = [1.12557, 0.425891];
const BOARD_LO: f64 = 4.19391; // ln Hz
const BOARD_HI: f64 = 9.90349; // ln Hz
const BOARD_ORDER: f64 = 5.34772;
const LOSS_B1: [f64; 4] = [-1.2902, 1.50136, 1.27231, -0.427587];
const LOSS_B3: [f64; 3] = [-24.2066, 20.0, -12.9901];
const AFTER_SHARE: [f64; 4] = [-2.60377, 0.098284, 2.34234, -1.69984];
const PROMPT_SPEED: [f64; 4] = [1.99149, 0.136281, -0.896444, -0.224635];
/// Strike-point comb (strike 1/8.5): ln of the notch-filling term eps(u).
const COMB_EPS: [f64; 2] = [1.16095, 6.03941];
// smooth fits to the measured inharmonicity and tuning (highest order first)
const LOG10_B: [f64; 5] = [0.45909, -0.87932, 0.32053, 1.77913, -3.40977];
const STRETCH_CENTS: [f64; 4] = [24.3399, 11.5195, 5.8238, -0.444];

/// Frequency-squared loss (1/s per Hz²) added per unit of `high_damp` above 1:
/// a partial at 3 kHz then decays about 8 dB/s faster.
const EXTRA_LOSS: f64 = 1e-7;

/// Most partials considered per note.
const MAX_PARTIALS: usize = 120;

fn poly_lo(c: &[f64], u: f64) -> f64 {
    c.iter().rev().fold(0.0, |acc, &k| acc * u + k)
}

fn poly_hi(c: &[f64], u: f64) -> f64 {
    c.iter().fold(0.0, |acc, &k| acc * u + k)
}

/// Voicing controls of the idealised piano (the tone controls shared with the
/// fitted engine live in [`Tone`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IdealParams {
    /// Strike point as a fraction of the speaking length (1/8.5 typical).
    pub strike: f32,
    /// Depth of the strike-point notches in the spectrum: 0 = none, 0.5 = as
    /// measured on the recorded grand, 1 = deep.
    pub notch: f32,
    /// Multiplier on the soundboard's low radiation corner: above 1 the bass
    /// fundamentals get weaker (small instrument), below 1 stronger.
    pub board: f32,
    /// Frequency-dependent string loss (how fast the upper partials die):
    /// 1 = as calibrated, above 1 adds damping that grows with frequency².
    pub high_damp: f32,
    /// Multiplier on the velocity-to-level range.
    pub dynamics: f32,
    /// Multiplier on the odds of the aftersound (the slow second stage of the
    /// decay): above 1 notes sing on longer after the initial drop.
    pub aftersound: f32,
}

impl IdealParams {
    pub const DEFAULT: IdealParams = IdealParams {
        strike: 1.0 / 8.5,
        notch: 0.5,
        board: 1.0,
        high_damp: 1.0,
        dynamics: 1.0,
        aftersound: 1.0,
    };
}

impl Default for IdealParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Smooth inharmonicity coefficient and tuning (cents re. 12-TET) of a key.
pub fn ideal_tuning(key: f32) -> (f64, f64) {
    let u = (key as f64 - 64.0) / 44.0;
    (10f64.powf(poly_hi(&LOG10_B, u)), poly_hi(&STRETCH_CENTS, u))
}

// -------------------------------------------------- Bessel functions
/// J_m(x) for integer m by its power series (|x| <= ~12 is plenty here).
pub fn bessel_j(m: i32, x: f64) -> f64 {
    let mm = m.unsigned_abs() as i32;
    let sign = if m < 0 && mm % 2 == 1 { -1.0 } else { 1.0 };
    if x == 0.0 {
        return if mm == 0 { 1.0 } else { 0.0 };
    }
    let h = 0.5 * x;
    let mut term = 1.0;
    for k in 1..=mm {
        term *= h / k as f64;
    }
    let mut sum = term;
    let h2 = h * h;
    for k in 1..80 {
        term *= -h2 / (k as f64 * (k + mm) as f64);
        sum += term;
        if term.abs() < 1e-17 * sum.abs().max(1e-300) {
            break;
        }
    }
    sign * sum
}

/// Amplitude on partial `n` of a unit stack centred on `c` with index `i`
/// (the 1:1 stack's lower sidebands fold back onto the harmonics).
fn stack_amp(c: usize, n: usize, i: f64) -> f64 {
    let (n, c) = (n as i32, c as i32);
    if c == 1 {
        let sgn = if n % 2 == 0 { 1.0 } else { -1.0 };
        bessel_j(n - 1, i) + sgn * bessel_j(n + 1, i)
    } else {
        bessel_j(n - c, i)
    }
}

// -------------------------------------------------- stack layout
fn sideband_error(c: f64, j: f64, f0: f64, b: f64) -> f64 {
    let fc = partial(c, f0, b);
    let fm = if c == 1.0 {
        fc
    } else {
        0.5 * (partial(c + 1.0, f0, b) - partial(c - 1.0, f0, b))
    };
    fc + j * fm - partial(c + j, f0, b)
}

/// Largest acceptable beat between a sideband and the real partial (Hz).
fn beat_tolerance(f: f64) -> f64 {
    let erb = 24.7 * (4.37 * f / 1000.0 + 1.0);
    (0.2 * erb).clamp(2.5, 12.0)
}

/// Largest |j| for which a stack's sidebands c-|j|..c+|j| are all in tune.
fn jmax_for(c: usize, f0: f64, b: f64) -> usize {
    let mut best = 0;
    for j in 1..=12usize {
        for sj in [j as i64, -(j as i64)] {
            let n = c as i64 + sj;
            if n < 1 {
                continue;
            }
            let e = sideband_error(c as f64, sj as f64, f0, b).abs();
            if e > beat_tolerance(partial(n as f64, f0, b)) {
                return best;
            }
        }
        best = j;
    }
    best
}

fn index_cap(c: usize, f0: f64, b: f64, n_top: usize) -> f64 {
    let mut cap = if c == 1 {
        3.0
    } else {
        (0.55 * c as f64).max(0.6)
    };
    let jm = jmax_for(c, f0, b);
    cap = cap.min(if jm == 0 { 0.15 } else { 0.9 * jm as f64 });
    if c > 1 {
        cap = cap.min((0.8 * (n_top as f64 - c as f64)).max(0.15));
    }
    cap
}

/// Stack centres: every partial up to 7, then about 1.22x apart.  Denser than
/// the fitted engine's layout (1, 2, 3, 4, 6, 9, 13, ...), because here each
/// stack's spectrum comes from moment matching rather than a fit, which is
/// only accurate when a stack covers a few partials (mapping error in the
/// ERB bands: bass 3.0 -> 1.5 dB, tenor 1.9 -> 1.2 dB, middle 1.4 -> 0.7 dB).
const BASE_CENTERS: [usize; 20] = [
    1, 2, 3, 4, 5, 6, 7, 9, 11, 14, 16, 20, 25, 30, 37, 45, 54, 66, 81, 99,
];

/// Stack centres: a roughly geometric base set, plus stacks wherever a
/// partial up to `nmax` cannot be reached in tune by a stack's sidebands.
fn layout(
    f0: f64,
    b: f64,
    nmax: usize,
    max_stacks: usize,
    out: &mut [usize; VOICE_STACKS],
) -> usize {
    let mut covered = [false; MAX_PARTIALS + 16];
    let mut k = 0;
    let cover = |c: usize, covered: &mut [bool; MAX_PARTIALS + 16]| {
        let jm = jmax_for(c, f0, b);
        for n in c.saturating_sub(jm).max(1)..=(c + jm).min(MAX_PARTIALS + 15) {
            covered[n] = true;
        }
    };
    for &c in BASE_CENTERS.iter().filter(|&&c| c <= nmax) {
        if k < max_stacks {
            out[k] = c;
            k += 1;
            cover(c, &mut covered);
        }
    }
    while k < max_stacks {
        let Some(first) = (1..=nmax).find(|&n| !covered[n]) else {
            break;
        };
        let next = out[..k]
            .iter()
            .copied()
            .filter(|&c| c > first)
            .min()
            .unwrap_or(nmax + 1);
        let mut best = first;
        for c in first..next {
            if c - jmax_for(c, f0, b).min(c) <= first {
                best = c;
            } else {
                break;
            }
        }
        out[k] = best;
        k += 1;
        cover(best, &mut covered);
    }
    out[..k].sort_unstable();
    k
}

// -------------------------------------------------- the voice
/// Parameters of a voice of the idealised piano.
pub fn ideal_voice_spec(
    key: u8,
    vel: f32,
    tone: &Tone,
    ip: &IdealParams,
    km: &KnockModel,
    sample_rate: f32,
) -> VoiceSpec {
    let kf = key as f32;
    let u = (kf as f64 - 64.0) / 44.0;
    let sr = sample_rate as f64;
    let (b0, cents) = ideal_tuning(kf);
    let b = b0 * tone.inharm as f64;
    let f1 = 440.0 * 2f64.powf((kf as f64 - 69.0) / 12.0 + cents * tone.stretch as f64 / 1200.0);
    let f0 = f1 / (1.0 + b).sqrt();
    // velocity on the scale the laws were calibrated on (recorded layers)
    let (layer, low_gain) = velocity_layer(vel, tone);
    let v = ((8.0 + layer as f64 * (127.0 - 8.0) / 15.0) / 127.0) * low_gain as f64;
    let v = v.max(1e-3);

    // -- partials: frequencies, initial amplitudes, decay rates
    let fmax = (0.45 * sr).min(16000.0);
    let vdb = 20.0 * v.log10(); // dB re. the loudest recorded layer
    let level_db = poly_lo(&LEVEL, u)
        + ip.dynamics as f64 * (poly_lo(&LEVEL_VEL, u) * vdb + LEVEL_VEL2 * vdb * vdb / 20.0);
    let level = 10f64.powf(level_db / 20.0);
    let hard = (1.0 + tone.hardness as f64).max(0.0);
    let lv = v.ln();
    let contact = (poly_lo(&CONTACT, u)
        + hard * (CONTACT_VEL2 * lv * lv - poly_lo(&CONTACT_VEL, u) * lv))
        .exp()
        / tone.brightness.max(0.05) as f64;
    let q = poly_lo(&HAMMER_Q, u).max(0.2);
    let gamma = poly_lo(&GAMMA, u);
    let (flo, fhi) = (BOARD_LO.exp() * ip.board as f64, BOARD_HI.exp());
    // notch 0.5 = as calibrated; towards 0 the notches fill in completely
    // (eps >= 4: under 0.2 dB of ripple), towards 1 they get 8x deeper
    let eps_cal = poly_lo(&COMB_EPS, u).exp();
    let nt = ip.notch.clamp(0.0, 1.0) as f64;
    let comb_eps = if nt < 0.5 {
        (2.0 * nt * eps_cal.ln() + (1.0 - 2.0 * nt) * eps_cal.max(4.0).ln()).exp()
    } else {
        eps_cal / 8f64.powf(2.0 * nt - 1.0)
    };
    let comb_norm = (0.5 + comb_eps * comb_eps).sqrt();
    let beta = ip.strike.clamp(0.02, 0.5) as f64;
    let b1 = poly_lo(&LOSS_B1, u).exp();
    // The calibrated f² loss only matters in the top octaves (below them the
    // recorded partials all decay at about the same rate).  `high_damp` scales
    // it and, above 1, adds a string loss of its own on every key.
    let hd = ip.high_damp.max(0.0) as f64;
    let b3 = poly_lo(&LOSS_B3, u).exp() * hd + (hd - 1.0).max(0.0) * EXTRA_LOSS;
    let after_odds = poly_lo(&AFTER_SHARE, u).exp() * ip.aftersound.max(0.0) as f64;
    let w2 = after_odds / (1.0 + after_odds);
    let speed = 1.0 + poly_lo(&PROMPT_SPEED, u).exp();

    let mut amp = [0.0f64; MAX_PARTIALS + 1]; // index = partial number
    let mut sig = [0.0f64; MAX_PARTIALS + 1];
    let mut np = 0;
    let mut pmax = 0.0f64;
    for n in 1..=MAX_PARTIALS {
        let f = partial(n as f64, f0, b);
        if f >= fmax {
            break;
        }
        let nf = n as f64;
        let h = (1.0 + (f * contact).powi(2)).powf(-q / 2.0);
        let x = (f / flo).powf(2.0 * BOARD_ORDER);
        let r = x / (1.0 + x) / (1.0 + (f / fhi).powi(2)).sqrt();
        let s = ((core::f64::consts::PI * nf * beta).sin().powi(2) + comb_eps * comb_eps).sqrt()
            / comb_norm;
        amp[n] = level * h * r * s * nf.powf(-gamma);
        sig[n] = b1 + b3 * f * f;
        pmax = pmax.max(amp[n] * amp[n]);
        np = n;
    }
    // partials more than 60 dB down are inaudible next to the rest
    let mut nmax = 1;
    for n in 1..=np {
        if amp[n] * amp[n] > pmax * 1e-6 {
            nmax = n;
        }
    }

    // -- stacks: layout, territories, moment matching
    let mut centers = [0usize; VOICE_STACKS];
    let k = layout(f0, b, nmax, VOICE_STACKS, &mut centers);
    let mut own = [usize::MAX; MAX_PARTIALS + 1];
    for n in 1..=nmax {
        let mut best = 0;
        for j in 1..k {
            if (centers[j] as i64 - n as i64).abs() < (centers[best] as i64 - n as i64).abs() {
                best = j;
            }
        }
        own[n] = best;
    }
    let mut energy = [0.0f64; VOICE_STACKS];
    let mut index = [0.0f64; VOICE_STACKS];
    let mut caps = [0.0f64; VOICE_STACKS];
    for j in 0..k {
        caps[j] = index_cap(centers[j], f0, b, nmax);
    }
    for n in 1..=nmax {
        energy[own[n]] += amp[n] * amp[n];
    }
    for j in 0..k {
        let c = centers[j];
        if energy[j] <= 0.0 {
            continue;
        }
        if c == 1 {
            // 1:1 stack: match the ratio of partial 2 to partial 1 if it owns both
            if nmax >= 2 && own[2] == j {
                let target = amp[2] / amp[1].max(1e-30);
                let mut best = (f64::INFINITY, 0.0);
                for g in 0..=96 {
                    let i = caps[j] * g as f64 / 96.0;
                    let r = (stack_amp(1, 2, i) / stack_amp(1, 1, i).abs().max(1e-9)).abs();
                    let e = (r - target).abs();
                    if e < best.0 {
                        best = (e, i);
                    }
                }
                index[j] = best.1;
            }
            continue;
        }
        let mut var = 0.0;
        for n in 1..=nmax {
            if own[n] == j {
                let d = n as f64 - c as f64;
                var += d * d * amp[n] * amp[n];
            }
        }
        index[j] = (2.0 * var / energy[j]).sqrt().min(caps[j]);
    }

    // -- overlap correction: share of stack l's power in territory j
    let mut m = [[0.0f64; VOICE_STACKS]; VOICE_STACKS];
    for l in 0..k {
        let c = centers[l];
        let reach = (3.0 * index[l] + 6.0) as usize;
        let lo = c.saturating_sub(reach).max(1);
        let hi = (c + reach).min(nmax);
        for n in lo..=hi {
            let a = stack_amp(c, n, index[l]);
            m[own[n]][l] += a * a;
        }
    }
    let mut power = energy;
    for _ in 0..40 {
        let mut got = [0.0f64; VOICE_STACKS];
        for j in 0..k {
            for l in 0..k {
                got[j] += m[j][l] * power[l];
            }
        }
        for j in 0..k {
            if got[j] > 0.0 {
                power[j] *= energy[j] / got[j];
            }
        }
    }

    // -- envelopes
    let mut envs = [StackEnv::default(); VOICE_STACKS];
    for j in 0..k {
        let c = centers[j];
        let a = power[j].max(0.0).sqrt();
        let s_c = if c <= np { sig[c] } else { b1 };
        let tau2 = 1.0 / s_c;
        let tau1 = tau2 / speed;
        let i0 = index[j];
        // index decay: follow the shrinking spread of the territory
        let mut taui = 1.0;
        if i0 > 0.05 {
            let t1 = tau2;
            let (mut e, mut var) = (0.0, 0.0);
            for n in 1..=nmax {
                if own[n] == j {
                    let env = (1.0 - w2) * (-speed * sig[n] * t1).exp() + w2 * (-sig[n] * t1).exp();
                    let p = (amp[n] * env).powi(2);
                    let d = n as f64 - c as f64;
                    e += p;
                    var += d * d * p;
                }
            }
            if e > 0.0 {
                let it = (2.0 * var / e).sqrt().min(0.999 * i0);
                taui = if it > 1e-3 {
                    t1 / (i0 / it).ln()
                } else {
                    t1 / 7.0
                };
            }
        }
        envs[j] = StackEnv {
            c: c as u8,
            p: [
                (a * (1.0 - w2)) as f32,
                (a * w2) as f32,
                tau1 as f32,
                tau2 as f32,
                i0 as f32,
                0.0,
                taui as f32,
            ],
        };
    }

    let shape = VoiceShape {
        f0,
        b,
        f1,
        vnorm: layer / 15.0,
        stack_gain: 1.0,
        knock_db: 20.0 * low_gain.log10(),
        index_scale: 1.0,
        spread_phases: true,
    };
    assemble(key, tone, km, sample_rate, &shape, &envs[..k])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bessel_values() {
        // J0(1) = 0.7651976866, J1(2.5) = 0.4970941025, J5(3) = 0.04302843487, J-3(2) = -0.1289432495
        assert!((bessel_j(0, 1.0) - 0.7651976866).abs() < 1e-9);
        assert!((bessel_j(1, 2.5) - 0.4970941025).abs() < 1e-9);
        assert!((bessel_j(5, 3.0) - 0.04302843487).abs() < 1e-9);
        assert!((bessel_j(-3, 2.0) + 0.1289432495).abs() < 1e-9);
    }

    fn power(v: &VoiceSpec) -> f32 {
        v.stacks[..v.n].iter().map(|s| (s.a1 + s.a2).powi(2)).sum()
    }

    #[test]
    fn louder_and_brighter_with_velocity() {
        let (t, km) = (Tone::GRAND, KnockModel::default());
        for key in [21u8, 40, 60, 80, 100] {
            let mut prev = (0.0f32, 0.0f32);
            for vel in [10.0f32, 30.0, 50.0, 70.0, 90.0, 110.0, 127.0] {
                let v = ideal_voice_spec(key, vel, &t, &IdealParams::DEFAULT, &km, 48000.0);
                let p = power(&v);
                // brightness: power-weighted mean carrier frequency
                let fc: f32 = v.stacks[..v.n]
                    .iter()
                    .map(|s| (s.a1 + s.a2).powi(2) * s.fc as f32)
                    .sum::<f32>()
                    / p;
                assert!(p > prev.0, "key {key} vel {vel}: not louder");
                assert!(fc >= prev.1 * 0.999, "key {key} vel {vel}: not brighter");
                prev = (p, fc);
            }
        }
    }

    #[test]
    fn voicing_controls_stay_finite() {
        let km = KnockModel::default();
        let extremes = [
            IdealParams {
                strike: 0.03,
                notch: 1.0,
                board: 0.2,
                high_damp: 8.0,
                dynamics: 2.5,
                aftersound: 20.0,
            },
            IdealParams {
                strike: 0.3,
                notch: 0.0,
                board: 5.0,
                high_damp: 0.1,
                dynamics: 0.2,
                aftersound: 0.0,
            },
        ];
        for ip in &extremes {
            for key in (21..=108u8).step_by(5) {
                let v = ideal_voice_spec(key, 100.0, &Tone::GRAND, ip, &km, 48000.0);
                assert!(v.n >= 1);
                for s in &v.stacks[..v.n] {
                    let ok = [s.a1, s.a2, s.tau1, s.tau2, s.i0, s.taui]
                        .iter()
                        .all(|x| x.is_finite());
                    assert!(ok && s.tau1 > 0.0 && s.tau2 > 0.0, "key {key}: {s:?}");
                }
            }
        }
    }

    #[test]
    fn every_key_has_stacks_and_is_smooth() {
        let t = Tone::GRAND;
        let ip = IdealParams::DEFAULT;
        let km = KnockModel::default();
        let mut prev = 0.0f32;
        for key in 21..=108u8 {
            let v = ideal_voice_spec(key, 90.0, &t, &ip, &km, 48000.0);
            assert!(v.n >= 1 && v.n <= VOICE_STACKS, "key {key}");
            for s in &v.stacks[..v.n] {
                assert!(s.a1.is_finite() && s.a2.is_finite() && s.a1 >= 0.0 && s.a2 >= 0.0);
                assert!(s.tau1 > 0.0 && s.tau2 > 0.0 && s.taui > 0.0 && s.i0.is_finite());
            }
            // total initial amplitude changes gradually from key to key
            let level: f32 = v.stacks[..v.n]
                .iter()
                .map(|s| (s.a1 + s.a2).powi(2))
                .sum::<f32>()
                .sqrt();
            if key > 21 {
                let step = 20.0 * (level / prev).log10();
                assert!(step.abs() < 2.5, "level jumps {step:.1} dB at key {key}");
            }
            prev = level;
        }
    }
}
