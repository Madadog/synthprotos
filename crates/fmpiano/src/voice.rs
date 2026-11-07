//! One sounding note: a set of FM stacks plus the hammer knock.

use crate::model::{KnockSpec, NoiseFm, StackKind, StackSpec, VoiceSpec};
use crate::sine::{phase_inc, rad_to_phase, SineTable};
use crate::tables::MAX_STACKS;

#[inline]
fn decay_coef(tau: f32, sr: f32) -> f32 {
    if tau <= 0.0 {
        0.0
    } else {
        (-1.0 / (tau * sr)).exp()
    }
}

/// One FM stack: a modulator and up to three carriers, one per unison string
/// (the 1:1 stack gives every copy its own modulator so each stays exactly
/// 1:1).  All copies share the prompt and aftersound envelopes; `w` says how
/// much of each a copy carries.
#[derive(Clone, Copy, Default)]
struct StackState {
    kind: StackKind,
    pm1: u32,
    dm1: u32,
    pm2: u32,
    dm2: u32,
    pm3: u32,
    dm3: u32,
    pc1: u32,
    dc1: u32,
    pc2: u32,
    dc2: u32,
    pc3: u32,
    dc3: u32,
    /// prompt and aftersound envelopes
    a1: f32,
    k1: f32,
    a2: f32,
    k2: f32,
    /// per string copy: share of prompt and of aftersound
    w: [[f32; 2]; 3],
    /// index = iinf + ix, with ix decaying by kix per sample
    ix: f32,
    kix: f32,
    iinf: f32,
}

impl StackState {
    fn new(s: &StackSpec, unison: &[f64; 3], weights: &[[f32; 2]; 3], sr: f32) -> Self {
        let srd = sr as f64;
        let own_mod = matches!(s.kind, StackKind::DualMod | StackKind::TripleMod);
        let single = s.kind == StackKind::Single;
        let u = if single { [1.0; 3] } else { *unison };
        let fm = |k: usize| if own_mod { s.fm * u[k] } else { s.fm };
        StackState {
            kind: s.kind,
            pm1: 0,
            dm1: phase_inc(fm(0), srd),
            pm2: 0,
            dm2: phase_inc(fm(1), srd),
            pm3: 0,
            dm3: phase_inc(fm(2), srd),
            pc1: 0,
            dc1: phase_inc(s.fc * u[0], srd),
            pc2: 0,
            dc2: phase_inc(s.fc * u[1], srd),
            pc3: 0,
            dc3: phase_inc(s.fc * u[2], srd),
            a1: s.a1,
            k1: decay_coef(s.tau1, sr),
            a2: s.a2,
            k2: decay_coef(s.tau2, sr),
            w: if single {
                [[1.0, 1.0], [0.0; 2], [0.0; 2]]
            } else {
                *weights
            },
            ix: s.i0 - s.iinf,
            kix: decay_coef(s.taui, sr),
            iinf: s.iinf,
        }
    }

    #[inline]
    fn level(&self) -> f32 {
        self.a1.abs() + self.a2.abs()
    }

    /// Advance envelopes and phases by `n` samples without producing sound.
    fn skip(&mut self, n: usize) {
        let ni = n as i32;
        self.a1 *= self.k1.powi(ni);
        self.a2 *= self.k2.powi(ni);
        self.ix *= self.kix.powi(ni);
        let nu = n as u32;
        self.pm1 = self.pm1.wrapping_add(self.dm1.wrapping_mul(nu));
        self.pm2 = self.pm2.wrapping_add(self.dm2.wrapping_mul(nu));
        self.pm3 = self.pm3.wrapping_add(self.dm3.wrapping_mul(nu));
        self.pc1 = self.pc1.wrapping_add(self.dc1.wrapping_mul(nu));
        self.pc2 = self.pc2.wrapping_add(self.dc2.wrapping_mul(nu));
        self.pc3 = self.pc3.wrapping_add(self.dc3.wrapping_mul(nu));
    }

