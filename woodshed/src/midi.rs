//! Standard MIDI files: parsing into notes for the MIDI view, and rendering
//! to audio through a SoundFont so the rest of the player (waveform,
//! spectrogram, loops, speed) treats them like any recording.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use crate::pitch::midi_to_freq;
use crate::track::Track;

/// Standard MIDI files, karaoke files (the same thing with lyrics) and
/// RIFF-wrapped MIDI.
pub const EXTENSIONS: &[&str] = &["mid", "midi", "kar", "rmi"];
/// MIDI files are rendered at this rate.
const SAMPLE_RATE: u32 = 44100;
/// Rendered after the last event, for releases and reverb to die away.
const TAIL: f64 = 2.5;
pub const DRUM_CHANNEL: u8 = 9;

pub fn is_midi(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// Seconds.
    pub start: f64,
    pub end: f64,
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
}

/// A channel message at a time, as the synthesiser wants it.
#[derive(Clone, Copy)]
struct Event {
    time: f64,
    channel: u8,
    command: u8,
    data1: u8,
    data2: u8,
}

pub struct Channel {
    pub channel: u8,
    /// General MIDI program, from the first program change.
    pub program: u8,
    pub notes: usize,
}

impl Channel {
    pub fn name(&self) -> &'static str {
        if self.channel == DRUM_CHANNEL { "Drums" } else { GM_NAMES[self.program as usize & 127] }
    }
}

pub struct MidiSong {
    /// Sorted by start time.
    pub notes: Vec<Note>,
    /// The channels that play notes.
    pub channels: Vec<Channel>,
    /// Time of the last event, seconds.
    pub length: f64,
    events: Vec<Event>,
}

impl MidiSong {
    pub fn duration(&self) -> f64 {
        self.length + TAIL
    }

    pub fn parse(bytes: &[u8]) -> Result<MidiSong, String> {
        // A RIFF MIDI file (.rmi) is a standard file inside a wrapper.
        let wrapped = bytes.starts_with(b"RIFF").then(|| bytes.windows(4).position(|w| w == b"MThd")).flatten();
        let bytes = &bytes[wrapped.unwrap_or(0)..];
        let smf = Smf::parse(bytes).map_err(|e| format!("Not a valid MIDI file: {e}"))?;
        // Merge the tracks into one stream ordered by tick.
        let mut merged: Vec<(u64, usize, TrackEventKind)> = Vec::new();
        for (index, track) in smf.tracks.iter().enumerate() {
            let mut tick = 0u64;
            for event in track {
                tick += event.delta.as_int() as u64;
                merged.push((tick, index, event.kind));
            }
        }
        merged.sort_by_key(|e| (e.0, e.1));

        let mut seconds_per_tick = match smf.header.timing {
            Timing::Metrical(per_beat) => 0.5 / per_beat.as_int().max(1) as f64,
            Timing::Timecode(fps, subframe) => 1.0 / (fps.as_f32() as f64 * subframe.max(1) as f64),
        };
        let (mut time, mut last_tick) = (0.0f64, 0u64);
        let mut events = Vec::new();
        let mut notes: Vec<Note> = Vec::new();
        // Indices into `notes` of the notes still held, per channel and key.
        let mut held: Vec<Vec<usize>> = vec![Vec::new(); 16 * 128];
        let mut programs = [None::<u8>; 16];
        for (tick, _, kind) in merged {
            time += (tick - last_tick) as f64 * seconds_per_tick;
            last_tick = tick;
            match kind {
                TrackEventKind::Meta(MetaMessage::Tempo(micros)) => {
                    if let Timing::Metrical(per_beat) = smf.header.timing {
                        seconds_per_tick = micros.as_int() as f64 / 1e6 / per_beat.as_int().max(1) as f64;
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let channel = channel.as_int();
                    let mut push = |command: u8, data1: u8, data2: u8| {
                        events.push(Event { time, channel, command, data1, data2 });
                    };
                    let slot = |key: u8| channel as usize * 128 + key as usize;
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let key = key.as_int();
                            push(0x90, key, vel.as_int());
                            held[slot(key)].push(notes.len());
                            notes.push(Note { start: time, end: f64::NAN, key, velocity: vel.as_int(), channel });
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let key = key.as_int();
                            push(0x80, key, 0);
                            if let Some(index) = held[slot(key)].pop() {
                                notes[index].end = time;
                            }
                        }
                        MidiMessage::Controller { controller, value } => push(0xB0, controller.as_int(), value.as_int()),
                        MidiMessage::ProgramChange { program } => {
                            push(0xC0, program.as_int(), 0);
                            programs[channel as usize].get_or_insert(program.as_int());
                        }
                        MidiMessage::PitchBend { bend } => {
                            let value = bend.0.as_int();
                            push(0xE0, (value & 0x7F) as u8, (value >> 7) as u8);
                        }
                        MidiMessage::Aftertouch { .. } | MidiMessage::ChannelAftertouch { .. } => {}
                    }
                }
                _ => {}
            }
        }
        // Notes never released last to the end.
        for note in notes.iter_mut().filter(|n| n.end.is_nan()) {
            note.end = time;
        }
        if notes.is_empty() {
            return Err("The MIDI file contains no notes".into());
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let channels = (0..16u8)
            .map(|c| Channel {
                channel: c,
                program: programs[c as usize].unwrap_or(0),
                notes: notes.iter().filter(|n| n.channel == c).count(),
            })
            .filter(|c| c.notes > 0)
            .collect();
        Ok(MidiSong { notes, channels, length: time, events })
    }

