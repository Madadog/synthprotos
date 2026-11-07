//! The piano model: turns (key, velocity, tone preset) into the parameters of
//! one FM voice.
//!
//! Each voice is a set of 2-operator FM "stacks".  Stack `k` puts its carrier
//! on the stretched partial `c_k` of the string,
//!
//! ```text
//!     f_n = n · f0 · sqrt(1 + B n²)          (stiff-string inharmonicity)
//! ```
//!
//! and its modulator on the *local* partial spacing, so the FM sidebands at
//! `f_c ± j·f_m` land on the neighbouring inharmonic partials.  The stack with
//! `c = 1` uses an exact 1:1 ratio (its lower sidebands fold back onto the
//! harmonics, the classic FM piano/e-piano spectrum).
//!
//! Amplitude envelopes are two decaying exponentials (the piano's "prompt
//! sound" and "aftersound"), rendered on two slightly detuned carriers so the
//! hand-over between them produces the beating of real unison strings.  The
//! modulation index decays from `I0` to `I∞`, which makes the upper partials
//! die away faster than the lower ones.
//!
//! All envelope/index parameters were fitted to the Salamander Grand Piano V3
//! recordings (see `tools/fit`); they live in [`crate::tables`] for 30 keys ×
//! 16 velocity layers and are interpolated here for any key and velocity.

use crate::tables::{NoteFit, LAYERS, MAX_STACKS, NOTES};

/// Global tone controls; the presets are combinations of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// Multiplier on every modulation index (FM brightness).
    pub brightness: f32,
    /// Extra brightness that grows with velocity (hammer felt hardness).
    pub hardness: f32,
    /// Spectral tilt in dB/octave applied to stacks (re. 1 kHz).
    pub tilt: f32,
    /// Multiplier on all decay times.
    pub decay: f32,
    /// Detuning between the two carrier copies in cents (unison beating).
    pub detune: f32,
    /// Multiplier on the measured octave stretch (0 = equal temperament).
    pub stretch: f32,
    /// Multiplier on string inharmonicity B.
    pub inharm: f32,
    /// Hammer knock level multiplier.
    pub hammer: f32,
    /// Damper release time multiplier.
    pub release: f32,
    /// Velocity curve exponent (<1 = easier to play loud).
    pub vel_curve: f32,
    /// Output gain of the preset.
    pub gain: f32,
}

impl Tone {
    pub const GRAND: Tone = Tone {
        brightness: 1.0,
        hardness: 0.0,
        tilt: 0.0,
        decay: 1.0,
        detune: 1.6,
        stretch: 1.0,
        inharm: 1.0,
        hammer: 1.0,
        release: 1.0,
        vel_curve: 1.0,
        gain: 1.0,
    };
}