    fn render(&mut self, tab: &SineTable, out: &mut [f32]) {
        let (mut pm1, dm1, mut pm2, dm2, mut pm3, dm3) =
            (self.pm1, self.dm1, self.pm2, self.dm2, self.pm3, self.dm3);
        let (mut pc1, dc1, mut pc2, dc2, mut pc3, dc3) =
            (self.pc1, self.dc1, self.pc2, self.dc2, self.pc3, self.dc3);
        let (mut a1, k1, mut a2, k2) = (self.a1, self.k1, self.a2, self.k2);
        let [[p1, q1], [p2, q2], [p3, q3]] = self.w;
        let (mut ix, kix, iinf) = (self.ix, self.kix, self.iinf);
        match self.kind {
            StackKind::Single => {
                for o in out.iter_mut() {
                    let m = tab.sin(pm1) * (iinf + ix);
                    *o += tab.sin(pc1.wrapping_add(rad_to_phase(m))) * (a1 + a2);
                    ix *= kix;
                    a1 *= k1;
                    a2 *= k2;
                    pm1 = pm1.wrapping_add(dm1);
                    pc1 = pc1.wrapping_add(dc1);
                }
            }
            StackKind::Dual => {
                for o in out.iter_mut() {
                    let off = rad_to_phase(tab.sin(pm1) * (iinf + ix));
                    *o += tab.sin(pc1.wrapping_add(off)) * (p1 * a1 + q1 * a2)
                        + tab.sin(pc2.wrapping_add(off)) * (p2 * a1 + q2 * a2);
                    ix *= kix;
                    a1 *= k1;
                    a2 *= k2;
                    pm1 = pm1.wrapping_add(dm1);
                    pc1 = pc1.wrapping_add(dc1);
                    pc2 = pc2.wrapping_add(dc2);
                }
            }
            StackKind::DualMod => {
                for o in out.iter_mut() {
                    let i = iinf + ix;
                    let o1 = rad_to_phase(tab.sin(pm1) * i);
                    let o2 = rad_to_phase(tab.sin(pm2) * i);
                    *o += tab.sin(pc1.wrapping_add(o1)) * (p1 * a1 + q1 * a2)
                        + tab.sin(pc2.wrapping_add(o2)) * (p2 * a1 + q2 * a2);
                    ix *= kix;
                    a1 *= k1;
                    a2 *= k2;
                    pm1 = pm1.wrapping_add(dm1);
                    pm2 = pm2.wrapping_add(dm2);
                    pc1 = pc1.wrapping_add(dc1);
                    pc2 = pc2.wrapping_add(dc2);
                }
            }
            StackKind::Triple => {
                for o in out.iter_mut() {
                    let off = rad_to_phase(tab.sin(pm1) * (iinf + ix));
                    *o += tab.sin(pc1.wrapping_add(off)) * (p1 * a1 + q1 * a2)
                        + tab.sin(pc2.wrapping_add(off)) * (p2 * a1 + q2 * a2)
                        + tab.sin(pc3.wrapping_add(off)) * (p3 * a1 + q3 * a2);
                    ix *= kix;
                    a1 *= k1;
                    a2 *= k2;
                    pm1 = pm1.wrapping_add(dm1);
                    pc1 = pc1.wrapping_add(dc1);
                    pc2 = pc2.wrapping_add(dc2);
                    pc3 = pc3.wrapping_add(dc3);
                }
            }
            StackKind::TripleMod => {
                for o in out.iter_mut() {
                    let i = iinf + ix;
                    let o1 = rad_to_phase(tab.sin(pm1) * i);
                    let o2 = rad_to_phase(tab.sin(pm2) * i);
                    let o3 = rad_to_phase(tab.sin(pm3) * i);
                    *o += tab.sin(pc1.wrapping_add(o1)) * (p1 * a1 + q1 * a2)
                        + tab.sin(pc2.wrapping_add(o2)) * (p2 * a1 + q2 * a2)
                        + tab.sin(pc3.wrapping_add(o3)) * (p3 * a1 + q3 * a2);
                    ix *= kix;
                    a1 *= k1;
                    a2 *= k2;
                    pm1 = pm1.wrapping_add(dm1);
                    pm2 = pm2.wrapping_add(dm2);
                    pm3 = pm3.wrapping_add(dm3);
                    pc1 = pc1.wrapping_add(dc1);
                    pc2 = pc2.wrapping_add(dc2);
                    pc3 = pc3.wrapping_add(dc3);
                }
            }
        }
        self.pm1 = pm1;
        self.pm2 = pm2;
        self.pm3 = pm3;
        self.pc1 = pc1;
        self.pc2 = pc2;
        self.pc3 = pc3;
        self.a1 = a1;
        self.a2 = a2;
        self.ix = ix;
    }
}

