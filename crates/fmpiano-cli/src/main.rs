//! Offline renderer for the FM piano.
//!
//!   fmpiano note   --key 60 --vel 100 [--dur 6] [--hold S] [--preset N] [--wet] -o out.wav
//!   fmpiano refset --outdir DIR [--dur 10]     # the 30 Salamander keys x 16 layers, dry mono
//!   fmpiano demo   -o demo.wav [--preset N]
//!   fmpiano bench
//!
//! Every command accepts --preset N and --param ID=VALUE[,ID=VALUE...] (ids in
//! `fmpiano::param`, e.g. 6 = unison detune, 13 = strings per note).

use fmpiano::{model, param, Synth, PRESETS};
use std::io::Write;
use std::time::Instant;

const SR: f32 = 48000.0;
/// Volume setting for which a centred voice comes out at unity gain in (L+R)/2.
const UNITY_VOLUME: f32 = 1.0 / (1.6 * core::f32::consts::FRAC_1_SQRT_2);

fn write_wav(path: &str, sr: u32, chans: &[Vec<f32>]) -> std::io::Result<()> {
    let nch = chans.len() as u16;
    let frames = chans[0].len();
    let data_len = (frames * nch as usize * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&nch.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * nch as u32 * 4).to_le_bytes())?;
    f.write_all(&(nch * 4).to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for i in 0..frames {
        for c in chans {
            f.write_all(&c[i].to_le_bytes())?;
        }
    }
    Ok(())
}

struct Args(Vec<String>);

impl Args {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .position(|a| a == name)
            .and_then(|i| self.0.get(i + 1))
            .map(|s| s.as_str())
    }
    fn num(&self, name: &str, default: f32) -> f32 {
        self.get(name)
            .map(|v| v.parse().expect("number"))
            .unwrap_or(default)
    }
    fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }
}

/// FMPIANO_KNOCK="name=value,..." overrides hammer-noise parameters (tuning).
fn knock_from_env() -> model::KnockModel {
    let mut k = model::KnockModel::default();
    if let Ok(spec) = std::env::var("FMPIANO_KNOCK") {
        for kv in spec.split(',').filter(|s| !s.is_empty()) {
            let (name, v) = kv.split_once('=').expect("name=value");
            let v: f32 = v.parse().expect("number");
            match name {
                "thump_db" => k.thump_db = v,
                "thump_key_db" => k.thump_key_db = v,
                "thump_vel_db" => k.thump_vel_db = v,
                "thump_tau" => k.thump_tau = v,
                "tail_rel_db" => k.tail_rel_db = v,
                "tail_tau" => k.tail_tau = v,
                "thump_fc" => k.thump_fc = v,
                "thump_fm" => k.thump_fm = v,
                "thump_i0" => k.thump_i0 = v,
                "thump_rise" => k.thump_rise = v,
                "thump_lp" => k.thump_lp = v,
                "click_db" => k.click_db = v,
                "click_key_db" => k.click_key_db = v,
                "click_vel_db" => k.click_vel_db = v,
                "click_tau" => k.click_tau = v,
                "click_fc" => k.click_fc = v,
                "click_fm" => k.click_fm = v,
                "click_i0" => k.click_i0 = v,
                "halo_db" => k.halo_db = v,
                "halo_key_db" => k.halo_key_db = v,
                "halo_vel_db" => k.halo_vel_db = v,
                "halo_tau" => k.halo_tau = v,
                "halo_fc" => k.halo_fc = v,
                "halo_fm" => k.halo_fm = v,
                "halo_i0" => k.halo_i0 = v,
                "halo_lp" => k.halo_lp = v,
                _ => panic!("unknown knock parameter {name}"),
            }
        }
    }
    k
}

fn make_synth(a: &Args) -> Synth {
    let mut s = Synth::new(SR);
    s.set_knock_model(knock_from_env());
    s.set_preset(a.num("--preset", 0.0) as usize);
    // tone overrides: --param ID=VALUE[,ID=VALUE...] (ids: fmpiano::param)
    if let Some(list) = a.get("--param") {
        for kv in list.split(',') {
            let (id, v) = kv.split_once('=').expect("ID=VALUE");
            s.set_param(id.parse().expect("param id"), v.parse().expect("value"));
        }
    }
    if a.flag("--wet") {
        s.set_param(param::REVERB, 0.18);
    } else {
        // dry, centred, unity gain: (L+R)/2 equals the raw voice signal
        s.set_param(param::REVERB, 0.0);
        s.set_param(param::WIDTH, 0.0);
        s.set_param(param::VOLUME, UNITY_VOLUME);
    }
    s
}

fn render_note(s: &mut Synth, key: u8, vel: f32, dur: f32, hold: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (dur * SR) as usize;
    let off = (hold * SR) as usize;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    s.note_on_f(key, vel);
    let mut pos = 0;
    while pos < n {
        let end = if pos < off { off.min(n) } else { n };
        let end = end.min(pos + 4096);
        s.render(&mut l[pos..end], &mut r[pos..end]);
        pos = end;
        if pos == off {
            s.note_off(key);
        }
    }
    (l, r)
}