/// Built-in tones.  "Concert Grand" is the model as fitted to the recordings;
/// the others are physically motivated variations of it.
pub const PRESETS: [(&str, Tone); 6] = [
    ("Concert Grand", Tone::GRAND),
    (
        "Bright Grand",
        Tone {
            brightness: 1.22,
            hardness: 0.35,
            tilt: 1.2,
            hammer: 1.5,
            gain: 0.9,
            ..Tone::GRAND
        },
    ),
    (
        "Mellow Grand",
        Tone {
            brightness: 0.78,
            hardness: -0.2,
            tilt: -1.8,
            hammer: 0.55,
            decay: 1.1,
            gain: 1.15,
            ..Tone::GRAND
        },
    ),
    (
        "Upright",
        Tone {
            brightness: 0.95,
            tilt: -0.6,
            decay: 0.55,
            detune: 2.6,
            inharm: 2.2,
            stretch: 1.4,
            hammer: 1.6,
            release: 0.7,
            gain: 1.1,
            ..Tone::GRAND
        },
    ),
    (
        "Honky-tonk",
        Tone {
            brightness: 1.12,
            tilt: 0.8,
            decay: 0.65,
            detune: 16.0,
            inharm: 1.6,
            hammer: 1.4,
            gain: 0.95,
            ..Tone::GRAND
        },
    ),
    (
        "Felt Piano",
        Tone {
            brightness: 0.5,
            hardness: -0.35,
            tilt: -4.0,
            decay: 0.8,
            detune: 1.2,
            hammer: 0.35,
            release: 1.6,
            vel_curve: 1.2,
            gain: 1.5,
            ..Tone::GRAND
        },
    ),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackKind {
    /// One carrier, envelope = A1 e^{-t/τ1} + A2 e^{-t/τ2}.
    #[default]
    Single,
    /// Two detuned carriers (prompt/aftersound) sharing one modulator.
    Dual,
    /// Two detuned carrier+modulator pairs (needed for the exact 1:1 stack,
    /// otherwise the detuned copy would put a sideband near DC).
    DualMod,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StackSpec {
    pub kind: StackKind,
    /// Carrier and modulator frequency (Hz).
    pub fc: f64,
    pub fm: f64,
    pub a1: f32,
    pub a2: f32,
    pub tau1: f32,
    pub tau2: f32,
    pub i0: f32,
    pub iinf: f32,
    pub taui: f32,
}

/// A noisy FM pair: the modulator feeds back on itself (chaotic for
/// `fb` > ~1.2), and drives the carrier with a decaying index.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoiseFm {
    pub fc: f64,
    pub fm: f64,
    pub fb: f32,
    pub i0: f32,
    pub taui: f32,
    pub amp: f32,
    pub tau: f32,
    /// slower second decay stage (soundboard/case ring-out)
    pub tail_amp: f32,
    pub tail_tau: f32,
    /// build-up time constant of the first stage
    pub rise: f32,
    /// 2-pole low-pass cutoff on the output (Hz), 0 = none
    pub lowpass: f32,
}

/// Parameters of the hammer-noise model.  Levels are absolute (same scale as
/// the fitted stack amplitudes) for a fortissimo strike of C8; they fall
/// towards the bass and with softer playing.  Measured on the recordings in
/// the bands below the fundamental of the top two octaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KnockModel {
    pub thump_db: f32,
    pub thump_key_db: f32,
    pub thump_vel_db: f32,
    pub thump_tau: f32,
    pub tail_rel_db: f32,
    pub tail_tau: f32,
    pub thump_fc: f32,
    pub thump_fm: f32,
    pub thump_i0: f32,
    pub thump_rise: f32,
    pub thump_lp: f32,
    pub click_db: f32,
    pub click_key_db: f32,
    pub click_vel_db: f32,
    pub click_tau: f32,
    pub click_fc: f32,
    pub click_fm: f32,
    pub click_i0: f32,
    /// "halo": the dense band of soundboard/sympathetic resonance that
    /// surrounds the fundamental of treble notes (frequencies are ratios to
    /// the note's fundamental)
    pub halo_db: f32,
    pub halo_key_db: f32,
    pub halo_vel_db: f32,
    pub halo_tau: f32,
    pub halo_fc: f32,
    pub halo_fm: f32,
    pub halo_i0: f32,
    pub halo_lp: f32,
}

impl Default for KnockModel {
    /// Values from coordinate searches (tools/fit/tune_knock.py): thump and
    /// click against the recordings' third-octave bands below and between
    /// the partials of C5..C8, the halo against the full ERB-band error of
    /// A5..C8.
    fn default() -> Self {
        KnockModel {
            thump_db: -22.7,
            thump_key_db: 9.2,
            thump_vel_db: 10.1,
            thump_tau: 0.104,
            tail_rel_db: -24.2,
            tail_tau: 0.98,
            thump_fc: 48.6,
            thump_fm: 214.6,
            thump_i0: 15.7,
            thump_rise: 0.0038,
            thump_lp: 920.0,
            click_db: -26.0,
            click_key_db: 10.0,
            click_vel_db: 15.0,
            click_tau: 0.0049,
            click_fc: 2250.0,
            click_fm: 900.0,
            click_i0: 3.0,
            halo_db: -22.2,
            halo_key_db: 22.6,
            halo_vel_db: 22.0,
            halo_tau: 0.114,
            halo_fc: 1.15,
            halo_fm: 0.19,
            halo_i0: 4.1,
            halo_lp: 2.7,
        }
    }
}

