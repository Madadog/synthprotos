//! A fully decoded audio file held in memory.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Files the dialogs offer. The first row is MIDI; the next two decode with
/// the built-in decoders (video containers for their audio track); the rest
/// are decoded by ffmpeg, if it is installed.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mid", "midi", "kar", "rmi",
    "mp3", "mp2", "mp1", "flac", "wav", "wave", "ogg", "oga", "opus", "m4a", "m4b", "aac", "aif", "aiff", "aifc", "caf",
    "mka", "weba", "mp4", "m4v", "mov", "3gp", "mkv", "webm",
    "wma", "wv", "ape", "mpc", "tta", "ac3", "eac3", "dts", "amr", "au", "dsf", "dff", "spx", "ra", "avi", "wmv", "flv",
    "mpg", "mpeg", "ts", "mod", "xm", "it", "s3m",
];

/// The built-in decoders plus Opus.
fn codecs() -> &'static CodecRegistry {
    static CODECS: OnceLock<CodecRegistry> = OnceLock::new();
    CODECS.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_audio_decoder::<symphonia_adapter_opus_rs::OpusAdapter>();
        registry
    })
}

/// Whether the `ffmpeg` program can be run, for formats not built in.
pub fn ffmpeg_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    })
}

/// Ask ffprobe for fields of a file, e.g. `stream=sample_rate`, as JSON.
pub fn ffprobe(path: &Path, entries: &str) -> Option<serde_json::Value> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", entries, "-of", "json"])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

/// Decode any file ffmpeg understands by having it write raw samples to a pipe.
fn decode_with_ffmpeg(path: &Path, progress: &AtomicU32) -> Result<(u32, Vec<f32>), String> {
    let info = ffprobe(path, "stream=sample_rate:format=duration");
    let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    let sample_rate = info
        .as_ref()
        .and_then(|i| text(&i["streams"][0]["sample_rate"])?.parse::<u32>().ok())
        .filter(|r| (8000..=192_000).contains(r))
        .unwrap_or(44100);
    let duration = info.as_ref().and_then(|i| text(&i["format"]["duration"])?.parse::<f64>().ok());
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-sn", "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "2", "-ar"])
        .arg(sample_rate.to_string())
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot run ffmpeg: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("Cannot read from ffmpeg")?;
    let expected = duration.map(|d| d * sample_rate as f64 * 8.0);
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        let n = stdout.read(&mut chunk).map_err(|e| format!("Cannot read from ffmpeg: {e}"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(expected) = expected.filter(|e| *e > 0.0) {
            progress.store(((bytes.len() as f64 / expected).min(1.0) * 1000.0) as u32, Ordering::Relaxed);
        }
    }
    let _ = child.wait();
    let samples: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    if samples.len() < 2 {
        return Err("ffmpeg found no audio in this file".into());
    }
    Ok((sample_rate, samples[..samples.len() / 2 * 2].to_vec()))
}

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
///
/// The built-in decoders are tried first; what they cannot read is handed
/// to ffmpeg when that is installed.
pub fn decode(path: &Path, progress: &AtomicU32) -> Result<Track, String> {
    let (sample_rate, samples) = match decode_built_in(path, progress) {
        Ok(decoded) => decoded,
        Err(_) if !path.exists() => return Err("The file does not exist".into()),
        Err(error) if !ffmpeg_available() => {
            return Err(format!("{error}. Installing ffmpeg adds support for many more formats."));
        }
        Err(error) => decode_with_ffmpeg(path, progress).map_err(|e| format!("{error}; {e}"))?,
    };
    progress.store(1000, Ordering::Relaxed);
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    Ok(Track { path: path.to_path_buf(), name, sample_rate, samples })
}

fn decode_built_in(path: &Path, progress: &AtomicU32) -> Result<(u32, Vec<f32>), String> {
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
    let mut decoder = codecs()
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
    Ok((sample_rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a 440 Hz tone to `name` with ffmpeg; `None` if it cannot.
    fn encoded(name: &str, args: &[&str]) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("woodshed-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(name);
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=48000"])
            .args(args)
            .arg(&path)
            .stderr(Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    /// The decoded audio is about two seconds of a 440 Hz tone.
    fn assert_is_the_tone(track: &Track) {
        assert!((track.duration() - 2.0).abs() < 0.15, "duration {}", track.duration());
        let (from, to) = (track.sample_rate as i64 / 2, track.sample_rate as i64 * 3 / 2);
        let crossings = (from..to).filter(|&i| track.mono(i) <= 0.0 && track.mono(i + 1) > 0.0).count();
        assert!((crossings as i64 - 440).abs() <= 2, "{crossings} cycles in one second");
        let peak = (from..to).map(|i| track.mono(i).abs()).fold(0.0f32, f32::max);
        assert!((0.05..=1.0).contains(&peak), "peak {peak}");
    }

    #[test]
    fn decodes_more_formats() {
        if !ffmpeg_available() {
            return; // nothing to make test files with
        }
        // Opus is decoded in-process, in each of its usual containers.
        for name in ["tone.opus", "tone.ogg", "tone.webm", "tone.mka"] {
            let Some(path) = encoded(name, &["-c:a", "libopus", "-b:a", "96k"]) else { continue };
            let (rate, samples) = decode_built_in(&path, &AtomicU32::new(0)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let track = Track { path, name: name.into(), sample_rate: rate, samples };
            assert_is_the_tone(&track);
        }
        // Formats without a built-in decoder go through ffmpeg.
        for (name, codec) in [("tone.wma", "wmav2"), ("tone.wv", "wavpack"), ("tone.ac3", "ac3"), ("tone.tta", "tta")] {
            let Some(path) = encoded(name, &["-c:a", codec]) else { continue };
            assert!(decode_built_in(&path, &AtomicU32::new(0)).is_err(), "{name} unexpectedly built in");
            assert_is_the_tone(&decode(&path, &AtomicU32::new(0)).unwrap_or_else(|e| panic!("{name}: {e}")));
        }
        // The audio track of a video file.
        if let Some(path) = encoded("clip.mp4", &["-f", "lavfi", "-i", "color=c=black:s=64x64:d=2", "-c:a", "aac", "-shortest"]) {
            assert_is_the_tone(&decode(&path, &AtomicU32::new(0)).unwrap());
        }
        assert!(decode(Path::new("/nonexistent/file.wma"), &AtomicU32::new(0)).is_err());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("woodshed-formats-{}", std::process::id())));
    }
}