#[derive(Clone, Copy, Default)]
struct NoiseState {
    pm: u32,
    dm: u32,
    pc: u32,
    dc: u32,
    fb: f32,
    y1: f32,
    y2: f32,
    a: f32,
    ka: f32,
    b: f32,
    kb: f32,
    r: f32,
    kr: f32,
    ix: f32,
    kix: f32,
    /// 2-pole low-pass (coefficient and states), lpc = 1 means bypass
    lpc: f32,
    lp1: f32,
    lp2: f32,
}

impl NoiseState {
    fn new(n: &NoiseFm, sr: f32) -> Self {
        let srd = sr as f64;
        NoiseState {
            pm: 0,
            dm: phase_inc(n.fm, srd),
            pc: 0,
            dc: phase_inc(n.fc, srd),
            fb: n.fb,
            y1: 0.0,
            y2: 0.0,
            a: n.amp,
            ka: decay_coef(n.tau, sr),
            b: n.tail_amp,
            kb: decay_coef(n.tail_tau, sr),
            r: if n.rise > 0.0 { 1.0 } else { 0.0 },
            kr: decay_coef(n.rise, sr),
            ix: n.i0,
            kix: decay_coef(n.taui, sr),
            lpc: if n.lowpass > 0.0 {
                1.0 - (-core::f32::consts::TAU * n.lowpass / sr).exp()
            } else {
                1.0
            },
            lp1: 0.0,
            lp2: 0.0,
        }
    }

    fn active(&self) -> bool {
        self.a + self.b > 1e-6
    }

    fn level(&self) -> f32 {
        if self.active() {
            self.a + self.b
        } else {
            0.0
        }
    }