/// Hammer impact: a short broadband "click" and the low "thump" of the
/// action and soundboard, which dominates the attack of the top octaves.
#[derive(Clone, Copy, Debug, Default)]
pub struct KnockSpec {
    pub click: NoiseFm,
    pub thump: NoiseFm,
    pub halo: NoiseFm,
}

#[derive(Clone, Copy, Debug)]
pub struct VoiceSpec {
    pub stacks: [StackSpec; MAX_STACKS],
    pub n: usize,
    /// Frequency ratio of the second carrier copy.
    pub detune_ratio: f64,
    /// Attack (hammer contact) time constant, seconds.
    pub attack: f32,
    /// Damper time constant after key release, seconds.
    pub release: f32,
    /// False for the top keys, which have no dampers.
    pub damped: bool,
    pub knock: KnockSpec,
    /// -1 (left) .. 1 (right)
    pub pan: f32,
    /// Fundamental (partial 1) frequency in Hz.
    pub f1: f64,
}

/// Index cap that keeps a stretched stack's lowest sideband away from DC.
pub fn index_cap(c: u8) -> f32 {
    if c == 1 {
        3.0
    } else {
        (0.55 * c as f32).clamp(0.3, 4.0)
    }
}

fn bracket(key: f32) -> (usize, f32) {
    let last = NOTES.len() - 1;
    if key <= NOTES[0].midi as f32 {
        return (0, 0.0);
    }
    if key >= NOTES[last].midi as f32 {
        return (last - 1, 1.0);
    }
    let mut i = 0;
    while i + 1 < last && NOTES[i + 1].midi as f32 <= key {
        i += 1;
    }
    let (m0, m1) = (NOTES[i].midi as f32, NOTES[i + 1].midi as f32);
    (i, (key - m0) / (m1 - m0))
}

fn lerp(a: f32, b: f32, w: f32) -> f32 {
    a + (b - a) * w
}

/// Tuning (cents re. 12-TET, A4 = 440 Hz) and inharmonicity of a key, from
/// the values measured on the recorded piano.
pub fn tuning(key: f32) -> (f32, f32) {
    let (i, w) = bracket(key);
    let (a, b) = (&NOTES[i], &NOTES[i + 1]);
    let cents = lerp(a.cents, b.cents, w);
    let lb = lerp(a.b.ln(), b.b.ln(), w);
    (cents, lb.exp())
}

/// Stretched partial frequency f_n = n f0 sqrt(1 + B n²).
#[inline]
pub fn partial(n: f64, f0: f64, b: f64) -> f64 {
    n * f0 * (1.0 + b * n * n).sqrt()
}

/// Map MIDI velocity to a fractional velocity layer (0..15) plus an extra
/// gain used below the softest recorded layer.
pub fn velocity_layer(vel: f32, tone: &Tone) -> (f32, f32) {
    let v = 127.0 * (vel.clamp(1.0, 127.0) / 127.0).powf(tone.vel_curve);
    const V_LO: f32 = 8.0;
    if v < V_LO {
        (0.0, (v / V_LO).powf(1.4))
    } else {
        ((v - V_LO) / (127.0 - V_LO) * (LAYERS as f32 - 1.0), 1.0)
    }
}

fn layer_params(note: &NoteFit, layer: f32, s: usize) -> [f32; 7] {
    let l0 = (layer.floor() as usize).min(LAYERS - 1);
    let l1 = (l0 + 1).min(LAYERS - 1);
    let w = layer - l0 as f32;
    let (p, q) = (&note.p[l0][s], &note.p[l1][s]);
    let mut o = [0.0f32; 7];
    for k in 0..7 {
        o[k] = if k == 2 || k == 3 || k == 6 {
            lerp(p[k].ln(), q[k].ln(), w).exp()
        } else {
            lerp(p[k], q[k], w)
        };
    }
    o
}

