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
//! sound" and "aftersound").  On the lowest partials they are rendered as the
//! normal modes of the note's unison strings: a single carrier for the
//! single-string bass notes, a prompt and an aftersound carrier half the
//! unison spread either side of the pitch for string pairs, and a prompt plus
//! two aftersound carriers for string triples.  They start in phase, like
//! strings struck by one hammer, and drift apart, which produces the beating
//! of real unisons.  The modulation index decays from `I0` to `I∞`, which
//! makes the upper partials die away faster than the lower ones.
//!
//! All envelope/index parameters were fitted to the Salamander Grand Piano V3
//! recordings (see `tools/fit`); they live in [`crate::tables`] for 30 keys ×
//! 16 velocity layers and are interpolated here for any key and velocity.

use crate::ideal::IdealParams;
use crate::tables::{NoteFit, LAYERS, NOTES};

/// Global tone controls; the presets are combinations of these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// Multiplier on every modulation index (FM brightness).
    pub brightness: f32,
    /// Extra brightness that grows with velocity (hammer felt hardness).
    pub hardness: f32,
    /// Spectral tilt in dB/octave applied to stacks (re. the fundamental).
    pub tilt: f32,
    /// Multiplier on all decay times.
    pub decay: f32,
    /// Multiplier on the measured detuning between a note's unison strings
    /// (beating speed); 0 = perfectly tuned unisons.
    pub detune: f32,
    /// Strings per note: 0 = as on the recorded grand (1 / 2 / 3 by
    /// register, see [`unison_strings`]); 1..=3 forces that many everywhere.
    pub strings: u8,
    /// How evenly the sound is shared between the unison strings: 0 = the
    /// prompt sound on one string and the aftersound on the other(s) (beating
    /// only while one hands over to the other), 1 = every string carries an
    /// equal share all the time (deep beating throughout).
    pub unison_mix: f32,
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
        detune: 1.0,
        strings: 0,
        unison_mix: 0.0,
        stretch: 1.0,
        inharm: 1.0,
        hammer: 1.0,
        release: 1.0,
        vel_curve: 1.0,
        gain: 1.0,
    };
}

/// Which voice generator a preset uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Stack envelopes fitted key by key to the recorded grand.
    Fitted,
    /// Smooth physical laws over the whole keyboard ([`crate::ideal`]).
    Ideal,
}

pub struct Preset {
    pub name: &'static str,
    pub engine: Engine,
    pub tone: Tone,
    pub ideal: IdealParams,
}

const fn fitted_preset(name: &'static str, tone: Tone) -> Preset {
    Preset {
        name,
        engine: Engine::Fitted,
        tone,
        ideal: IdealParams::DEFAULT,
    }
}

const fn ideal_preset(name: &'static str, tone: Tone, ideal: IdealParams) -> Preset {
    Preset {
        name,
        engine: Engine::Ideal,
        tone,
        ideal,
    }
}

