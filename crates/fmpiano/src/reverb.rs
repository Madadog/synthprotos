//! Small 8-line feedback-delay-network reverb (Hadamard mixing, per-line
//! damping) to place the piano in a room.  Not part of the tone model.

const LINES: usize = 8;
// mutually prime lengths (samples @ 48 kHz), ~24..67 ms
const BASE_LEN: [usize; LINES] = [1153, 1381, 1621, 1847, 2113, 2381, 2711, 3203];

struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    fn new(len: usize) -> Self {
        Delay {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }
    #[inline]
    fn read(&self) -> f32 {
        self.buf[self.pos]
    }
    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
    }
}

pub struct Reverb {
    lines: Vec<Delay>,
    lp: [f32; LINES],
    g: [f32; LINES],
    damp: f32,
    pre: Delay,
    hp: f32,
}

impl Reverb {
    pub fn new(sr: f32) -> Self {
        let scale = sr / 48000.0;
        let lines: Vec<Delay> = BASE_LEN
            .iter()
            .map(|&l| Delay::new((l as f32 * scale) as usize))
            .collect();
        let mut r = Reverb {
            lines,
            lp: [0.0; LINES],
            g: [0.0; LINES],
            damp: 0.35,
            pre: Delay::new((0.012 * sr) as usize),
            hp: 0.0,
        };
        r.set_time(1.9, sr);
        r
    }

    /// Reverb time (T60, seconds) at low frequencies.
    pub fn set_time(&mut self, t60: f32, sr: f32) {
        for (g, d) in self.g.iter_mut().zip(self.lines.iter()) {
            *g = 10f32.powf(-3.0 * d.buf.len() as f32 / (t60 * sr));
        }
    }

    /// Process a mono input sample, returning a stereo wet signal.
    #[inline]
    pub fn tick(&mut self, x: f32) -> (f32, f32) {
        // remove deep lows from the reverb input
        self.hp += 0.004 * (x - self.hp);
        let xin = x - self.hp;
        let xd = self.pre.read();
        self.pre.write(xin);
        let k = 1.0 - self.damp;
        for (lp, line) in self.lp.iter_mut().zip(self.lines.iter()) {
            *lp += k * (line.read() - *lp);
        }
        let y = self.lp;
        let (l, r) = (
            y[0] - y[1] + y[2] - y[3] + y[4] - y[5] + y[6] - y[7],
            y[0] + y[1] - y[2] - y[3] + y[4] + y[5] - y[6] - y[7],
        );
        // fast Walsh-Hadamard transform (orthogonal after 1/sqrt(8))
        let mut h = y;
        let mut span = 1;
        while span < LINES {
            for i in (0..LINES).step_by(span * 2) {
                for j in i..i + span {
                    let (a, b) = (h[j], h[j + span]);
                    h[j] = a + b;
                    h[j + span] = a - b;
                }
            }
            span *= 2;
        }
        const NORM: f32 = 0.353_553_38; // 1/sqrt(8)
        for (i, (line, (&hi, &gi))) in self
            .lines
            .iter_mut()
            .zip(h.iter().zip(self.g.iter()))
            .enumerate()
        {
            let inj = if i % 2 == 0 { xd } else { -xd };
            line.write(inj * 0.5 + hi * NORM * gi);
        }
        (l * 0.25, r * 0.25)
    }
}
