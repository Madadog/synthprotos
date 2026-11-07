//! C-ABI exports for running the FM piano inside a Web Audio AudioWorklet.
//!
//! The module has no imports.  JavaScript creates an engine with
//! `fmp_new(sampleRate)`, sends note/pedal/parameter events, and calls
//! `fmp_render(engine, frames)` for every audio quantum, then reads the two
//! channel buffers at `fmp_out_left` / `fmp_out_right` (f32, `frames` long).

// Every `unsafe extern "C"` function below takes an engine pointer that must
// come from `fmp_new` and not have been passed to `fmp_free`.
#![allow(clippy::missing_safety_doc)]

use fmpiano::{Synth, PRESETS};

const MAX_FRAMES: usize = 1024;

pub struct Engine {
    synth: Synth,
    l: [f32; MAX_FRAMES],
    r: [f32; MAX_FRAMES],
}

/// # Safety
/// All functions taking `e` require a pointer returned by [`fmp_new`].
unsafe fn eng<'a>(e: *mut Engine) -> &'a mut Engine {
    &mut *e
}

#[no_mangle]
pub extern "C" fn fmp_new(sample_rate: f32) -> *mut Engine {
    Box::into_raw(Box::new(Engine {
        synth: Synth::new(sample_rate),
        l: [0.0; MAX_FRAMES],
        r: [0.0; MAX_FRAMES],
    }))
}

#[no_mangle]
pub unsafe extern "C" fn fmp_free(e: *mut Engine) {
    if !e.is_null() {
        drop(Box::from_raw(e));
    }
}

#[no_mangle]
pub unsafe extern "C" fn fmp_note_on(e: *mut Engine, key: u32, vel: f32) {
    eng(e).synth.note_on_f(key.min(127) as u8, vel);
}

#[no_mangle]
pub unsafe extern "C" fn fmp_note_off(e: *mut Engine, key: u32) {
    eng(e).synth.note_off(key.min(127) as u8);
}

#[no_mangle]
pub unsafe extern "C" fn fmp_sustain(e: *mut Engine, down: u32) {
    eng(e).synth.set_sustain(down != 0);
}

#[no_mangle]
pub unsafe extern "C" fn fmp_all_off(e: *mut Engine) {
    eng(e).synth.all_notes_off();
}

#[no_mangle]
pub unsafe extern "C" fn fmp_set_preset(e: *mut Engine, idx: u32) {
    eng(e).synth.set_preset(idx as usize);
}

#[no_mangle]
pub unsafe extern "C" fn fmp_set_param(e: *mut Engine, id: u32, value: f32) {
    eng(e).synth.set_param(id, value);
}

#[no_mangle]
pub unsafe extern "C" fn fmp_get_param(e: *mut Engine, id: u32) -> f32 {
    eng(e).synth.param(id)
}

#[no_mangle]
pub unsafe extern "C" fn fmp_active_voices(e: *mut Engine) -> u32 {
    eng(e).synth.active_voices() as u32
}

/// Render `frames` (<= 1024) samples; returns the number rendered.
#[no_mangle]
pub unsafe extern "C" fn fmp_render(e: *mut Engine, frames: u32) -> u32 {
    let en = eng(e);
    let n = (frames as usize).min(MAX_FRAMES);
    en.synth.render(&mut en.l[..n], &mut en.r[..n]);
    n as u32
}

#[no_mangle]
pub unsafe extern "C" fn fmp_out_left(e: *mut Engine) -> *const f32 {
    eng(e).l.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn fmp_out_right(e: *mut Engine) -> *const f32 {
    eng(e).r.as_ptr()
}

#[no_mangle]
pub extern "C" fn fmp_preset_count() -> u32 {
    PRESETS.len() as u32
}

#[no_mangle]
pub extern "C" fn fmp_preset_name(idx: u32) -> *const u8 {
    PRESETS
        .get(idx as usize)
        .map_or(core::ptr::null(), |p| p.0.as_ptr())
}

#[no_mangle]
pub extern "C" fn fmp_preset_name_len(idx: u32) -> u32 {
    PRESETS.get(idx as usize).map_or(0, |p| p.0.len() as u32)
}