/// Built-in tones.  "Concert Grand" is the model as fitted to the recordings
/// and the next five are physically motivated variations of it; the "Ideal"
/// tones use the idealised engine.
pub const PRESETS: [Preset; 11] = [
    fitted_preset("Concert Grand", Tone::GRAND),
    fitted_preset(
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
    fitted_preset(
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
    fitted_preset(
        "Upright",
        Tone {
            brightness: 0.95,
            tilt: -0.6,
            decay: 0.55,
            detune: 1.6,
            inharm: 2.2,
            stretch: 1.4,
            hammer: 1.6,
            release: 0.7,
            gain: 1.3,
            ..Tone::GRAND
        },
    ),
    fitted_preset(
        "Honky-tonk",
        Tone {
            brightness: 1.12,
            tilt: 0.8,
            decay: 0.65,
            detune: 10.0,
            inharm: 1.6,
            hammer: 1.4,
            gain: 1.12,
            ..Tone::GRAND
        },
    ),
    fitted_preset(
        "Felt Piano",
        Tone {
            brightness: 0.5,
            hardness: -0.35,
            tilt: -4.0,
            decay: 0.8,
            detune: 0.75,
            hammer: 0.35,
            release: 1.6,
            vel_curve: 1.2,
            gain: 1.5,
            ..Tone::GRAND
        },
    ),
    ideal_preset("Ideal Grand", Tone::GRAND, IdealParams::DEFAULT),
    // a bigger instrument: longer, less stiff strings, a larger soundboard
    // (stronger bass fundamentals) and a longer singing aftersound
    ideal_preset(
        "Ideal Concert Grand",
        Tone {
            inharm: 0.7,
            decay: 1.25,
            brightness: 1.05,
            gain: 0.88,
            ..Tone::GRAND
        },
        IdealParams {
            strike: 1.0 / 8.0,
            board: 0.7,
            high_damp: 0.85,
            aftersound: 1.4,
            ..IdealParams::DEFAULT
        },
    ),
    // hard hammers, shallow strike notches
    ideal_preset(
        "Ideal Bright Grand",
        Tone {
            brightness: 1.35,
            hardness: 0.3,
            tilt: 0.8,
            hammer: 1.4,
            gain: 0.83,
            ..Tone::GRAND
        },
        IdealParams {
            notch: 0.3,
            ..IdealParams::DEFAULT
        },
    ),
    // short, stiff strings, a small soundboard (thin bass), more string loss
    ideal_preset(
        "Ideal Upright",
        Tone {
            inharm: 2.2,
            stretch: 1.3,
            decay: 0.6,
            detune: 1.6,
            hammer: 1.5,
            release: 0.8,
            gain: 1.42,
            ..Tone::GRAND
        },
        IdealParams {
            strike: 1.0 / 9.0,
            board: 1.8,
            high_damp: 1.3,
            notch: 0.6,
            ..IdealParams::DEFAULT
        },
    ),
    // soft felt between hammer and strings: dull, damped highs, small dynamics
    ideal_preset(
        "Ideal Felt",
        Tone {
            brightness: 0.45,
            hardness: -0.4,
            tilt: -3.0,
            decay: 0.85,
            hammer: 0.4,
            vel_curve: 1.15,
            gain: 1.6,
            ..Tone::GRAND
        },
        IdealParams {
            dynamics: 0.8,
            notch: 0.2,
            high_damp: 2.5,
            ..IdealParams::DEFAULT
        },
    ),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackKind {
    /// One carrier, envelope = A1 e^{-t/τ1} + A2 e^{-t/τ2}.
    #[default]
    Single,
    /// Two detuned carriers (prompt/aftersound of a string pair) sharing one
    /// modulator.
    Dual,
    /// Two detuned carrier+modulator pairs (needed for the exact 1:1 stack,
    /// otherwise a detuned copy would put a sideband near DC).
    DualMod,
    /// Prompt plus two aftersound carriers (string triple), one modulator.
    Triple,
    /// Three detuned carrier+modulator pairs (1:1 stack of a string triple).
    TripleMod,
}

/// Strings per key on the recorded Yamaha C5.  `tools/analysis/unison.py`
/// resolves the partials into their components: A0, C1 and D#1 have a single
/// component (single strings) while F#1 and everything above show several
/// within a cent or two.  Where pairs give way to triples cannot be told
/// apart in the recordings, so the usual layout of a grand this size is used:
/// single strings A0–E1, pairs F1–E2, triples from F2.
pub fn unison_strings(key: u8) -> u8 {
    match key {
        0..=28 => 1,
        29..=40 => 2,
        _ => 3,
    }
}

/// Measured frequency spread (cents) between the unison components of a
/// partial: about 1 cent up to C5, rising to about 2.4 cents from F#6.
pub fn unison_spread(key: f32) -> f32 {
    let t = ((key - 72.0) / (90.0 - 72.0)).clamp(0.0, 1.0);
    1.0 + 1.4 * t
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
    /// Starting phase of the carrier(s), radians.
    pub phase: f32,
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
    pub stacks: [StackSpec; VOICE_STACKS],
    pub n: usize,
    /// Strings sounding for this note (1..=3).
    pub strings: u8,
    /// Frequency ratios of the prompt carrier and the aftersound carrier(s).
    pub unison: [f64; 3],
    /// Share of the prompt (`[0]`) and aftersound (`[1]`) amplitude carried
    /// by each of the (up to three) string copies; each row sums to 1 so the
    /// copies add up to the fitted envelope while they are still in phase.
    pub weights: [[f32; 2]; 3],
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

/// Most stacks a voice can have: the fitted tables use up to
/// `tables::MAX_STACKS` per recorded key (two neighbouring keys can need more
/// between them), the idealised engine up to 24.
pub const VOICE_STACKS: usize = 24;

/// Envelope of one stack before the tone controls are applied:
/// centre partial `c` and `p` = [A1, A2, τ1, τ2, I0, I∞, τI].
#[derive(Clone, Copy, Debug, Default)]
pub struct StackEnv {
    pub c: u8,
    pub p: [f32; 7],
}

/// Interpolated fitted stacks for a key and velocity layer, in ascending
/// centre order.  The two neighbouring recorded keys may use different stack
/// layouts: stacks are matched by centre, and a stack present on one side
/// only fades in/out.
fn fitted(key: f32, layer: f32) -> ([StackEnv; VOICE_STACKS], usize) {
    let (i, w) = bracket(key);
    let (na, nb) = (&NOTES[i], &NOTES[i + 1]);
    let ca = &na.centers[..na.n_stacks as usize];
    let cb = &nb.centers[..nb.n_stacks as usize];
    let mut out = [StackEnv::default(); VOICE_STACKS];
    let mut n = 0;
    let (mut ia, mut ib) = (0, 0);
    while (ia < ca.len() || ib < cb.len()) && n < VOICE_STACKS {
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
            StackEnv { c: a, p: o }
        } else if a < b {
            let mut p = layer_params(na, layer, ia);
            p[0] *= 1.0 - w;
            p[1] *= 1.0 - w;
            ia += 1;
            StackEnv { c: a, p }
        } else {
            let mut q = layer_params(nb, layer, ib);
            q[0] *= w;
            q[1] *= w;
            ib += 1;
            StackEnv { c: b, p: q }
        };
        // drop stacks that are silent at this interpolation point
        if item.p[0] + item.p[1] > 0.0 {
            out[n] = item;
            n += 1;
        }
    }
    (out, n)
}

/// Compute all parameters of a voice (fitted engine).
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
    let (cents, b0) = tuning(kf);
    let b = (b0 * tone.inharm) as f64;
    let f1 = 440.0 * 2f64.powf((kf as f64 - 69.0) / 12.0 + (cents * tone.stretch) as f64 / 1200.0);
    let f0 = f1 / (1.0 + b).sqrt();
    let (layer, low_gain) = velocity_layer(vel, tone);
    let vnorm = layer / (LAYERS as f32 - 1.0); // 0..1
    let bright = (tone.brightness * (1.0 + tone.hardness * (vnorm - 0.5))).max(0.0);
    let (envs, n) = fitted(kf, layer);
    let shape = VoiceShape {
        f0,
        b,
        f1,
        vnorm,
        stack_gain: low_gain,
        knock_db: 20.0 * low_gain.log10(),
        index_scale: bright,
        spread_phases: false,
    };
    assemble(key, tone, km, sample_rate, &shape, &envs[..n])
}

/// Pitch, dynamics and scaling handed from an engine to [`assemble`].
const GOLDEN_ANGLE: f32 = 2.399_963;

pub struct VoiceShape {
    /// String fundamental (without inharmonic stretch) and inharmonicity.
    pub f0: f64,
    pub b: f64,
    /// Frequency of partial 1.
    pub f1: f64,
    /// Velocity on the 0..1 scale of the recorded layers (for the knock).
    pub vnorm: f32,
    /// Extra gain on all stacks, and on the knock in dB.
    pub stack_gain: f32,
    pub knock_db: f32,
    /// Multiplier on every modulation index.
    pub index_scale: f32,
    /// Start the stacks' carriers at golden-angle phases instead of all at 0.
    /// Neighbouring stacks put sidebands on the same partials; with equal
    /// phases a lower sideband (J_-1 = -J_1) cancels the upper sideband of the
    /// stack below at the attack, while spread phases make their powers add,
    /// as the idealised engine's moment matching assumes.
    pub spread_phases: bool,
}

/// Final stage shared by both engines: turns stack envelopes into carrier and
/// modulator frequencies, applies the tone controls (tilt, sustain, unison
/// strings) and adds hammer noise, dampers and panning.
pub fn assemble(
    key: u8,
    tone: &Tone,
    km: &KnockModel,
    sample_rate: f32,
    sh: &VoiceShape,
    envs: &[StackEnv],
) -> VoiceSpec {
    let kf = key as f32;
    let sr = sample_rate as f64;
    let (f0, b, f1) = (sh.f0, sh.b, sh.f1);
    let vnorm = sh.vnorm;
    let nyq = 0.45 * sr;

    // unison strings: prompt carrier and aftersound carrier(s), placed so the
    // spread between them is the measured unison spread (times the control)
    let strings = if tone.strings == 0 {
        unison_strings(key)
    } else {
        tone.strings.clamp(1, 3)
    };
    let spread = (unison_spread(kf) * tone.detune.max(0.0)) as f64;
    let ratio = |cents: f64| 2f64.powf(cents / 1200.0);
    let unison = match strings {
        1 => [1.0, 1.0, 1.0],
        2 => [ratio(-0.5 * spread), ratio(0.5 * spread), 1.0],
        _ => [1.0, ratio(0.55 * spread), ratio(-0.45 * spread)],
    };
    // share of prompt / aftersound per string copy: blend from "prompt on
    // string 1, aftersound mostly on string 2" (closest to the recordings in
    // tools/analysis/compare.py) to "equal shares" (deepest beating)
    let mix = tone.unison_mix.clamp(0.0, 1.0);
    let (p0, q0, even): ([f32; 3], [f32; 3], [f32; 3]) = match strings {
        1 => ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
        2 => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.5, 0.5, 0.0]),
        _ => ([1.0, 0.0, 0.0], [0.0, 0.85, 0.15], [0.36, 0.34, 0.30]),
    };
    let mut weights = [[0.0f32; 2]; 3];
    for k in 0..3 {
        weights[k] = [
            p0[k] + mix * (even[k] - p0[k]),
            q0[k] + mix * (even[k] - q0[k]),
        ];
    }

    let mut stacks = [StackSpec::default(); VOICE_STACKS];
    let mut n = 0;
    for e in envs.iter().take(VOICE_STACKS) {
        let (c, p) = (e.c, e.p);
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
        let mut i0 = (p[4] * sh.index_scale).min(cap);
        let mut iinf = (p[5] * sh.index_scale).min(cap);
        // keep the significant sidebands (|j| <= I + 2) below Nyquist
        let room = ((nyq - fc) / fm - 2.0).max(0.0) as f32;
        i0 = i0.min(room);
        iinf = iinf.min(room);
        // tilt pivots on the fundamental: it reshapes the spectrum without
        // changing the level of partial 1
        let tilt = 10f32.powf(tone.tilt * (fc / f1).log2() as f32 / 20.0);
        let g = sh.stack_gain * tilt;
        stacks[n] = StackSpec {
            kind: match (strings, c) {
                (1, _) | (_, 5..) => StackKind::Single,
                (2, 1) => StackKind::DualMod,
                (2, _) => StackKind::Dual,
                (_, 1) => StackKind::TripleMod,
                _ => StackKind::Triple,
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
            phase: if sh.spread_phases {
                n as f32 * GOLDEN_ANGLE
            } else {
                0.0
            },
        };
        n += 1;
    }

    // hammer knock (absolute levels, see KnockModel)
    let kn = (kf - 21.0) / 87.0; // 0 (A0) .. 1 (C8)
    let vel_db = sh.knock_db;
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
        strings,
        unison,
        weights,
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
    fn unison_layout() {
        assert_eq!(unison_strings(21), 1); // A0
        assert_eq!(unison_strings(28), 1); // E1
        assert_eq!(unison_strings(29), 2); // F1
        assert_eq!(unison_strings(40), 2); // E2
        assert_eq!(unison_strings(41), 3); // F2
        assert_eq!(unison_strings(108), 3);
        let t = Tone::GRAND;
        assert_eq!(voice_spec(21, 80.0, &t, 48000.0).strings, 1);
        assert_eq!(voice_spec(33, 80.0, &t, 48000.0).strings, 2);
        assert_eq!(voice_spec(60, 80.0, &t, 48000.0).strings, 3);
        let forced = Tone { strings: 2, ..t };
        assert_eq!(voice_spec(21, 80.0, &forced, 48000.0).strings, 2);
        // single strings: no beating copies at all
        let v = voice_spec(21, 80.0, &t, 48000.0);
        assert!(v.stacks[..v.n].iter().all(|s| s.kind == StackKind::Single));
        // triples: prompt + two aftersound carriers on the lowest stacks
        let v = voice_spec(60, 80.0, &t, 48000.0);
        assert_eq!(v.stacks[0].kind, StackKind::TripleMod);
        assert_eq!(v.stacks[1].kind, StackKind::Triple);
        let spread = 1200.0 * (v.unison[1] / v.unison[2]).log2();
        assert!((spread - unison_spread(60.0) as f64).abs() < 1e-6);
        // zero detune -> all copies in unison
        let v = voice_spec(60, 80.0, &Tone { detune: 0.0, ..t }, 48000.0);
        assert!(v.unison.iter().all(|&r| (r - 1.0).abs() < 1e-12));
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
