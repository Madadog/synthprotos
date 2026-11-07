//! Polyphonic engine: voice allocation, sustain pedal, mixing and effects.

use crate::model::{voice_spec_with, KnockModel, Tone, PRESETS};
use crate::reverb::Reverb;
use crate::sine::SineTable;
use crate::voice::Voice;

pub const BLOCK: usize = 128;
pub const DEFAULT_VOICES: usize = 40;

/// Parameter ids for [`Synth::set_param`] / [`Synth::param`].
pub mod param {
    pub const VOLUME: u32 = 0;
    pub const REVERB: u32 = 1;
    pub const BRIGHTNESS: u32 = 2;
    pub const HARDNESS: u32 = 3;
    pub const TILT: u32 = 4;
    pub const DECAY: u32 = 5;
    pub const DETUNE: u32 = 6;
    pub const STRETCH: u32 = 7;
    pub const INHARM: u32 = 8;
    pub const HAMMER: u32 = 9;
    pub const RELEASE: u32 = 10;
    pub const VEL_CURVE: u32 = 11;
    pub const WIDTH: u32 = 12;
    pub const COUNT: u32 = 13;
}

struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.9995 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    const K: f32 = 0.85;
    let a = x.abs();
    if a <= K {
        x
    } else {
        let over = (a - K) / (1.0 - K);
        let y = K + (1.0 - K) * over / (1.0 + over * over).sqrt();
        y.copysign(x)
    }
}

pub struct Synth {
    sr: f32,
    tab: SineTable,
    voices: Vec<Voice>,
    serial: u64,
    tone: Tone,
    knock: KnockModel,
    preset: usize,
    sustain: bool,
    reverb: Reverb,
    reverb_mix: f32,
    volume: f32,
    width: f32,
    vbuf: [f32; BLOCK],
    dry_l: [f32; BLOCK],
    dry_r: [f32; BLOCK],
    dc_l: DcBlock,
    dc_r: DcBlock,
}

impl Synth {
    pub fn new(sample_rate: f32) -> Self {
        Self::with_voices(sample_rate, DEFAULT_VOICES)
    }

