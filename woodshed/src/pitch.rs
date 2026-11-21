//! Pitch helpers: MIDI note numbers, frequencies and note names.

const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Frequency in Hz of a (fractional) MIDI note number, A4 = 69 = 440 Hz.
pub fn midi_to_freq(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// Name with octave of a MIDI note, e.g. 60 -> "C4".
pub fn note_name(midi: i32) -> String {
    format!("{}{}", NAMES[midi.rem_euclid(12) as usize], midi.div_euclid(12) - 1)
}

pub fn is_black_key(midi: i32) -> bool {
    matches!(midi.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// "A4 +12c" style description of a fractional MIDI pitch.
pub fn describe(midi: f64) -> String {
    let nearest = midi.round();
    let cents = ((midi - nearest) * 100.0).round() as i32;
    format!("{} {:+}¢", note_name(nearest as i32), cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_frequencies() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(21), "A0");
        assert_eq!(note_name(108), "C8");
        assert!((midi_to_freq(69.0) - 440.0).abs() < 1e-9);
        assert!((midi_to_freq(57.0) - 220.0).abs() < 1e-9);
        assert!(is_black_key(61) && !is_black_key(60));
        assert_eq!(describe(69.12), "A4 +12¢");
    }
}
