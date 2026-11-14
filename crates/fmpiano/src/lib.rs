//! # fmpiano
//!
//! An acoustic piano emulated with FM (phase-modulation) synthesis.
//!
//! * [`model`] maps key/velocity to the parameters of a voice made of
//!   2-operator FM stacks placed on the stretched (inharmonic) partials of a
//!   piano string, with envelopes fitted to recordings of a Yamaha C5 grand
//!   (Salamander Grand Piano V3, CC-BY 3.0, Alexander Holm).
//! * [`voice`] renders one note, [`synth::Synth`] handles polyphony, sustain
//!   pedal, and a small room reverb.
//!
//! The crate has no dependencies and builds for `wasm32-unknown-unknown`.

pub mod ideal;
pub mod model;
pub mod reverb;
pub mod sine;
pub mod synth;
pub mod tables;
pub mod voice;

pub use ideal::IdealParams;
pub use model::{Tone, PRESETS};
pub use synth::{param, Synth};