/// Interpolated fitted stacks for a key and velocity layer, as
/// (centre partial, [A1, A2, τ1, τ2, I0, I∞, τI]) in ascending centre order.
/// The two neighbouring recorded keys may use different stack layouts: stacks
/// are matched by centre, and a stack present on one side only fades in/out.
fn fitted(key: f32, layer: f32) -> ([(u8, [f32; 7]); MAX_STACKS], usize) {
    let (i, w) = bracket(key);
    let (na, nb) = (&NOTES[i], &NOTES[i + 1]);
    let ca = &na.centers[..na.n_stacks as usize];
    let cb = &nb.centers[..nb.n_stacks as usize];
    let mut out = [(0u8, [0.0f32; 7]); MAX_STACKS];
    let mut n = 0;
    let (mut ia, mut ib) = (0, 0);
    while (ia < ca.len() || ib < cb.len()) && n < MAX_STACKS {
        let a = ca.get(ia).copied().unwrap_or(u8::MAX);
        let b = cb.get(ib).copied().unwrap_or(u8::MAX);
        let item = if a == b {
            let (p, q) = (layer_params(na, layer, ia), layer_params(nb, layer, ib));
            let mut o = [0.0f32; 7];
            for k in 0..7 {
                o[k] = if k == 2 || k == 3 || k == 6 {
                    lerp(p[k].ln(), q[k].ln(), w).exp()
                } else {
                    lerp(p[k], q[k], w)
                };
            }
            ia += 1;
            ib += 1;
            (a, o)
        } else if a < b {
            let mut p = layer_params(na, layer, ia);
            p[0] *= 1.0 - w;
            p[1] *= 1.0 - w;
            ia += 1;
            (a, p)
        } else {
            let mut q = layer_params(nb, layer, ib);
            q[0] *= w;
            q[1] *= w;
            ib += 1;
            (b, q)
        };
        // drop stacks that are silent at this interpolation point
        if item.1[0] + item.1[1] > 0.0 {
            out[n] = item;
            n += 1;
        }
    }
    (out, n)
}

/// Compute all parameters of a voice.
pub fn voice_spec(key: u8, vel: f32, tone: &Tone, sample_rate: f32) -> VoiceSpec {
    voice_spec_with(key, vel, tone, &KnockModel::default(), sample_rate)
}