    fn render(&mut self, tab: &SineTable, out: &mut [f32]) {
        if !self.active() {
            return;
        }
        for o in out.iter_mut() {
            // DX7-style feedback: average of the last two outputs
            let m = tab.sin(
                self.pm
                    .wrapping_add(rad_to_phase(self.fb * 0.5 * (self.y1 + self.y2))),
            );
            self.y2 = self.y1;
            self.y1 = m;
            let y = tab.sin(self.pc.wrapping_add(rad_to_phase(self.ix * m)))
                * ((self.a + self.b) * (1.0 - self.r));
            self.lp1 += self.lpc * (y - self.lp1);
            self.lp2 += self.lpc * (self.lp1 - self.lp2);
            *o += self.lp2;
            self.r *= self.kr;
            self.a *= self.ka;
            self.b *= self.kb;
            self.ix *= self.kix;
            self.pm = self.pm.wrapping_add(self.dm);
            self.pc = self.pc.wrapping_add(self.dc);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct KnockState {
    click: NoiseState,
    thump: NoiseState,
    halo: NoiseState,
}

impl KnockState {
    fn new(k: &KnockSpec, sr: f32) -> Self {
        KnockState {
            click: NoiseState::new(&k.click, sr),
            thump: NoiseState::new(&k.thump, sr),
            halo: NoiseState::new(&k.halo, sr),
        }
    }

    fn level(&self) -> f32 {
        self.click.level() + self.thump.level() + self.halo.level()
    }

    fn render(&mut self, tab: &SineTable, out: &mut [f32]) {
        self.click.render(tab, out);
        self.thump.render(tab, out);
        self.halo.render(tab, out);
    }
}

/// Fast fade used when a voice is stolen or its key is struck again.
const KILL_TAU: f32 = 0.012;

pub struct Voice {
    pub key: u8,
    pub active: bool,
    pub key_down: bool,
    pub releasing: bool,
    /// note-on order, used for voice stealing
    pub serial: u64,
    stacks: [StackState; MAX_STACKS],
    n: usize,
    knock: KnockState,
    att: f32,
    katt: f32,
    rel: f32,
    krel: f32,
    damped: bool,
    release_tau: f32,
    sr: f32,
    pub gain_l: f32,
    pub gain_r: f32,
    level: f32,
    /// stacks quieter than this are inaudible (-90 dB re. the note's start)
    floor: f32,
}

impl Voice {
    pub fn idle(sr: f32) -> Self {
        Voice {
            key: 0,
            active: false,
            key_down: false,
            releasing: false,
            serial: 0,
            stacks: [StackState::default(); MAX_STACKS],
            n: 0,
            knock: KnockState::default(),
            att: 0.0,
            katt: 0.0,
            rel: 1.0,
            krel: 1.0,
            damped: true,
            release_tau: 0.1,
            sr,
            gain_l: 0.0,
            gain_r: 0.0,
            level: 0.0,
            floor: 1e-7,
        }
    }

    pub fn start(&mut self, key: u8, spec: &VoiceSpec, serial: u64) {
        let sr = self.sr;
        self.key = key;
        self.active = true;
        self.key_down = true;
        self.releasing = false;
        self.serial = serial;
        self.n = spec.n;
        for (st, sp) in self.stacks.iter_mut().zip(spec.stacks[..spec.n].iter()) {
            *st = StackState::new(sp, &spec.unison, &spec.weights, sr);
        }
        self.knock = KnockState::new(&spec.knock, sr);
        self.att = 1.0;
        self.katt = decay_coef(spec.attack, sr);
        self.rel = 1.0;
        self.krel = 1.0;
        self.damped = spec.damped;
        self.release_tau = spec.release;
        let ang = (spec.pan.clamp(-1.0, 1.0) + 1.0) * core::f32::consts::FRAC_PI_4;
        self.gain_l = ang.cos();
        self.gain_r = ang.sin();
        self.level = self.stacks[..self.n].iter().map(|s| s.level()).sum();
        self.floor = (3e-5 * self.level).max(1e-7);
    }

    /// Key released and not held by the sustain pedal: the damper falls.
    pub fn release(&mut self) {
        self.key_down = false;
        if self.damped && !self.releasing {
            self.releasing = true;
            self.krel = decay_coef(self.release_tau, self.sr);
        }
    }

    /// Quickly silence (voice stealing or re-strike of the same key).
    pub fn kill(&mut self) {
        self.key_down = false;
        self.releasing = true;
        self.krel = self.krel.min(decay_coef(KILL_TAU, self.sr));
    }

    /// Current output level estimate (used for voice stealing).
    pub fn level(&self) -> f32 {
        self.level * self.rel
    }

    /// Render one block into `buf` (overwritten).  Returns false when the
    /// voice has become silent and was freed.
    pub fn render(&mut self, tab: &SineTable, buf: &mut [f32]) -> bool {
        buf.fill(0.0);
        // stacks decay at different rates: those 70 dB below the voice's
        // current level are inaudible, so only their envelopes are advanced
        let thresh = self.floor.max(3e-4 * self.level);
        for st in self.stacks[..self.n].iter_mut() {
            if st.level() > thresh {
                st.render(tab, buf);
            } else {
                st.skip(buf.len());
            }
        }
        // hammer contact: the strings' vibration builds up over a few ms
        let mut att = self.att;
        if att > 1e-6 {
            let katt = self.katt;
            for x in buf.iter_mut() {
                *x *= 1.0 - att;
                att *= katt;
            }
            self.att = att;
        }
        // the impact noise itself is not ramped
        self.knock.render(tab, buf);
        let (mut rel, krel) = (self.rel, self.krel);
        if krel < 1.0 {
            for x in buf.iter_mut() {
                *x *= rel;
                rel *= krel;
            }
            self.rel = rel;
        }
        if self.releasing {
            // dampers also kill the upper partials: shrink the indices
            let k = (-(buf.len() as f32) / (0.5 * self.release_tau * self.sr)).exp();
            for st in self.stacks[..self.n].iter_mut() {
                st.ix *= k;
                st.iinf *= k;
            }
        }
        self.level = self.stacks[..self.n].iter().map(|s| s.level()).sum();
        if (self.level + self.knock.level()) * self.rel < self.floor.max(2e-6) {
            self.active = false;
        }
        self.active
    }
}