    /// Render to interleaved stereo. Without a SoundFont, a plain built-in
    /// tone stands in so the file is still playable.
    fn render(&self, font: Option<&Arc<SoundFont>>, progress: &AtomicU32) -> Vec<f32> {
        let rate = SAMPLE_RATE as f64;
        let total = (self.duration() * rate) as usize;
        let synth = font.and_then(|f| Synthesizer::new(f, &SynthesizerSettings::new(SAMPLE_RATE as i32)).ok());
        let Some(mut synth) = synth else { return self.render_basic(total, progress) };
        let mut out = Vec::with_capacity(total * 2);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        let mut done = 0usize;
        let mut render_to = |synth: &mut Synthesizer, until: usize, out: &mut Vec<f32>| {
            while done < until {
                let n = (until - done).min(4096);
                left.resize(n, 0.0);
                right.resize(n, 0.0);
                synth.render(&mut left, &mut right);
                out.extend(left.iter().zip(&right).flat_map(|(l, r)| [*l, *r]));
                done += n;
                progress.store((done as f64 / total as f64 * 1000.0) as u32, Ordering::Relaxed);
            }
        };
        for event in &self.events {
            render_to(&mut synth, ((event.time * rate) as usize).min(total), &mut out);
            synth.process_midi_message(event.channel as i32, event.command as i32, event.data1 as i32, event.data2 as i32);
        }
        render_to(&mut synth, total, &mut out);
        out
    }

    fn render_basic(&self, total: usize, progress: &AtomicU32) -> Vec<f32> {
        let rate = SAMPLE_RATE as f64;
        let mut out = vec![0.0f32; total * 2];
        let mut noise = 0x9E3779B9u32;
        for (index, note) in self.notes.iter().enumerate() {
            let first = (note.start * rate) as usize;
            let level = note.velocity as f32 / 127.0 * 0.11;
            if note.channel == DRUM_CHANNEL {
                // A short burst of noise for every drum hit.
                for i in 0..(0.05 * rate) as usize {
                    noise ^= noise << 13;
                    noise ^= noise >> 17;
                    noise ^= noise << 5;
                    let s = (noise as f32 / u32::MAX as f32 - 0.5) * level * (-(i as f32) / (0.012 * rate as f32)).exp();
                    if let Some(frame) = out.get_mut((first + i) * 2..(first + i) * 2 + 2) {
                        frame[0] += s;
                        frame[1] += s;
                    }
                }
                continue;
            }
            // A plucked tone: a few harmonics, decaying while held, short release.
            let step = midi_to_freq(note.key as f64) / rate;
            let held = ((note.end - note.start).max(0.03) * rate) as usize;
            let release = (0.08 * rate) as usize;
            for i in 0..held + release {
                let Some(frame) = out.get_mut((first + i) * 2..(first + i) * 2 + 2) else { break };
                let phase = (step * i as f64).fract() as f32 * std::f32::consts::TAU;
                let attack = (i as f32 / (0.004 * rate as f32)).min(1.0);
                let decay = (-(i as f32) / (1.5 * rate as f32)).exp();
                let fade = if i > held { 1.0 - (i - held) as f32 / release as f32 } else { 1.0 };
                let wave = phase.sin() + 0.35 * (2.0 * phase).sin() + 0.15 * (3.0 * phase).sin();
                let s = wave * level * attack * decay * fade;
                frame[0] += s;
                frame[1] += s;
            }
            if index % 256 == 0 {
                progress.store((index as f64 / self.notes.len() as f64 * 1000.0) as u32, Ordering::Relaxed);
            }
        }
        out.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
        out
    }
}

