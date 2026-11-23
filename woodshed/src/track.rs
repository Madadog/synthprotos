//! A fully decoded audio file held in memory.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mid", "midi", "mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac", "aif", "aiff", "caf", "mka", "webm",
];

pub struct Track {
    pub path: PathBuf,
    pub name: String,
    pub sample_rate: u32,
    /// Interleaved stereo samples (mono files are duplicated to both channels).
    pub samples: Vec<f32>,
}

impl Track {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Mono mix of one frame; silence outside the track.
    #[inline]
    pub fn mono(&self, frame: i64) -> f32 {
        if frame < 0 {
            return 0.0;
        }
        let i = frame as usize * 2;
        match self.samples.get(i..i + 2) {
            Some(s) => (s[0] + s[1]) * 0.5,
            None => 0.0,
        }
    }

    /// One stereo frame; silence outside the track.
    #[inline]
    pub fn stereo(&self, frame: i64) -> [f32; 2] {
        if frame < 0 {
            return [0.0; 2];
        }
        let i = frame as usize * 2;
        match self.samples.get(i..i + 2) {
            Some(s) => [s[0], s[1]],
            None => [0.0; 2],
        }
    }

    #[cfg(test)]
    pub fn from_mono(sample_rate: u32, mono: &[f32]) -> Track {
        Track {
            path: PathBuf::new(),
            name: "test".into(),
            sample_rate,
            samples: mono.iter().flat_map(|&s| [s, s]).collect(),
        }
    }
}

/// Decode a whole file. `progress` is updated with the fraction done in 1/1000.
pub fn decode(path: &Path, progress: &AtomicU32) -> Result<Track, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("Unrecognised audio format: {e}"))?;
    let track = format.default_track(TrackType::Audio).ok_or("The file has no audio track")?;
    let track_id = track.id;
    let total_frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("The file has no audio track")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("Unsupported codec: {e}"))?;

    let mut samples: Vec<f32> = Vec::new();
    if let Some(n) = total_frames {
        samples.reserve(n as usize * 2);
    }
    let mut sample_rate = 0u32;
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // Truncated files end with an IO error: keep what was decoded.
            Err(_) if !samples.is_empty() => break,
            Err(e) => return Err(format!("Cannot read audio: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let channels = audio.spec().channels().count();
                if channels == 0 {
                    continue;
                }
                sample_rate = audio.spec().rate();
                buf.clear();
                audio.copy_to_vec_interleaved(&mut buf);
                match channels {
                    1 => samples.extend(buf.iter().flat_map(|&s| [s, s])),
                    2 => samples.extend_from_slice(&buf),
                    // Surround: keep front left/right.
                    n => samples.extend(buf.chunks_exact(n).flat_map(|f| [f[0], f[1]])),
                }
                if let Some(n) = total_frames.filter(|&n| n > 0) {
                    let done = (samples.len() / 2) as f64 / n as f64;
                    progress.store((done.min(1.0) * 1000.0) as u32, Ordering::Relaxed);
                }
            }
            Err(Error::DecodeError(_)) => continue,
            Err(_) => break,
        }
    }
    if samples.is_empty() || sample_rate == 0 {
        return Err("No audio could be decoded from this file".into());
    }
    progress.store(1000, Ordering::Relaxed);
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    Ok(Track { path: path.to_path_buf(), name, sample_rate, samples })
}