/// MIDI velocity that lands exactly on Salamander layer `layer` (1..=16).
fn layer_velocity(layer: u32) -> f32 {
    8.0 + (layer as f32 - 1.0) * (127.0 - 8.0) / 15.0
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().cloned().unwrap_or_default();
    let a = Args(argv);
    match cmd.as_str() {
        "note" => {
            let mut s = make_synth(&a);
            let key = a.num("--key", 60.0) as u8;
            let vel = a
                .get("--layer")
                .map(|l| layer_velocity(l.parse().unwrap()))
                .unwrap_or(a.num("--vel", 100.0));
            let dur = a.num("--dur", 6.0);
            let hold = a.num("--hold", dur + 1.0);
            let (l, r) = render_note(&mut s, key, vel, dur, hold);
            let out = a.get("-o").unwrap_or("note.wav");
            if a.flag("--mono") {
                let m: Vec<f32> = l.iter().zip(&r).map(|(x, y)| 0.5 * (x + y)).collect();
                write_wav(out, SR as u32, &[m]).unwrap();
            } else {
                write_wav(out, SR as u32, &[l, r]).unwrap();
            }
        }
        "refset" => {
            let dir = a.get("--outdir").unwrap_or("out/refset").to_string();
            std::fs::create_dir_all(&dir).unwrap();
            let names = [
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
            ];
            let layers: Vec<u32> = match a.get("--layers") {
                Some(v) => v.split(',').map(|x| x.parse().unwrap()).collect(),
                None => (1..=16).collect(),
            };
            let dur = a.num("--dur", 10.0);
            let mut keys: Vec<u8> = vec![21];
            for o in 1..8 {
                for k in [0u8, 3, 6, 9] {
                    keys.push(12 * (o + 1) + k);
                }
            }
            keys.push(108);
            for &key in &keys {
                let name = format!("{}{}", names[key as usize % 12], key as i32 / 12 - 1);
                for &layer in &layers {
                    let mut s = make_synth(&a);
                    let (l, r) = render_note(&mut s, key, layer_velocity(layer), dur, dur + 1.0);
                    let m: Vec<f32> = l.iter().zip(&r).map(|(x, y)| 0.5 * (x + y)).collect();
                    write_wav(&format!("{dir}/{name}v{layer}.wav"), SR as u32, &[m]).unwrap();
                }
            }
            eprintln!("rendered {} files to {dir}", keys.len() * layers.len());
        }
        "demo" => {
            let mut s = make_synth(&a);
            s.set_param(param::REVERB, 0.2);
            // (time s, key, vel, dur s)
            let mut ev: Vec<(f32, u8, f32, f32)> = Vec::new();
            let mut t = 0.0;
            for (i, k) in [60u8, 62, 64, 65, 67, 69, 71, 72].iter().enumerate() {
                ev.push((t, *k, 50.0 + 9.0 * i as f32, 0.35));
                t += 0.3;
            }
            t += 0.4;
            for chord in [
                [48u8, 55, 64, 72],
                [45, 52, 60, 69],
                [41, 53, 60, 69],
                [43, 55, 62, 71],
            ] {
                for (j, k) in chord.iter().enumerate() {
                    ev.push((t + 0.012 * j as f32, *k, 70.0, 1.1));
                }
                t += 1.2;
            }
            ev.push((t, 21, 110.0, 4.0));
            ev.push((t, 33, 100.0, 4.0));
            ev.push((t + 0.6, 96, 90.0, 2.0));
            ev.push((t + 0.9, 103, 90.0, 2.0));
            let total = t + 5.0;
            let n = (total * SR) as usize;
            let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
            let mut offs: Vec<(usize, u8)> = Vec::new();
            let mut ons: Vec<(usize, u8, f32)> = Vec::new();
            for &(t0, k, v, d) in &ev {
                ons.push(((t0 * SR) as usize, k, v));
                offs.push((((t0 + d) * SR) as usize, k));
            }
            let mut pos = 0;
            while pos < n {
                let end = (pos + 64).min(n);
                for &(p, k, v) in &ons {
                    if p >= pos && p < end {
                        s.note_on_f(k, v);
                    }
                }
                for &(p, k) in &offs {
                    if p >= pos && p < end {
                        s.note_off(k);
                    }
                }
                s.render(&mut l[pos..end], &mut r[pos..end]);
                pos = end;
            }
            write_wav(a.get("-o").unwrap_or("demo.wav"), SR as u32, &[l, r]).unwrap();
        }
        "bench" => {
            let mut s = Synth::new(SR);
            let secs = 10.0;
            let n = (secs * SR) as usize;
            let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
            for k in (21..109).step_by(3) {
                s.note_on(k as u8, 100);
            }
            s.set_sustain(true);
            let t0 = Instant::now();
            let mut done = 0;
            let mut voice_frames = 0usize;
            while done < n {
                s.render(&mut l, &mut r);
                voice_frames += s.active_voices() * 128;
                done += 128;
            }
            let el = t0.elapsed().as_secs_f64();
            println!(
                "{:.1}x realtime with avg {:.1} voices; {:.1} ns per voice-sample",
                secs as f64 / el,
                voice_frames as f64 / n as f64,
                el * 1e9 / voice_frames as f64
            );
        }
        "spec" => {
            let key = a.num("--key", 60.0) as u8;
            let vel = a.num("--vel", 100.0);
            let v = model::voice_spec(key, vel, &PRESETS[a.num("--preset", 0.0) as usize].1, SR);
            println!("f1 = {:.3} Hz, {} stacks", v.f1, v.n);
            for s in &v.stacks[..v.n] {
                println!("{s:?}");
            }
            println!("{:?}", v.knock);
        }
        _ => {
            eprintln!("usage: fmpiano note|refset|demo|bench|spec [options]");
            std::process::exit(2);
        }
    }
}