    pub fn with_voices(sample_rate: f32, voices: usize) -> Self {
        Synth {
            sr: sample_rate,
            tab: SineTable::new(),
            voices: (0..voices.max(1))
                .map(|_| Voice::idle(sample_rate))
                .collect(),
            serial: 0,
            tone: PRESETS[0].1,
            knock: KnockModel::default(),
            preset: 0,
            sustain: false,
            reverb: Reverb::new(sample_rate),
            reverb_mix: 0.18,
            volume: 0.6,
            width: 1.0,
            vbuf: [0.0; BLOCK],
            dry_l: [0.0; BLOCK],
            dry_r: [0.0; BLOCK],
            dc_l: DcBlock { x1: 0.0, y1: 0.0 },
            dc_r: DcBlock { x1: 0.0, y1: 0.0 },
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    pub fn tone(&self) -> &Tone {
        &self.tone
    }

    pub fn set_tone(&mut self, tone: Tone) {
        self.tone = tone;
    }

    pub fn set_knock_model(&mut self, km: KnockModel) {
        self.knock = km;
    }

    pub fn preset(&self) -> usize {
        self.preset
    }

    pub fn set_preset(&mut self, idx: usize) {
        if let Some((_, t)) = PRESETS.get(idx) {
            self.preset = idx;
            self.tone = *t;
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    pub fn note_on(&mut self, key: u8, vel: u8) {
        self.note_on_f(key, vel as f32);
    }

    /// Note on with a fractional MIDI velocity (1.0..=127.0).
    pub fn note_on_f(&mut self, key: u8, vel: f32) {
        if vel <= 0.0 {
            self.note_off(key);
            return;
        }
        let key = key.clamp(21, 108);
        // re-strike: the hammer stops the old vibration of this string
        for v in self.voices.iter_mut() {
            if v.active && v.key == key {
                v.kill();
            }
        }
        let idx = self.allocate();
        let mut spec = voice_spec_with(key, vel, &self.tone, &self.knock, self.sr);
        spec.pan *= self.width;
        self.serial += 1;
        self.voices[idx].start(key, &spec, self.serial);
    }

    fn allocate(&mut self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        // steal: prefer released voices, then the quietest
        let mut best = 0;
        let mut best_score = f32::INFINITY;
        for (i, v) in self.voices.iter().enumerate() {
            let held = if v.key_down { 4.0 } else { 1.0 };
            let score = v.level() * held;
            if score < best_score {
                best_score = score;
                best = i;
            }
        }
        best
    }

    pub fn note_off(&mut self, key: u8) {
        let sustain = self.sustain;
        for v in self.voices.iter_mut() {
            if v.active && v.key == key && v.key_down {
                if sustain {
                    v.key_down = false;
                } else {
                    v.release();
                }
            }
        }
    }

    pub fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if !down {
            for v in self.voices.iter_mut() {
                if v.active && !v.key_down && !v.releasing {
                    v.release();
                }
            }
        }
    }

    pub fn sustain(&self) -> bool {
        self.sustain
    }

    pub fn all_notes_off(&mut self) {
        self.sustain = false;
        for v in self.voices.iter_mut() {
            if v.active {
                v.kill();
            }
        }
    }

    pub fn set_param(&mut self, id: u32, value: f32) {
        use param::*;
        let t = &mut self.tone;
        match id {
            VOLUME => self.volume = value.clamp(0.0, 2.0),
            REVERB => self.reverb_mix = value.clamp(0.0, 1.0),
            BRIGHTNESS => t.brightness = value.clamp(0.0, 3.0),
            HARDNESS => t.hardness = value.clamp(-1.0, 1.0),
            TILT => t.tilt = value.clamp(-12.0, 12.0),
            DECAY => t.decay = value.clamp(0.1, 4.0),
            DETUNE => t.detune = value.clamp(0.0, 40.0),
            STRETCH => t.stretch = value.clamp(0.0, 3.0),
            INHARM => t.inharm = value.clamp(0.0, 8.0),
            HAMMER => t.hammer = value.clamp(0.0, 4.0),
            RELEASE => t.release = value.clamp(0.1, 6.0),
            VEL_CURVE => t.vel_curve = value.clamp(0.3, 3.0),
            WIDTH => self.width = value.clamp(0.0, 2.0),
            _ => {}
        }
    }

    pub fn param(&self, id: u32) -> f32 {
        use param::*;
        let t = &self.tone;
        match id {
            VOLUME => self.volume,
            REVERB => self.reverb_mix,
            BRIGHTNESS => t.brightness,
            HARDNESS => t.hardness,
            TILT => t.tilt,
            DECAY => t.decay,
            DETUNE => t.detune,
            STRETCH => t.stretch,
            INHARM => t.inharm,
            HAMMER => t.hammer,
            RELEASE => t.release,
            VEL_CURVE => t.vel_curve,
            WIDTH => self.width,
            _ => 0.0,
        }
    }

    /// Render stereo audio.  Both slices must have the same length.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let mut done = 0;
        while done < n {
            let m = (n - done).min(BLOCK);
            self.render_block(&mut out_l[done..done + m], &mut out_r[done..done + m]);
            done += m;
        }
    }

    fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let m = out_l.len();
        let (dl, dr) = (&mut self.dry_l[..m], &mut self.dry_r[..m]);
        dl.fill(0.0);
        dr.fill(0.0);
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let buf = &mut self.vbuf[..m];
            v.render(&self.tab, buf);
            let (gl, gr) = (v.gain_l, v.gain_r);
            for i in 0..m {
                dl[i] += buf[i] * gl;
                dr[i] += buf[i] * gr;
            }
        }
        let gain = self.volume * self.tone.gain * 1.6;
        let wet = self.reverb_mix;
        let dry = 1.0 - 0.5 * wet;
        for i in 0..m {
            let l = self.dc_l.tick(dl[i]);
            let r = self.dc_r.tick(dr[i]);
            let (rl, rr) = if wet > 0.0 {
                self.reverb.tick(0.5 * (l + r))
            } else {
                (0.0, 0.0)
            };
            out_l[i] = soft_clip(gain * (dry * l + wet * rl));
            out_r[i] = soft_clip(gain * (dry * r + wet * rr));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &mut Synth, secs: f32) -> (f32, f32) {
        let n = (secs * s.sample_rate()) as usize;
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        s.render(&mut l, &mut r);
        let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
        let rms = (l.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt();
        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()));
        (peak, rms)
    }

    #[test]
    fn every_key_sounds_and_stops() {
        for key in (21..=108u8).step_by(5) {
            let mut s = Synth::new(48000.0);
            s.set_param(param::REVERB, 0.0);
            s.note_on(key, 100);
            let (peak, rms) = run(&mut s, 0.3);
            assert!(peak > 1e-3 && rms > 1e-5, "key {key}: peak {peak}");
            assert!(peak < 1.0);
            s.note_off(key);
            run(&mut s, 3.0);
            if key < 90 {
                assert_eq!(s.active_voices(), 0, "damped key {key} still sounding");
            }
        }
    }

    #[test]
    fn sustain_pedal_holds_notes() {
        let mut s = Synth::new(44100.0);
        s.set_sustain(true);
        s.note_on(48, 90);
        run(&mut s, 0.1);
        s.note_off(48);
        run(&mut s, 1.0);
        assert_eq!(s.active_voices(), 1);
        s.set_sustain(false);
        run(&mut s, 2.0);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn restrike_and_polyphony_limit() {
        let mut s = Synth::with_voices(48000.0, 8);
        for k in 40..60u8 {
            s.note_on(k, 127);
            s.note_on(k, 127);
        }
        let (peak, _) = run(&mut s, 0.5);
        assert!(s.active_voices() <= 8);
        assert!(peak <= 1.0);
    }

    #[test]
    fn presets_render() {
        for i in 0..PRESETS.len() {
            let mut s = Synth::new(48000.0);
            s.set_preset(i);
            for k in [21u8, 45, 60, 84, 108] {
                s.note_on(k, 80);
            }
            let (peak, _) = run(&mut s, 0.5);
            assert!(peak > 1e-3 && peak <= 1.0, "preset {i}");
        }
    }
}
