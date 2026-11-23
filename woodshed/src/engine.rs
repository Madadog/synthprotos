//! Playback engine: transport, looping, and pitch-preserving speed change.
//!
//! The engine pulls audio from the in-memory track through a WSOLA
//! time-stretcher (overlap-added Hann frames whose start is nudged to the best
//! waveform match), then resamples to the output device's rate. It is driven by
//! whoever owns the output: a cpal stream, or a silent clock when the machine
//! has no audio device.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::track::Track;

pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 1.5;

pub struct Engine {
    track: Option<Arc<Track>>,
    playing: bool,
    /// Source position (frames) of the next stretcher frame.
    pos: f64,
    /// Source position of the previous stretcher frame, for loop-end detection.
    prev_pos: f64,
    speed: f64,
    /// Time-stretch so speed changes keep the pitch; otherwise speed works
    /// like a tape machine and pitch follows it.
    keep_pitch: bool,
    volume: f32,
    /// Loop region in source frames.
    loop_region: Option<(f64, f64)>,
    /// Silence between loop repeats, seconds.
    loop_gap: f64,
    out_rate: f64,
    /// Set when playback ran off the end of the track.
    finished: bool,

    // Time-stretcher (runs at the track's sample rate).
    hop: usize,
    search: i64,
    window: Vec<f32>,
    ola: Vec<[f32; 2]>,
    fifo: VecDeque<[f32; 2]>,
    gap_left: usize,
    /// Where the previous frame would continue if played unstretched.
    natural: Option<i64>,
    target: Vec<f32>,

    // Resampler to the device rate.
    hist: [[f32; 2]; 4],
    frac: f64,
    fade: f32,