/// [`voice_spec`] with an explicit hammer-noise model.
pub fn voice_spec_with(
    key: u8,
    vel: f32,
    tone: &Tone,
    km: &KnockModel,
    sample_rate: f32,
) -> VoiceSpec {
    let kf = key as f32;
    let sr = sample_rate as f64;
    let (cents, b0) = tuning(kf);
    let b = (b0 * tone.inharm) as f64;
    let f1 = 440.0 * 2f64.powf((kf as f64 - 69.0) / 12.0 + (cents * tone.stretch) as f64 / 1200.0);
    let f0 = f1 / (1.0 + b).sqrt();
    let (layer, low_gain) = velocity_layer(vel, tone);
    let vnorm = layer / (LAYERS as f32 - 1.0); // 0..1
    let bright = (tone.brightness * (1.0 + tone.hardness * (vnorm - 0.5))).max(0.0);
    let nyq = 0.45 * sr;

    let mut stacks = [StackSpec::default(); MAX_STACKS];
    let mut n = 0;
    let (fit, nfit) = fitted(kf, layer);
    for &(c, p) in &fit[..nfit] {
        let cf = c as f64;
        let fc = partial(cf, f0, b);
        let fm = if c == 1 {
            fc
        } else {
            0.5 * (partial(cf + 1.0, f0, b) - partial(cf - 1.0, f0, b))
        };
        if fc >= nyq {
            break;
        }
        let cap = index_cap(c) * 1.25;
        let mut i0 = (p[4] * bright).min(cap);
        let mut iinf = (p[5] * bright).min(cap);
        // keep the significant sidebands (|j| <= I + 2) below Nyquist
        let room = ((nyq - fc) / fm - 2.0).max(0.0) as f32;
        i0 = i0.min(room);
        iinf = iinf.min(room);
        let tilt = 10f32.powf(tone.tilt * (fc as f32 / 1000.0).log2() / 20.0);
        let g = low_gain * tilt;
        stacks[n] = StackSpec {
            kind: match c {
                1 => StackKind::DualMod,
                2..=4 => StackKind::Dual,
                _ => StackKind::Single,
            },
            fc,
            fm,
            a1: p[0] * g,
            a2: p[1] * g,
            tau1: p[2] * tone.decay,
            tau2: p[3] * tone.decay,
            i0,
            iinf,
            taui: p[6] * tone.decay,
        };
        n += 1;
    }

    // hammer knock (absolute levels, see KnockModel)
    let kn = (kf - 21.0) / 87.0; // 0 (A0) .. 1 (C8)
    let vel_db = 20.0 * low_gain.log10();
    let thump_db =
        km.thump_db - km.thump_key_db * (1.0 - kn) + km.thump_vel_db * (vnorm - 1.0) + vel_db;
    let click_db =
        km.click_db - km.click_key_db * (1.0 - kn) + km.click_vel_db * (vnorm - 1.0) + vel_db;
    let halo_db =
        km.halo_db - km.halo_key_db * (1.0 - kn) + km.halo_vel_db * (vnorm - 1.0) + vel_db;
    let h = tone.hammer;
    let thump_amp = h * 10f32.powf(thump_db / 20.0);
    let knock = KnockSpec {
        thump: NoiseFm {
            fc: km.thump_fc as f64,
            fm: km.thump_fm as f64,
            fb: 1.6,
            i0: km.thump_i0 * (0.6 + 0.4 * vnorm),
            taui: 0.25,
            amp: thump_amp,
            tau: km.thump_tau,
            tail_amp: thump_amp * 10f32.powf(km.tail_rel_db / 20.0),
            tail_tau: km.tail_tau * tone.decay,
            rise: km.thump_rise,
            lowpass: km.thump_lp,
        },
        click: NoiseFm {
            fc: km.click_fc as f64,
            fm: km.click_fm as f64,
            fb: 1.5,
            i0: km.click_i0 * (0.5 + 0.5 * vnorm),
            taui: 0.004,
            amp: h * 10f32.powf(click_db / 20.0),
            tau: km.click_tau,
            tail_amp: 0.0,
            tail_tau: 0.01,
            rise: 0.0,
            lowpass: 0.0,
        },
        halo: NoiseFm {
            fc: km.halo_fc as f64 * f1,
            fm: km.halo_fm as f64 * f1,
            fb: 1.5,
            i0: km.halo_i0,
            taui: 1.0,
            amp: h * 10f32.powf(halo_db / 20.0),
            tau: km.halo_tau * tone.decay,
            tail_amp: 0.0,
            tail_tau: 0.01,
            rise: 0.002,
            lowpass: (km.halo_lp as f64 * f1).min(0.4 * sr) as f32,
        },
    };

    VoiceSpec {
        stacks,
        n,
        detune_ratio: 2f64.powf(tone.detune as f64 / 1200.0),
        attack: 0.0006 + 0.0022 * (1.0 - kn),
        release: (0.035 + 0.22 * (1.0 - kn) * (1.0 - kn)) * tone.release,
        damped: key < 90,
        knock,
        pan: (kf - 64.0) / 64.0 * 0.4,
        f1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_is_monotone_ish_and_a4_close() {
        let (c, b) = tuning(69.0);
        assert!(c.abs() < 5.0, "A4 cents {c}");
        assert!(b > 1e-4 && b < 2e-3, "A4 B {b}");
    }

    #[test]
    fn spec_has_stacks_everywhere() {
        for key in 21..=108u8 {
            for vel in [1.0f32, 40.0, 80.0, 127.0] {
                let v = voice_spec(key, vel, &Tone::GRAND, 48000.0);
                assert!(v.n >= 1, "key {key} vel {vel}");
                for s in &v.stacks[..v.n] {
                    assert!(s.fc < 0.45 * 48000.0);
                    assert!(s.a1.is_finite() && s.a2.is_finite());
                    assert!(s.tau1 > 0.0 && s.tau2 > 0.0 && s.taui > 0.0);
                }
            }
        }
    }
}