/// Load a MIDI file: its notes, and its audio rendered through `font`.
pub fn load(path: &Path, font: Option<&Arc<SoundFont>>, progress: &AtomicU32) -> Result<(Track, MidiSong), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let song = MidiSong::parse(&bytes)?;
    let samples = song.render(font, progress);
    progress.store(1000, Ordering::Relaxed);
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled".into());
    Ok((Track { path: path.to_path_buf(), name, sample_rate: SAMPLE_RATE, samples }, song))
}

/// Length in seconds of a MIDI file, for the playlist.
pub fn duration(path: &Path) -> Option<f64> {
    Some(MidiSong::parse(&std::fs::read(path).ok()?).ok()?.duration())
}

pub fn load_soundfont(path: &Path) -> Result<Arc<SoundFont>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open SoundFont: {e}"))?;
    let font = SoundFont::new(&mut std::io::BufReader::new(file)).map_err(|e| format!("Cannot read SoundFont: {e}"))?;
    Ok(Arc::new(font))
}

/// A General MIDI SoundFont installed with the system, if there is one.
pub fn system_soundfont() -> Option<PathBuf> {
    [
        "/usr/share/sounds/sf2/default-GM.sf2",
        "/usr/share/sounds/sf2/FluidR3_GM.sf2",
        "/usr/share/sounds/sf2/TimGM6mb.sf2",
        "/usr/share/soundfonts/default.sf2",
        "/usr/share/soundfonts/FluidR3_GM.sf2",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

pub const GM_NAMES: [&str; 128] = [
    "Grand Piano", "Bright Piano", "Electric Grand", "Honky-tonk Piano", "Electric Piano 1", "Electric Piano 2",
    "Harpsichord", "Clavinet", "Celesta", "Glockenspiel", "Music Box", "Vibraphone", "Marimba", "Xylophone",
    "Tubular Bells", "Dulcimer", "Drawbar Organ", "Percussive Organ", "Rock Organ", "Church Organ", "Reed Organ",
    "Accordion", "Harmonica", "Tango Accordion", "Nylon Guitar", "Steel Guitar", "Jazz Guitar", "Clean Guitar",
    "Muted Guitar", "Overdriven Guitar", "Distortion Guitar", "Guitar Harmonics", "Acoustic Bass", "Finger Bass",
    "Pick Bass", "Fretless Bass", "Slap Bass 1", "Slap Bass 2", "Synth Bass 1", "Synth Bass 2", "Violin", "Viola",
    "Cello", "Contrabass", "Tremolo Strings", "Pizzicato Strings", "Harp", "Timpani", "String Ensemble 1",
    "String Ensemble 2", "Synth Strings 1", "Synth Strings 2", "Choir Aahs", "Voice Oohs", "Synth Voice",
    "Orchestra Hit", "Trumpet", "Trombone", "Tuba", "Muted Trumpet", "French Horn", "Brass Section", "Synth Brass 1",
    "Synth Brass 2", "Soprano Sax", "Alto Sax", "Tenor Sax", "Baritone Sax", "Oboe", "English Horn", "Bassoon",
    "Clarinet", "Piccolo", "Flute", "Recorder", "Pan Flute", "Blown Bottle", "Shakuhachi", "Whistle", "Ocarina",
    "Square Lead", "Saw Lead", "Calliope Lead", "Chiff Lead", "Charang Lead", "Voice Lead", "Fifths Lead",
    "Bass + Lead", "New Age Pad", "Warm Pad", "Polysynth Pad", "Choir Pad", "Bowed Pad", "Metallic Pad", "Halo Pad",
    "Sweep Pad", "Rain", "Soundtrack", "Crystal", "Atmosphere", "Brightness", "Goblins", "Echoes", "Sci-fi",
    "Sitar", "Banjo", "Shamisen", "Koto", "Kalimba", "Bagpipe", "Fiddle", "Shanai", "Tinkle Bell", "Agogo",
    "Steel Drums", "Woodblock", "Taiko Drum", "Melodic Tom", "Synth Drum", "Reverse Cymbal", "Guitar Fret Noise",
    "Breath Noise", "Seashore", "Bird Tweet", "Telephone Ring", "Helicopter", "Applause", "Gunshot",
];

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A two-track file at 480 ticks per beat: a tempo change, a flute on
    /// channel 1 and a drum hit on channel 10.
    pub fn sample_file() -> Vec<u8> {
        let mut file = Vec::new();
        file.extend(b"MThd");
        file.extend([0, 0, 0, 6, 0, 1, 0, 2, 0x01, 0xE0]);
        let tracks: [&[u8]; 2] = [
            &[
                0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // 120 bpm: 0.5 s per beat
                0x00, 0xC0, 73, // channel 1: flute
                0x00, 0x90, 60, 100, // C4 at 0 s
                0x83, 0x60, 0x80, 60, 0, // one beat later (0.5 s): off
                0x00, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40, // 60 bpm: 1 s per beat
                0x00, 0x90, 64, 90, // E4 at 0.5 s
                0x83, 0x60, 0x90, 64, 0, // one beat (1 s) later: note-on with velocity 0
                0x00, 0xFF, 0x2F, 0x00,
            ],
            &[0x87, 0x40, 0x99, 36, 127, 0x3C, 0x89, 36, 0, 0x00, 0xFF, 0x2F, 0x00], // drum at beat 2
        ];
        for track in tracks {
            file.extend(b"MTrk");
            file.extend((track.len() as u32).to_be_bytes());
            file.extend(track);
        }
        file
    }

    #[test]
    fn parses_notes_and_tempo() {
        let song = MidiSong::parse(&sample_file()).unwrap();
        let brief: Vec<_> = song.notes.iter().map(|n| (n.channel, n.key, n.start, n.end)).collect();
        // The drum at tick 960: one beat at 120 bpm, then one at 60 bpm.
        assert_eq!(brief, [(0, 60, 0.0, 0.5), (0, 64, 0.5, 1.5), (9, 36, 1.5, 1.625)]);
        let channels: Vec<_> = song.channels.iter().map(|c| (c.channel, c.name(), c.notes)).collect();
        assert_eq!(channels, [(0, "Flute", 2), (9, "Drums", 1)]);
        assert!((song.length - 1.625).abs() < 1e-9);
        assert!(MidiSong::parse(b"RIFF....").is_err());
        // The same file in a RIFF wrapper.
        let mut rmi = b"RIFF\x00\x00\x00\x00RMIDdata\x00\x00\x00\x00".to_vec();
        rmi.extend(sample_file());
        assert_eq!(MidiSong::parse(&rmi).unwrap().notes, song.notes);
    }

    #[test]
    fn basic_synth_plays_the_notes() {
        let song = MidiSong::parse(&sample_file()).unwrap();
        let audio = song.render(None, &AtomicU32::new(0));
        assert_eq!(audio.len(), (song.duration() * SAMPLE_RATE as f64) as usize * 2);
        let rms = |from: f64, to: f64| {
            let s = &audio[(from * 44100.0) as usize * 2..(to * 44100.0) as usize * 2];
            (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
        };
        assert!(rms(0.1, 0.4) > 0.02 && rms(0.6, 1.4) > 0.02);
        assert!(rms(2.5, 4.0) == 0.0);
    }

    #[test]
    fn soundfont_render_if_one_is_installed() {
        let Some(path) = system_soundfont() else { return };
        let font = load_soundfont(&path).unwrap();
        let song = MidiSong::parse(&sample_file()).unwrap();
        let audio = song.render(Some(&font), &AtomicU32::new(0));
        assert_eq!(audio.len(), (song.duration() * SAMPLE_RATE as f64) as usize * 2);
        assert!(audio.iter().any(|s| s.abs() > 0.01));
    }
}