    // Reference tone played from the piano.
    tone: Option<f64>,
    tone_freq: f64,
    tone_phase: f64,
    tone_level: f32,
    tone_volume: f32,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            track: None,
            playing: false,
            pos: 0.0,
            prev_pos: 0.0,
            speed: 1.0,
            keep_pitch: true,
            volume: 0.8,
            loop_region: None,
            loop_gap: 0.0,
            out_rate: 48000.0,
            finished: false,
            hop: 1,
            search: 0,
            window: Vec::new(),
            ola: Vec::new(),
            fifo: VecDeque::new(),
            gap_left: 0,
            natural: None,
            target: Vec::new(),
            hist: [[0.0; 2]; 4],
            frac: 1.0,
            fade: 0.0,
            tone: None,
            tone_freq: 440.0,
            tone_phase: 0.0,
            tone_level: 0.0,
            tone_volume: 0.5,
        }
    }

    pub fn set_track(&mut self, track: Option<Arc<Track>>) {
        self.playing = false;
        self.finished = false;
        self.loop_region = None;
        if let Some(t) = &track {
            let sr = t.sample_rate as f64;
            // 60 ms frames with 50% overlap; search +-15 ms for the best splice.
            self.hop = (sr * 0.030) as usize;
            self.search = (sr * 0.015) as i64;
            let w = self.hop * 2;
            self.window = (0..w)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / w as f32).cos())
                .collect();
            self.ola = vec![[0.0; 2]; w];
        }
        self.track = track;
        self.reset_to(0.0);
    }

    pub fn set_output_rate(&mut self, rate: u32) {
        self.out_rate = rate as f64;
    }

    fn rate(&self) -> f64 {
        self.track.as_ref().map_or(48000.0, |t| t.sample_rate as f64)
    }

    fn frames(&self) -> f64 {
        self.track.as_ref().map_or(0.0, |t| t.frames() as f64)
    }

    fn reset_to(&mut self, frame: f64) {
        self.pos = frame;
        self.prev_pos = frame;
        self.natural = None;
        self.fifo.clear();
        self.gap_left = 0;
        if !self.playing {
            // While playing, the overlap tail is kept so a seek crossfades.
            self.ola.iter_mut().for_each(|f| *f = [0.0; 2]);
            self.hist = [[0.0; 2]; 4];
            self.frac = 1.0;
            self.fade = 0.0;
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        if self.track.is_none() {
            return;
        }
        if self.pos >= self.frames() - 1.0 {
            self.reset_to(0.0);
        }
        self.finished = false;
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Seek to a time in seconds.
    pub fn seek(&mut self, secs: f64) {
        let frame = (secs * self.rate()).clamp(0.0, self.frames());
        self.finished = false;
        self.reset_to(frame);
    }

    /// The position currently being heard, in seconds.
    pub fn position(&self) -> f64 {
        // The FIFO is refilled only when empty, so this lies between the
        // previous and the next stretcher frame.
        let mut p = self.pos - self.fifo.len() as f64 * self.stretch();
        if self.pos >= self.frames() {
            p = self.frames(); // only the final fade-out is left
        }
        if let Some((a, b)) = self.loop_region {
            // The last frame overshoots the loop end slightly before wrapping.
            if self.prev_pos < b && p >= b {
                p = a + (p - b);
            }
        }
        (p.max(0.0) / self.rate()).min(self.frames() / self.rate())
    }

    pub fn set_keep_pitch(&mut self, keep_pitch: bool) {
        self.keep_pitch = keep_pitch;
    }

    /// Source frames consumed per frame the stretcher emits.
    fn stretch(&self) -> f64 {
        if self.keep_pitch {
            self.speed
        } else {
            1.0
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    /// Loop region in seconds, and the pause between repeats.
    pub fn set_loop(&mut self, region: Option<(f64, f64)>, gap: f64) {
        let sr = self.rate();
        self.loop_region = region.filter(|(a, b)| b > a).map(|(a, b)| (a * sr, b * sr));
        self.loop_gap = gap.max(0.0);
    }

    /// True once after playback stopped by reaching the end of the track.
    pub fn take_finished(&mut self) -> bool {
        std::mem::take(&mut self.finished)
    }

    /// Emit the fading-out half of the last frame and start afresh.
    fn flush_tail(&mut self) {
        let hop = self.hop;
        self.fifo.extend(self.ola[..hop].iter().copied());
        self.ola.iter_mut().for_each(|f| *f = [0.0; 2]);
        self.natural = None;
    }

    /// Produce the next `hop` stretched frames into the FIFO.
    fn step(&mut self, track: &Track) {
        let hop = self.hop;
        if let Some((a, b)) = self.loop_region {
            if self.prev_pos < b && self.pos >= b {
                self.pos = a;
                self.prev_pos = a;
                self.natural = None;
                if self.loop_gap > 0.0 {
                    self.flush_tail();
                    self.gap_left = (self.loop_gap * track.sample_rate as f64) as usize;
                    return;
                }
            }
        }
        let end = track.frames() as f64;
        if self.pos >= end {
            self.flush_tail();
            self.pos = end;
            self.prev_pos = end;
            if self.playing {
                self.playing = false;
                self.finished = true;
            }
            return;
        }

        let nominal = self.pos.round() as i64;
        let start = match self.natural {
            None => nominal,
            Some(nat) if (self.stretch() - 1.0).abs() < 1e-3 => nat,
            Some(nat) => self.best_match(track, nominal, nat),
        };
        for (i, (acc, w)) in self.ola.iter_mut().zip(&self.window).enumerate() {
            let s = track.stereo(start + i as i64);
            acc[0] += s[0] * w;
            acc[1] += s[1] * w;
        }
        self.fifo.extend(self.ola[..hop].iter().copied());
        self.ola.copy_within(hop.., 0);
        self.ola[hop..].iter_mut().for_each(|f| *f = [0.0; 2]);
        self.natural = Some(start + hop as i64);
        self.prev_pos = self.pos;
        self.pos += hop as f64 * self.stretch();
    }

    /// Find the frame start near `nominal` whose waveform best continues the
    /// previous frame (which would naturally continue at `natural`).
    fn best_match(&mut self, track: &Track, nominal: i64, natural: i64) -> i64 {
        if (natural - nominal).abs() <= self.search {
            return natural;
        }
        const STRIDE: usize = 4;
        let n = self.hop / STRIDE;
        self.target.clear();
        self.target.extend((0..n).map(|j| track.mono(natural + (j * STRIDE) as i64)));
        let target = &self.target;
        let score = |start: i64| -> f32 {
            let mut num = 0.0f32;
            let mut den = 1e-9f32;
            for (j, t) in target.iter().enumerate() {
                let x = track.mono(start + (j * STRIDE) as i64);
                num += x * t;
                den += x * x;
            }
            num / den.sqrt()
        };
        let mut best = (f32::MIN, nominal);
        let mut d = -self.search;
        while d <= self.search {
            let s = score(nominal + d);
            if s > best.0 {
                best = (s, nominal + d);
            }
            d += 3;
        }
        let coarse = best.1;
        for d in [-2, -1, 1, 2] {
            let s = score(coarse + d);
            if s > best.0 {
                best = (s, coarse + d);
            }
        }
        best.1
    }

    fn next_frame(&mut self, track: &Track) -> [f32; 2] {
        loop {
            if let Some(f) = self.fifo.pop_front() {
                return f;
            }
            if self.gap_left > 0 {
                self.gap_left -= 1;
                return [0.0; 2];
            }
            self.step(track);
        }
    }

    /// Fill an interleaved output buffer at the device rate.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        let track = self.track.clone();
        // Without time-stretching, speed is applied here by resampling.
        let ratio = track.as_ref().map_or(1.0, |t| t.sample_rate as f64 / self.out_rate) * (self.speed / self.stretch());
        let fade_step = (1.0 / (0.008 * self.out_rate)) as f32;
        for frame in out.chunks_mut(channels.max(1)) {
            let mut s = [0.0f32; 2];
            if self.playing {
                self.fade = (self.fade + fade_step).min(1.0);
            } else {
                self.fade = (self.fade - fade_step).max(0.0);
            }
            if let Some(track) = track.as_ref().filter(|_| self.fade > 0.0) {
                while self.frac >= 1.0 {
                    self.hist.copy_within(1.., 0);
                    self.hist[3] = self.next_frame(track);
                    self.frac -= 1.0;
                }
                let t = self.frac as f32;
                let gain = self.fade * self.volume;
                for (ch, s) in s.iter_mut().enumerate() {
                    let [y0, y1, y2, y3] = self.hist.map(|f| f[ch]);
                    // 4-point Catmull-Rom interpolation between y1 and y2.
                    let c1 = 0.5 * (y2 - y0);
                    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
                    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
                    *s = (((c3 * t + c2) * t + c1) * t + y1) * gain;
                }
                self.frac += ratio;
            }
            // The reference tone, with a short fade either side to avoid clicks.
            let target = if self.tone.is_some() { 1.0 } else { 0.0 };
            if self.tone_level != target || target > 0.0 {
                self.tone_level += (target - self.tone_level).clamp(-fade_step, fade_step);
                if let Some(freq) = self.tone {
                    self.tone_freq = freq;
                }
                self.tone_phase = (self.tone_phase + self.tone_freq / self.out_rate).fract();
                let tone = (self.tone_phase * std::f64::consts::TAU).sin() as f32 * self.tone_level * self.tone_volume;
                s[0] += tone;
                s[1] += tone;
            }
            match frame {
                [m] => *m = (s[0] + s[1]) * 0.5,
                [l, r, ..] => {
                    *l = s[0];
                    *r = s[1];
                }
                [] => {}
            }
        }
    }

    /// Sound a sine tone at `freq` Hz until called again with `None`.
    pub fn set_tone(&mut self, freq: Option<f64>, volume: f32) {
        self.tone = freq;
        self.tone_volume = volume.clamp(0.0, 1.0);
    }
}

/// The audio output driving an engine. Dropping it stops playback.
pub struct Output {
    _stream: Option<cpal::Stream>,
    silent_stop: Option<Arc<AtomicBool>>,
    /// Why there is no sound, if the audio device could not be opened.
    pub error: Option<String>,
}

impl Output {
    /// Open the default output device, preferably at `preferred_rate` so the
    /// track needs no resampling. Falls back to a silent real-time clock so
    /// the transport still runs on machines without audio.
    pub fn open(engine: &Arc<Mutex<Engine>>, preferred_rate: Option<u32>) -> Output {
        match open_stream(engine, preferred_rate) {
            Ok(stream) => Output { _stream: Some(stream), silent_stop: None, error: None },
            Err(error) => {
                let stop = Arc::new(AtomicBool::new(false));
                spawn_silent_clock(engine.clone(), stop.clone());
                Output { _stream: None, silent_stop: Some(stop), error: Some(error) }
            }
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(stop) = &self.silent_stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

fn open_stream(engine: &Arc<Mutex<Engine>>, preferred_rate: Option<u32>) -> Result<cpal::Stream, String> {
    let device = cpal::default_host().default_output_device().ok_or("No audio output device found")?;
    let default = device.default_output_config().map_err(|e| format!("Audio output unavailable: {e}"))?;
    let mut chosen = default;
    if let Some(rate) = preferred_rate.filter(|&r| r != default.sample_rate()) {
        if let Ok(configs) = device.supported_output_configs() {
            let mut configs = configs;
            let matching = configs.find(|c| {
                c.channels() == default.channels()
                    && c.sample_format() == default.sample_format()
                    && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
            });
            if let Some(c) = matching {
                chosen = c.with_sample_rate(rate);
            }
        }
    }
    let build = |config: &cpal::SupportedStreamConfig| -> Result<cpal::Stream, String> {
        use cpal::SampleFormat as F;
        let stream = match config.sample_format() {
            F::F32 => build_stream::<f32>(&device, config, engine.clone()),
            F::I16 => build_stream::<i16>(&device, config, engine.clone()),
            F::U16 => build_stream::<u16>(&device, config, engine.clone()),
            F::I32 => build_stream::<i32>(&device, config, engine.clone()),
            F::U8 => build_stream::<u8>(&device, config, engine.clone()),
            other => return Err(format!("Unsupported audio sample format {other}")),
        }
        .map_err(|e| format!("Cannot open audio output: {e}"))?;
        stream.play().map_err(|e| format!("Cannot start audio output: {e}"))?;
        engine.lock().unwrap().set_output_rate(config.sample_rate());
        Ok(stream)
    };
    build(&chosen).or_else(|e| if chosen == default { Err(e) } else { build(&default) })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    engine: Arc<Mutex<Engine>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels() as usize;
    let mut scratch: Vec<f32> = Vec::new();
    device.build_output_stream(
        config.config(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            scratch.resize(data.len(), 0.0);
            engine.lock().unwrap().render(&mut scratch, channels);
            for (out, s) in data.iter_mut().zip(&scratch) {
                *out = T::from_sample(*s);
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )
}

fn spawn_silent_clock(engine: Arc<Mutex<Engine>>, stop: Arc<AtomicBool>) {
    const RATE: u32 = 48000;
    const BLOCK: usize = 480;
    engine.lock().unwrap().set_output_rate(RATE);
    std::thread::spawn(move || {
        let mut scratch = vec![0.0f32; BLOCK * 2];
        let mut next = Instant::now();
        let block = Duration::from_secs_f64(BLOCK as f64 / RATE as f64);
        while !stop.load(Ordering::Relaxed) {
            engine.lock().unwrap().render(&mut scratch, 2);
            next += block;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > Duration::from_millis(200) {
                next = now;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48000;

    fn sine_track(freq: f64, secs: f64) -> Arc<Track> {
        let n = (secs * SR as f64) as usize;
        let mono: Vec<f32> =
            (0..n).map(|i| (std::f64::consts::TAU * freq * i as f64 / SR as f64).sin() as f32 * 0.5).collect();
        Arc::new(Track::from_mono(SR, &mono))
    }

    fn engine(track: Arc<Track>) -> Engine {
        let mut e = Engine::new();
        e.set_output_rate(SR);
        e.set_track(Some(track));
        e.set_volume(1.0);
        e
    }

    fn render(e: &mut Engine, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        // Odd-sized chunks, like a real device callback.
        for chunk in out.chunks_mut(2 * 441) {
            e.render(chunk, 2);
        }
        out.chunks(2).map(|f| f[0]).collect()
    }

    /// Estimate frequency from upward zero crossings.
    fn zero_crossing_freq(x: &[f32]) -> f64 {
        let crossings: Vec<f64> = x
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] <= 0.0 && w[1] > 0.0)
            .map(|(i, w)| i as f64 + (w[0] / (w[0] - w[1])) as f64)
            .collect();
        (crossings.len() - 1) as f64 * SR as f64 / (crossings.last().unwrap() - crossings[0])
    }

    #[test]
    fn unit_speed_reproduces_the_track() {
        let track = sine_track(440.0, 2.0);
        let mut e = engine(track.clone());
        e.play();
        let out = render(&mut e, SR as usize);
        // After the fade-in and the resampler's 2-frame delay, output == input.
        for (i, &got) in out.iter().enumerate().skip(4000) {
            let want = track.samples[(i - 2) * 2];
            assert!((got - want).abs() < 1e-4, "frame {i}: {got} vs {want}");
        }
        assert!((e.position() - 1.0).abs() < 0.01);
    }

    #[test]
    fn half_speed_keeps_pitch_and_level() {
        let mut e = engine(sine_track(440.0, 3.0));
        e.set_speed(0.5);
        e.play();
        let out = render(&mut e, 2 * SR as usize);
        let body = &out[4800..];
        assert!((zero_crossing_freq(body) - 440.0).abs() < 1.0);
        let rms = (body.iter().map(|x| x * x).sum::<f32>() / body.len() as f32).sqrt();
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms {rms}");
        // Two seconds of output at half speed covers one second of the track.
        assert!((e.position() - 1.0).abs() < 0.05, "position {}", e.position());
    }

    #[test]
    fn half_speed_without_stretch_drops_an_octave() {
        let mut e = engine(sine_track(440.0, 3.0));
        e.set_keep_pitch(false);
        e.set_speed(0.5);
        e.play();
        let out = render(&mut e, 2 * SR as usize);
        assert!((zero_crossing_freq(&out[4800..]) - 220.0).abs() < 0.5);
        assert!((e.position() - 1.0).abs() < 0.05, "position {}", e.position());
    }

    #[test]
    fn tone_sounds_without_a_track_and_fades_out() {
        let mut e = Engine::new();
        e.set_output_rate(SR);
        e.set_tone(Some(440.0), 0.5);
        let out = render(&mut e, SR as usize / 2);
        let body = &out[4800..];
        assert!((zero_crossing_freq(body) - 440.0).abs() < 0.5);
        let peak = body.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        // A change of note keeps the waveform continuous.
        e.set_tone(Some(880.0), 0.5);
        let next = render(&mut e, 480);
        assert!((next[0] - out[out.len() - 1]).abs() < 0.06);
        e.set_tone(None, 0.5);
        let tail = render(&mut e, SR as usize / 10);
        assert!(tail[4000..].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn loop_wraps_and_end_stops() {
        let mut e = engine(sine_track(220.0, 2.0));
        e.set_loop(Some((0.5, 1.0)), 0.0);
        e.seek(0.4);
        e.play();
        for _ in 0..40 {
            render(&mut e, SR as usize / 10);
            let p = e.position();
            assert!((0.39..1.0).contains(&p), "position {p} left the loop");
        }
        assert!(e.is_playing());

        e.set_loop(None, 0.0);
        e.seek(1.8);
        render(&mut e, SR as usize / 2);
        assert!(!e.is_playing());
        assert!(e.take_finished());
        assert!((e.position() - 2.0).abs() < 1e-6);
        // Playing again from the end restarts the track.
        e.play();
        assert!(e.position() < 0.01);
    }

    #[test]
    fn loop_gap_holds_at_loop_start() {
        let mut e = engine(sine_track(220.0, 2.0));
        e.set_loop(Some((0.5, 0.7)), 0.5);
        e.seek(0.5);
        e.play();
        // 0.2 s of loop, then 0.5 s of silence with the playhead parked at 0.5.
        let out = render(&mut e, (SR as f64 * 0.6) as usize);
        assert!((e.position() - 0.5).abs() < 0.01, "position {}", e.position());
        let tail = &out[out.len() - 4800..];
        assert!(tail.iter().all(|x| x.abs() < 1e-6));
    }
}
