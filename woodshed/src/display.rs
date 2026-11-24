//! The zoomable display: time ruler with bookmarks and loop region, waveform,
//! and the pitch-axis spectrogram with its piano and hover readout.
//!
//! Two layouts share this code. Normally time runs left to right with the
//! piano as a ribbon down the left. In piano mode the piano lies along the
//! bottom (low notes on the left) and time runs vertically above it.

use std::collections::HashMap;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Painter, PointerButton, Pos2,
    Rangef, Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui,
};

use crate::analysis::{Peaks, Spectrogram, BLOCK, DB_RANGE};
use crate::app::{format_time, App, View, ACCENT, BOOKMARK, LOOP};
use crate::midi::{MidiSong, Note};
use crate::pitch;
use crate::session::{Channel, FreqScale, LevelScale, LoopMode, Settings, Theme, ViewMode};
use crate::track::Track;

const RULER_HEIGHT: f32 = 38.0;
const RULER_WIDTH: f32 = 76.0;
const FLAG_HEIGHT: f32 = 15.0;
const RIBBON_WIDTH: f32 = 54.0;
const PIANO_HEIGHT: f32 = 68.0;
const BACKGROUND: Color32 = Color32::from_rgb(14, 14, 18);
const RULER_BACKGROUND: Color32 = Color32::from_rgb(28, 28, 34);
/// Pointer distance within which an edge or divider can be grabbed.
const GRAB: f32 = 5.0;

/// What a pointer drag in the display is doing.
pub enum Drag {
    None,
    /// Dragging the playhead along.
    Scrub,
    /// Selecting a loop region from `anchor` seconds.
    Select { anchor: f64 },
    /// Moving one edge of the selection.
    Edge { start: bool },
    /// Moving a bookmark.
    Bookmark { id: u64 },
    /// Panning time and pitch.
    Pan,
    /// Playing notes on the piano.
    Piano,
    /// Resizing the waveform / spectrogram split.
    Divider,
}

/// A linear map from a value range to screen coordinates along one direction.
/// `length` is signed, so the axis can run either way.
#[derive(Clone, Copy)]
struct Axis {
    origin: f32,
    length: f32,
    lo: f64,
    hi: f64,
    /// How values are spread along the axis. Time is always even; pitch can
    /// be laid out by pitch, mel or hertz.
    warp: FreqScale,
}

impl Axis {
    fn at(&self, value: f64) -> f32 {
        let (w, lo, hi) = (self.warp.warp(value), self.warp.warp(self.lo), self.warp.warp(self.hi));
        self.origin + ((w - lo) / (hi - lo)) as f32 * self.length
    }

    fn value(&self, coord: f32) -> f64 {
        let (lo, hi) = (self.warp.warp(self.lo), self.warp.warp(self.hi));
        self.warp.unwarp(lo + ((coord - self.origin) / self.length) as f64 * (hi - lo))
    }

    fn size(&self) -> f32 {
        self.length.abs()
    }

    /// Screen distance per unit of value, signed (the average, on a warped axis).
    fn scale(&self) -> f64 {
        self.length as f64 / (self.hi - self.lo)
    }

    /// Screen size of one unit of value around `value`.
    fn unit_size(&self, value: f64) -> f32 {
        (self.at(value + 0.5) - self.at(value - 0.5)).abs()
    }

    /// The range after sliding the axis's contents by `pixels`.
    fn panned(&self, pixels: f32) -> (f64, f64) {
        (self.value(self.origin - pixels), self.value(self.origin + self.length - pixels))
    }

    /// The range after scaling by `factor` (> 1 shows more) about the value at `coord`.
    fn zoomed(&self, coord: f32, factor: f32) -> (f64, f64) {
        let ends = [self.origin, self.origin + self.length];
        let [lo, hi] = ends.map(|end| self.value(coord + (end - coord) * factor));
        (lo, hi)
    }
}

/// Where everything is, for one frame.
#[derive(Clone, Copy)]
struct Geometry {
    /// Time runs vertically (piano mode).
    vertical: bool,
    time: Axis,
    pitch: Option<Axis>,
    ruler: Rect,
    /// Waveform and spectrogram together.
    body: Rect,
    wave: Option<Rect>,
    spec: Option<Rect>,
    piano: Option<Rect>,
    /// Ruler and body: everything with a time coordinate.
    timed: Rect,
}

impl Geometry {
    fn new(full: Rect, settings: &Settings, view: &View) -> Geometry {
        let mode = settings.view;
        let has_spec = mode != ViewMode::Waveform;
        let vertical = settings.piano_mode && has_spec;
        let split = if mode == ViewMode::Both { settings.split.clamp(0.1, 0.8) } else { 0.0 };
        let (ruler, body, piano);
        if vertical {
            let bottom = full.bottom() - PIANO_HEIGHT;
            ruler = Rect::from_min_max(full.min, pos2(full.left() + RULER_WIDTH, bottom));
            body = Rect::from_min_max(pos2(ruler.right(), full.top()), pos2(full.right(), bottom));
        } else {
            let left = full.left() + if has_spec { RIBBON_WIDTH } else { 0.0 };
            ruler = Rect::from_min_max(pos2(left, full.top()), pos2(full.right(), full.top() + RULER_HEIGHT));
            body = Rect::from_min_max(pos2(left, ruler.bottom()), full.max);
        }
        // The waveform takes the first `split` of the body, across the time axis.
        let cut = if vertical { body.left() + body.width() * split } else { body.top() + body.height() * split };
        let (first, second) = if vertical {
            (Rect::from_min_max(body.min, pos2(cut, body.bottom())), Rect::from_min_max(pos2(cut + 1.0, body.top()), body.max))
        } else {
            (Rect::from_min_max(body.min, pos2(body.right(), cut)), Rect::from_min_max(pos2(body.left(), cut + 1.0), body.max))
        };
        let (wave, spec) = match mode {
            ViewMode::Waveform => (Some(body), None),
            ViewMode::Both => (Some(first), Some(second)),
            _ => (None, Some(body)),
        };
        piano = spec.map(|s| {
            if vertical {
                Rect::from_min_max(pos2(s.left(), s.bottom()), pos2(s.right(), full.bottom()))
            } else {
                Rect::from_min_max(pos2(full.left(), s.top()), pos2(body.left(), s.bottom()))
            }
        });
        let (lo, hi) = (view.start, view.start + view.span);
        let time = if !vertical {
            Axis { origin: body.left(), length: body.width().max(10.0), lo, hi, warp: FreqScale::Pitch }
        } else if settings.reverse {
            // The past rises from the piano: later is lower.
            Axis { origin: body.top(), length: body.height().max(10.0), lo, hi, warp: FreqScale::Pitch }
        } else {
            // What is coming falls towards the piano: later is higher.
            Axis { origin: body.bottom(), length: -body.height().max(10.0), lo, hi, warp: FreqScale::Pitch }
        };
        let pitch = spec.map(|s| {
            let (lo, hi) = (view.pitch_lo, view.pitch_hi);
            if vertical {
                Axis { origin: s.left(), length: s.width(), lo, hi, warp: settings.freq_scale }
            } else {
                Axis { origin: s.bottom(), length: -s.height(), lo, hi, warp: settings.freq_scale }
            }
        });
        Geometry { vertical, time, pitch, ruler, body, wave, spec, piano, timed: ruler.union(body) }
    }

    /// Coordinate along the time axis.
    fn along(&self, p: Pos2) -> f32 {
        if self.vertical { p.y } else { p.x }
    }

    /// Coordinate across the time axis (the pitch direction).
    fn across(&self, p: Pos2) -> f32 {
        if self.vertical { p.x } else { p.y }
    }

    fn point(&self, along: f32, across: f32) -> Pos2 {
        if self.vertical { pos2(across, along) } else { pos2(along, across) }
    }

    fn along_range(&self, rect: Rect) -> Rangef {
        if self.vertical { rect.y_range() } else { rect.x_range() }
    }

    fn across_range(&self, rect: Rect) -> Rangef {
        if self.vertical { rect.x_range() } else { rect.y_range() }
    }

    fn time_at(&self, p: Pos2) -> f64 {
        self.time.value(self.along(p))
    }

    fn pitch_at(&self, p: Pos2) -> Option<f64> {
        Some(self.pitch?.value(self.across(p)))
    }

    /// A line at one moment in time, spanning `across`.
    fn time_line(&self, painter: &Painter, along: f32, across: impl Into<Rangef>, stroke: impl Into<Stroke>) {
        if self.vertical {
            painter.hline(across, along, stroke);
        } else {
            painter.vline(along, across, stroke);
        }
    }

    /// A line at one pitch (or amplitude), spanning `along` in time.
    fn cross_line(&self, painter: &Painter, across: f32, along: impl Into<Rangef>, stroke: impl Into<Stroke>) {
        if self.vertical {
            painter.vline(across, along, stroke);
        } else {
            painter.hline(along, across, stroke);
        }
    }

    /// The rectangle between two time coordinates, spanning `across`.
    fn band(&self, a: f32, b: f32, across: Rangef) -> Rect {
        let along = Rangef::new(a.min(b), a.max(b));
        if self.vertical { Rect::from_x_y_ranges(across, along) } else { Rect::from_x_y_ranges(along, across) }
    }

    /// The divider between waveform and spectrogram, as a grab area.
    fn divider(&self) -> Option<Rect> {
        let (wave, _) = (self.wave?, self.spec?);
        Some(if self.vertical {
            Rect::from_x_y_ranges(wave.right() - GRAB..=wave.right() + GRAB + 1.0, wave.y_range())
        } else {
            Rect::from_x_y_ranges(wave.x_range(), wave.bottom() - GRAB..=wave.bottom() + GRAB + 1.0)
        })
    }
}

struct Flag {
    id: u64,
    time: f64,
    rect: Rect,
    galley: std::sync::Arc<egui::Galley>,
}

impl App {
    /// Whether the display currently runs time vertically (piano mode).
    pub fn vertical(&self) -> bool {
        self.settings.piano_mode && self.settings.view != ViewMode::Waveform
    }

    /// Ctrl+click: place a loop point. The first click starts a region, the
    /// second completes it; after that, clicks move whichever end is nearer.
    fn place_loop_point(&mut self, t: f64) {
        match (self.session.selection, self.loop_anchor.take()) {
            (Some((a, b)), _) => {
                let (a, b) = if (t - a).abs() < (t - b).abs() { (t, b) } else { (a, t) };
                self.session.selection = Some((a.min(b), a.max(b)));
            }
            (None, Some(anchor)) if anchor != t => {
                self.session.selection = Some((anchor.min(t), anchor.max(t)));
            }
            (None, _) => {
                self.loop_anchor = Some(t);
                return;
            }
        }
        self.settings.loop_mode = LoopMode::Selection;
        self.touch_session();
    }

    pub fn display(&mut self, ui: &mut Ui) {
        let Some(loaded) = &self.loaded else { return };
        let (track, peaks, spectrogram) = (loaded.track.clone(), loaded.peaks.clone(), loaded.spectrogram.clone());
        let duration = track.duration();
        let now = ui.input(|i| i.time);
        // The MIDI view shows the file's notes where the spectrogram would be.
        let song = loaded.midi.clone().filter(|_| self.settings.view == ViewMode::Midi);
        if let Some(song) = &song {
            self.midi_legend(ui, song);
        }

        let full = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(full, Sense::click_and_drag());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, CornerRadius::ZERO, BACKGROUND);

        // -------------------------------------------------------- input
        let g = Geometry::new(full, &self.settings, &self.view);
        let flags = self.flags(&painter, &g);
        let flag_at = |p: Pos2| flags.iter().rev().find(|f| f.rect.contains(p)).map(|f| (f.id, f.time));
        let edge_at = |p: Pos2, selection: Option<(f64, f64)>| {
            let (a, b) = selection?;
            if !g.timed.contains(p) {
                return None;
            }
            let (da, db) = ((g.time.at(a) - g.along(p)).abs(), (g.time.at(b) - g.along(p)).abs());
            (da.min(db) <= GRAB).then_some(da < db)
        };
        let over_piano = |p: Pos2| g.piano.is_some_and(|r| r.contains(p));
        let over_divider = |p: Pos2| g.divider().is_some_and(|r| r.contains(p));
        // The oscilloscope has no time axis of its own to click on.
        let scope = g.wave.filter(|_| self.settings.oscilloscope);
        let over_scope = |p: Pos2| scope.is_some_and(|r| r.contains(p));
        let hover = response.hover_pos();
        let pointer = response.interact_pointer_pos();
        let (origin, primary, ctrl) =
            ui.input(|i| (i.pointer.press_origin(), i.pointer.button_down(PointerButton::Primary), i.modifiers.command));

        if response.drag_started() {
            let origin = origin.or(hover).unwrap_or(full.center());
            self.drag = if !primary {
                Drag::Pan
            } else if over_piano(origin) {
                Drag::Piano
            } else if over_divider(origin) {
                Drag::Divider
            } else if over_scope(origin) {
                Drag::None
            } else if let Some((id, _)) = flag_at(origin) {
                Drag::Bookmark { id }
            } else if let Some(start) = edge_at(origin, self.session.selection) {
                Drag::Edge { start }
            } else if !g.timed.contains(origin) {
                Drag::None
            } else if ctrl {
                Drag::Select { anchor: g.time_at(origin).clamp(0.0, duration) }
            } else {
                Drag::Scrub
            };
        }
        if response.dragged() {
            let delta = response.drag_delta();
            let t = pointer.map(|p| g.time_at(p).clamp(0.0, duration));
            match (&mut self.drag, t) {
                (Drag::Scrub, Some(t)) => self.seek(t),
                (Drag::Select { anchor }, Some(t)) => {
                    self.session.selection = Some((anchor.min(t), anchor.max(t)));
                    self.settings.loop_mode = LoopMode::Selection;
                    self.loop_anchor = None;
                    self.touch_session();
                }
                (Drag::Edge { start }, Some(t)) => {
                    if let Some((a, b)) = self.session.selection {
                        let (a, b) = if *start { (t, b) } else { (a, t) };
                        if a > b {
                            *start = !*start;
                        }
                        self.session.selection = Some((a.min(b), a.max(b)));
                        self.touch_session();
                    }
                }
                (Drag::Bookmark { id }, Some(t)) => {
                    if let Some(bookmark) = self.session.bookmarks.iter_mut().find(|b| b.id == *id) {
                        bookmark.time = t;
                    }
                    self.session.sort();
                    self.touch_session();
                }
                (Drag::Pan, _) => {
                    self.view.start -= g.along(delta.to_pos2()) as f64 / g.time.scale();
                    if let Some(pitch) = g.pitch {
                        (self.view.pitch_lo, self.view.pitch_hi) = pitch.panned(g.across(delta.to_pos2()));
                    }
                    self.last_pan = now;
                }
                (Drag::Divider, _) => {
                    if let Some(p) = pointer {
                        let range = g.across_range(g.body);
                        self.settings.split = ((g.across(p) - range.min) / range.span()).clamp(0.1, 0.8);
                    }
                }
                _ => {}
            }
        }
        if response.drag_stopped() {
            if let (Drag::Select { .. }, Some((a, b))) = (&self.drag, self.session.selection) {
                if (g.time.at(b) - g.time.at(a)).abs() < 3.0 {
                    self.session.selection = None;
                }
            }
            self.drag = Drag::None;
        }
        if let Some(p) = pointer.filter(|_| response.clicked()) {
            if over_piano(p) || over_divider(p) || over_scope(p) {
                // Handled as a note, or nothing.
            } else if let Some((_, time)) = flag_at(p) {
                self.seek(time);
            } else if g.timed.contains(p) {
                let t = g.time_at(p).clamp(0.0, duration);
                if ctrl {
                    self.place_loop_point(t);
                } else {
                    self.seek(t);
                }
            }
        }
        if let Some((id, _)) = hover.filter(|_| response.double_clicked()).and_then(flag_at) {
            self.focus_label = Some(id);
        }

        // A key held on the piano sounds for as long as the button is down.
        let key_down = pointer
            .filter(|_| primary && response.is_pointer_button_down_on() && origin.is_some_and(over_piano))
            .and_then(|p| g.pitch_at(p))
            .map(|pitch| pitch.round() as i32);
        if key_down != self.sounding {
            self.sounding = key_down;
            self.set_key(key_down);
        }

        if let Some(p) = hover {
            let (scroll, pinch, alt) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta() as f64, i.modifiers.alt));
            let zoom_pitch = |view: &mut View, factor: f64| {
                if let Some(pitch) = g.pitch {
                    (view.pitch_lo, view.pitch_hi) = pitch.zoomed(g.across(p), factor as f32);
                }
            };
            let wheel = (-scroll.y as f64 * 0.004).exp();
            if over_piano(p) {
                if let Some(pitch) = g.pitch {
                    // The wheel moves towards higher notes; sideways scrolling drags the keys.
                    let pixels = -scroll.y * pitch.length.signum() + scroll.x;
                    (self.view.pitch_lo, self.view.pitch_hi) = pitch.panned(pixels);
                }
                zoom_pitch(&mut self.view, 1.0 / pinch);
            } else if alt && g.spec.is_some_and(|r| r.contains(p)) {
                zoom_pitch(&mut self.view, wheel);
            } else {
                if scroll.y != 0.0 || pinch != 1.0 {
                    self.zoom_time(wheel / pinch, g.time_at(p).clamp(self.view.start, self.view.start + self.view.span));
                }
                if scroll.x != 0.0 {
                    self.view.start -= scroll.x as f64 / g.time.scale().abs();
                    self.last_pan = now;
                }
            }
            // Cursor hints.
            let (resize_time, resize_split) = if g.vertical {
                (CursorIcon::ResizeVertical, CursorIcon::ResizeHorizontal)
            } else {
                (CursorIcon::ResizeHorizontal, CursorIcon::ResizeVertical)
            };
            match self.drag {
                Drag::Edge { .. } => ui.ctx().set_cursor_icon(resize_time),
                Drag::Divider => ui.ctx().set_cursor_icon(resize_split),
                Drag::Bookmark { .. } | Drag::Pan => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
                Drag::None if over_piano(p) => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
                Drag::None if over_divider(p) => ui.ctx().set_cursor_icon(resize_split),
                Drag::None if flag_at(p).is_some() => ui.ctx().set_cursor_icon(CursorIcon::PointingHand),
                Drag::None if edge_at(p, self.session.selection).is_some() => ui.ctx().set_cursor_icon(resize_time),
                _ => {}
            }
        }
        self.clamp_view();

        // ------------------------------------------------------ drawing
        let g = Geometry::new(full, &self.settings, &self.view);
        let content = painter.with_clip_rect(g.timed);
        let theme = self.settings.theme;
        let light = theme.is_light() && song.is_none();
        if let Some(rect) = g.wave {
            // One lane per channel shown, side by side across the time axis.
            let lanes = self.settings.stereo.wave.lanes();
            for (index, &channel) in lanes.iter().enumerate() {
                let lane = lane(rect, index, lanes.len(), !g.vertical);
                let clipped = content.with_clip_rect(lane);
                draw_waveform(&clipped, lane, &g, &track, &peaks, &self.settings, channel);
                if lanes.len() > 1 {
                    lane_label(&clipped, lane, channel);
                }
            }
        }
        if let (Some(rect), Some(pitch)) = (g.spec, g.pitch) {
            let clipped = content.with_clip_rect(rect);
            match &song {
                Some(song) => draw_midi(&clipped, &g, rect, pitch, song, self.midi_hidden),
                None => self.tiles.draw(ui.ctx(), &clipped, &g, &spectrogram, &self.settings, now),
            }
            let grid = |alpha: u8| {
                if light { Color32::from_black_alpha(alpha * 2) } else { Color32::from_white_alpha(alpha) }
            };
            let along = g.along_range(rect);
            if self.settings.semitone_lines {
                // Only where the notes are far enough apart to tell the lines from shading.
                for midi in (pitch.lo.floor() as i32..=pitch.hi.ceil() as i32).filter(|m| pitch.unit_size(*m as f64) >= 5.0) {
                    g.cross_line(&clipped, pitch.at(midi as f64 - 0.5), along, Stroke::new(1.0, grid(9)));
                }
            }
            for midi in (12..=120).step_by(12).filter(|_| self.settings.octave_lines) {
                g.cross_line(&clipped, pitch.at(midi as f64 - 0.5), along, Stroke::new(1.0, grid(22)));
            }
            if !spectrogram.is_complete() && song.is_none() {
                clipped.text(
                    rect.right_top() + vec2(-8.0, 6.0),
                    Align2::RIGHT_TOP,
                    format!("Analysing… {:.0}%", spectrogram.progress() * 100.0),
                    FontId::proportional(12.0),
                    grid(85),
                );
            }
        }
        if let Some(divider) = g.divider() {
            let hot = matches!(self.drag, Drag::Divider) || hover.is_some_and(|p| divider.contains(p));
            let colour = if hot { Color32::from_white_alpha(120) } else { RULER_BACKGROUND };
            let centre = g.across(divider.center());
            g.cross_line(&painter, centre, g.along_range(divider), Stroke::new(if hot { 2.0 } else { 1.0 }, colour));
        }

        // Selection and active loop region.
        let across = g.across_range(g.body);
        let active = match self.settings.loop_mode {
            LoopMode::Off => None,
            LoopMode::Selection => self.session.selection,
            LoopMode::Section => Some(self.session.section(self.pos, duration)),
            LoopMode::Track => Some((0.0, duration)),
        };
        if let Some((a, b)) = self.session.selection.filter(|s| Some(*s) != active) {
            let band = g.band(g.time.at(a), g.time.at(b), across);
            content.rect_filled(band, CornerRadius::ZERO, Color32::from_white_alpha(10));
            content.rect_stroke(band, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_white_alpha(50)), StrokeKind::Inside);
        }
        painter.rect_filled(g.ruler, CornerRadius::ZERO, RULER_BACKGROUND);
        // The edge of the ruler that meets the body, and the direction away from it.
        let (ruler_edge, outward) = if g.vertical { (g.ruler.right(), -1.0) } else { (g.ruler.bottom(), -1.0) };
        let ruler_strip = |depth: f32| Rangef::new(ruler_edge + outward * depth, ruler_edge);
        if let Some((a, b)) = active {
            let (ca, cb) = (g.time.at(a), g.time.at(b));
            content.rect_filled(g.band(ca, cb, across), CornerRadius::ZERO, LOOP.gamma_multiply(0.13));
            for c in [ca, cb] {
                g.time_line(&content, c, across, Stroke::new(1.0, LOOP.gamma_multiply(0.8)));
            }
            content.rect_filled(g.band(ca, cb, ruler_strip(4.0)), CornerRadius::ZERO, LOOP);
        }
        if let Some(anchor) = self.loop_anchor {
            // A loop start waiting for its end.
            let c = g.time.at(anchor);
            g.time_line(&content, c, across, Stroke::new(1.5, LOOP));
            content.rect_filled(g.band(c - 1.5, c + 1.5, ruler_strip(10.0)), CornerRadius::ZERO, LOOP);
        }

        draw_ruler_ticks(&content, &g, duration);

        // Bookmarks: a flag in the ruler and a line across the display.
        let ruler_across = g.across_range(g.ruler);
        for flag in self.flags(&painter, &g) {
            let c = g.time.at(flag.time);
            g.time_line(&content, c, ruler_across, Stroke::new(1.0, BOOKMARK));
            g.time_line(&content, c, across, Stroke::new(1.0, BOOKMARK.gamma_multiply(0.45)));
            content.rect_filled(flag.rect, CornerRadius { nw: 0, ne: 3, sw: 0, se: 3 }, BOOKMARK);
            content.with_clip_rect(flag.rect).galley(flag.rect.min + vec2(4.0, 1.0), flag.galley, Color32::BLACK);
        }

        // Playhead: a line across everything and a marker in the ruler.
        let c = g.time.at(self.pos);
        let time_range = g.along_range(g.body);
        if (time_range.min - 1.0..=time_range.max + 1.0).contains(&c) {
            let from = if g.vertical { g.ruler.left() + 4.0 } else { g.ruler.top() + FLAG_HEIGHT + 2.0 };
            // Keep the line visible when the playhead sits on the very edge.
            let line = c.clamp(time_range.min + 0.75, time_range.max - 0.75);
            g.time_line(&content, line, Rangef::new(from, across.max), Stroke::new(1.5, ACCENT));
            let tip = g.point(c, ruler_edge - 1.0);
            let (back, side) = (g.point(0.0, -8.0).to_vec2(), g.point(5.0, 0.0).to_vec2());
            content.add(Shape::convex_polygon(vec![tip + back - side, tip + back + side, tip], ACCENT, Stroke::NONE));
        }

        // Piano, hover crosshair and readout.
        if let Some(rect) = scope {
            draw_scope(&painter.with_clip_rect(rect), rect, &track, self.pos, &self.settings);
        }
        let hover = hover.filter(|_| matches!(self.drag, Drag::None | Drag::Select { .. } | Drag::Scrub | Drag::Piano));
        let on_pitch = |p: &Pos2| g.spec.is_some_and(|r| r.contains(*p)) || over_piano(*p);
        let hover_pitch = hover.filter(on_pitch).and_then(|p| g.pitch_at(p));
        if let (Some(rect), Some(pitch)) = (g.piano, g.pitch) {
            draw_piano(&painter.with_clip_rect(rect), rect, pitch, g.vertical, hover_pitch, self.sounding);
        }
        if let Some(p) = hover.filter(|p| g.timed.contains(*p) && !over_scope(*p)) {
            // On a light spectrogram the crosshair needs a dark line.
            let in_spec = g.spec.is_some_and(|r| r.contains(p));
            let light = light && in_spec;
            let colour = if light { Color32::from_black_alpha(140) } else { Color32::from_white_alpha(90) };
            let line = Stroke::new(1.0, colour);
            // The crosshair stays out of the oscilloscope, which has no time axis.
            let line_across = match (scope, g.spec) {
                (Some(_), Some(spec)) => g.across_range(spec),
                (Some(_), None) => Rangef::new(across.min, across.min),
                (None, _) => across,
            };
            g.time_line(&content, g.along(p), line_across, line);
            let mut text = format_time(g.time_at(p), if self.view.span < 2.0 { 3 } else { 2 });
            if let (Some(midi), Some(rect), Some(pitch)) = (hover_pitch.filter(|_| in_spec), g.spec, g.pitch) {
                let along = g.along_range(rect);
                g.cross_line(&content, g.across(p), along, line);
                text = format!("{}   {:.1} Hz\n{text}", pitch::describe(midi), pitch::midi_to_freq(midi));
                // In the MIDI view, say which note this is.
                let t = g.time_at(p);
                let hidden = self.midi_hidden;
                let under = song.as_ref().and_then(|s| {
                    let hit = |n: &&Note| n.key as i32 == midi.round() as i32 && n.start <= t && t <= n.end && hidden >> n.channel & 1 == 0;
                    let note = s.notes.iter().rev().find(hit)?;
                    let channel = s.channels.iter().find(|c| c.channel == note.channel)?;
                    Some(format!("\nch {} {}   velocity {}", note.channel + 1, channel.name(), note.velocity))
                });
                text += under.as_deref().unwrap_or("");
                if self.settings.harmonics {
                    // Where the overtones of this pitch would lie.
                    let clipped = content.with_clip_rect(rect);
                    for n in 2..=8 {
                        let c = pitch.at(midi + 12.0 * (n as f64).log2());
                        g.cross_line(&clipped, c, along, Stroke::new(1.0, colour.gamma_multiply(0.55)));
                        let at = g.point(g.along(p) + if g.vertical { -10.0 } else { 12.0 }, c);
                        clipped.text(at, Align2::CENTER_CENTER, format!("{n}"), FontId::monospace(10.0), colour);
                    }
                }
            }
            let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::WHITE);
            let size = galley.size() + vec2(12.0, 8.0);
            let mut corner = p + vec2(14.0, -size.y - 10.0);
            if corner.x + size.x > full.right() {
                corner.x = p.x - size.x - 14.0;
            }
            corner.y = corner.y.max(g.body.top() + 2.0);
            let bubble = Rect::from_min_size(corner, size);
            painter.rect_filled(bubble, CornerRadius::same(4), Color32::from_black_alpha(215));
            painter.rect_stroke(bubble, CornerRadius::same(4), Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);
            painter.galley(corner + vec2(6.0, 4.0), galley, Color32::WHITE);
        }
    }

    /// A row of the MIDI file's channels; click one to hide or show its notes.
    fn midi_legend(&mut self, ui: &mut Ui, song: &MidiSong) {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Channels");
                for channel in &song.channels {
                    let bit = 1u16 << channel.channel;
                    let shown = self.midi_hidden & bit == 0;
                    let text = format!("{} {} ({})", channel.channel + 1, channel.name(), channel.notes);
                    let colour = if shown { channel_colour(channel.channel) } else { ui.visuals().weak_text_color() };
                    let chip = ui
                        .selectable_label(shown, egui::RichText::new(text).color(colour))
                        .on_hover_text("Click to hide or show this channel; right-click to show only it");
                    if chip.clicked() {
                        self.midi_hidden ^= bit;
                    }
                    if chip.secondary_clicked() {
                        // Solo, or back to everything if it already is.
                        let others = song.channels.iter().filter(|c| c.channel != channel.channel).fold(0, |m, c| m | 1 << c.channel);
                        self.midi_hidden = if self.midi_hidden == others { 0 } else { others };
                    }
                }
            });
        });
    }

    fn flags(&self, painter: &Painter, g: &Geometry) -> Vec<Flag> {
        self.session
            .bookmarks
            .iter()
            .map(|bookmark| {
                let mut label: String = bookmark.label.chars().take(24).collect();
                if label.len() < bookmark.label.len() {
                    label.push('…');
                }
                let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), Color32::BLACK);
                let c = g.time.at(bookmark.time);
                let width = galley.size().x + 8.0;
                let rect = if g.vertical {
                    // Sitting on the bookmark's line, at the outer side of the ruler.
                    let width = width.min(g.ruler.width() - 6.0);
                    Rect::from_min_size(pos2(g.ruler.left() + 1.0, c - FLAG_HEIGHT), vec2(width, FLAG_HEIGHT))
                } else {
                    Rect::from_min_size(pos2(c, g.ruler.top() + 1.0), vec2(width, FLAG_HEIGHT))
                };
                Flag { id: bookmark.id, time: bookmark.time, rect, galley }
            })
            .collect()
    }
}

fn draw_ruler_ticks(painter: &Painter, g: &Geometry, duration: f64) {
    const STEPS: [f64; 20] = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
        600.0, 1800.0,
    ];
    let axis = g.time;
    let per_point = (axis.hi - axis.lo) / axis.size() as f64;
    let spacing = if g.vertical { 44.0 } else { 80.0 };
    let step = STEPS.into_iter().find(|s| s / per_point >= spacing).unwrap_or(3600.0);
    let decimals = STEPS.iter().position(|&s| s == step).map_or(0, |i| 3usize.saturating_sub(i / 3));
    let minor = step / 5.0;
    let colour = Color32::from_white_alpha(150);
    let edge = if g.vertical { g.ruler.right() } else { g.ruler.bottom() };
    let first = (axis.lo / minor).floor() as i64;
    let last = (axis.hi.min(duration) / minor).ceil() as i64;
    for i in first.max(0)..=last {
        let c = axis.at(i as f64 * minor);
        let major = i % 5 == 0;
        let length = if major { 9.0 } else { 4.0 };
        let stroke = Stroke::new(1.0, colour.gamma_multiply(if major { 1.0 } else { 0.5 }));
        g.time_line(painter, c, Rangef::new(edge - length, edge), stroke);
        if major {
            let text = format_time(i as f64 * minor, decimals);
            let (at, align) = if g.vertical {
                (pos2(edge - 12.0, c), Align2::RIGHT_CENTER)
            } else {
                (pos2(c + 3.0, edge - 6.0), Align2::LEFT_BOTTOM)
            };
            painter.text(at, align, text, FontId::monospace(10.5), colour);
        }
    }
}

/// Where a sample sits across a waveform lane, as a fraction of its half-height
/// (-1..=1, a little more when clipping), after the zoom and scale settings.
fn wave_level(settings: &Settings, sample: f32) -> f32 {
    let v = sample * settings.wave_zoom.max(0.1);
    // On the decibel scale, 60 dB below full scale is the centre line.
    let v = if settings.wave_log { v.signum() * (1.0 + 20.0 * v.abs().max(1e-6).log10() / 60.0).max(0.0) } else { v };
    v.clamp(-1.05, 1.05)
}

/// The `index`th of `count` equal lanes of `rect`, side by side along `vertical_split`.
fn lane(rect: Rect, index: usize, count: usize, stacked: bool) -> Rect {
    let range = if stacked { rect.y_range() } else { rect.x_range() };
    let at = |i: usize| range.min + range.span() * i as f32 / count as f32;
    let part = Rangef::new(at(index), at(index + 1));
    if stacked { Rect::from_x_y_ranges(rect.x_range(), part) } else { Rect::from_x_y_ranges(part, rect.y_range()) }
}

/// A small letter naming a lane's channel.
fn lane_label(painter: &Painter, rect: Rect, channel: Channel) {
    let at = rect.left_top() + vec2(5.0, 3.0);
    painter.text(at, Align2::LEFT_TOP, channel.letter(), FontId::monospace(11.0), Color32::from_white_alpha(140));
}

#[allow(clippy::too_many_arguments)]
fn draw_waveform(
    painter: &Painter,
    rect: Rect,
    g: &Geometry,
    track: &Track,
    peaks: &Peaks,
    settings: &Settings,
    channel: Channel,
) {
    let across = g.across_range(rect);
    let along = g.along_range(rect);
    let mid = across.center();
    // Positive samples go up, or to the right when time is vertical.
    let amp = across.span() * 0.47 * if g.vertical { 1.0 } else { -1.0 };
    let level = move |sample: f32| -> f32 { mid + wave_level(settings, sample) * amp };
    if settings.wave_centre_line {
        g.cross_line(painter, mid, along, Stroke::new(1.0, Color32::from_white_alpha(30)));
    }
    let [r, green, b] = settings.wave_colour;
    let colour = Color32::from_rgb(r, green, b);
    let sr = track.sample_rate as f64;
    let axis = g.time;
    let frames_per_point = (axis.hi - axis.lo) * sr / axis.size() as f64;
    if frames_per_point < 0.5 {
        // Zoomed in to individual samples: join them up.
        let first = (axis.lo * sr).floor() as i64;
        let last = (axis.hi * sr).ceil() as i64;
        let points: Vec<Pos2> =
            (first..=last).map(|f| g.point(axis.at(f as f64 / sr), level(track.channel(f, channel)))).collect();
        if frames_per_point < 0.12 {
            for p in &points {
                painter.circle_filled(*p, 2.0, colour);
            }
        }
        painter.line(points, Stroke::new(1.5, colour));
        return;
    }
    let stroke = Stroke::new(1.0, colour);
    for i in 0..along.span().ceil() as usize {
        let c = along.min + i as f32;
        let (t0, t1) = (axis.value(c), axis.value(c + 1.0));
        let (a, b) = (t0.min(t1) * sr, t0.max(t1) * sr);
        if b < 0.0 || a >= track.frames() as f64 {
            continue;
        }
        let (lo, hi) = peaks.range(track, channel, a, b);
        let (x, y) = (level(lo), level(hi));
        let (from, to) = (x.min(y), x.max(y).max(x.min(y) + 1.0));
        g.time_line(painter, c + 0.5, Rangef::new(from, to), stroke);
    }
}

/// Oscilloscope mode: a short stretch of audio starting at the playhead,
/// held steady by starting each trace where the signal crosses zero going up.
fn draw_scope(painter: &Painter, rect: Rect, track: &Track, pos: f64, settings: &Settings) {
    painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
    let sr = track.sample_rate as f64;
    let window = ((settings.scope_ms as f64 / 1000.0 * sr) as i64).max(16);
    let playhead = (pos * sr) as i64;
    let start = (playhead..playhead + window)
        .find(|&i| track.mono(i) <= 0.0 && track.mono(i + 1) > 0.0)
        .unwrap_or(playhead);
    let [r, green, b] = settings.wave_colour;
    let colour = Color32::from_rgb(r, green, b);
    let faint = Stroke::new(1.0, Color32::from_white_alpha(30));
    if settings.scope_xy {
        // Left against right, turned so that mono is a vertical line and
        // out-of-phase sound spreads sideways.
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.46;
        let turn = std::f32::consts::FRAC_1_SQRT_2;
        for (dir, name) in [(vec2(-1.0, -1.0), "L"), (vec2(1.0, -1.0), "R")] {
            let end = dir * turn * radius;
            painter.line_segment([centre - end, centre + end], faint);
            painter.text(centre + end * 1.06, Align2::CENTER_CENTER, name, FontId::monospace(11.0), Color32::from_white_alpha(140));
        }
        let points: Vec<Pos2> = (start..start + window)
            .map(|i| {
                let [left, right] = track.stereo(i).map(|s| wave_level(settings, s));
                centre + vec2(right - left, -(left + right)) * turn * radius
            })
            .collect();
        painter.line(points, Stroke::new(1.0, colour.gamma_multiply(0.8)));
    } else {
        let lanes = settings.stereo.wave.lanes();
        for (index, &channel) in lanes.iter().enumerate() {
            let lane = lane(rect, index, lanes.len(), true);
            let (mid, amp) = (lane.center().y, lane.height() * 0.47);
            if settings.wave_centre_line {
                painter.hline(lane.x_range(), mid, faint);
            }
            let points: Vec<Pos2> = (0..=window)
                .map(|i| {
                    let x = lane.left() + lane.width() * i as f32 / window as f32;
                    pos2(x, mid - wave_level(settings, track.channel(start + i, channel)) * amp)
                })
                .collect();
            painter.with_clip_rect(lane.intersect(painter.clip_rect())).line(points, Stroke::new(1.5, colour));
            if lanes.len() > 1 {
                lane_label(painter, lane, channel);
            }
        }
    }
    let label = if settings.scope_xy { "Left against right".to_owned() } else { format!("{:.0} ms", settings.scope_ms) };
    painter.text(
        rect.right_top() + vec2(-8.0, 5.0),
        Align2::RIGHT_TOP,
        format!("Oscilloscope · {label}"),
        FontId::proportional(11.5),
        Color32::from_white_alpha(120),
    );
}

/// The piano: one equal strip per semitone, black keys short, every C
/// labelled. `along_bottom` lays it out left to right for piano mode;
/// otherwise it is a ribbon running bottom to top.
fn draw_piano(painter: &Painter, rect: Rect, axis: Axis, along_bottom: bool, hover: Option<f64>, sounding: Option<i32>) {
    const WHITE: Color32 = Color32::from_rgb(232, 232, 226);
    const BLACK: Color32 = Color32::from_rgb(24, 24, 28);
    const LINE: Color32 = Color32::from_rgb(120, 120, 120);
    painter.rect_filled(rect, CornerRadius::ZERO, WHITE);
    let key_size = axis.unit_size((axis.lo + axis.hi) / 2.0);
    let hovered = hover.map(|p| p.round() as i32);
    // A key's strip, shortened to `depth` of the way from the playing surface's far end.
    let strip = |midi: i32, depth: f32| {
        let (a, b) = (axis.at(midi as f64 - 0.5), axis.at(midi as f64 + 0.5));
        let keys = Rangef::new(a.min(b), a.max(b));
        if along_bottom {
            Rect::from_x_y_ranges(keys, rect.top()..=rect.top() + rect.height() * depth)
        } else {
            Rect::from_x_y_ranges(rect.left()..=rect.left() + rect.width() * depth, keys)
        }
    };
    let line = |from: Pos2, to: Pos2| painter.line_segment([from, to], Stroke::new(1.0, LINE));
    let label = |midi: i32, size: f32, colour: Color32| {
        let key = strip(midi, 1.0);
        let (at, align) = if along_bottom {
            (pos2(key.center().x, rect.bottom() - 3.0), Align2::CENTER_BOTTOM)
        } else {
            (pos2(rect.right() - 3.0, key.center().y), Align2::RIGHT_CENTER)
        };
        painter.text(at, align, pitch::note_name(midi), FontId::proportional(size), colour);
    };
    let (low, high) = ((axis.lo - 1.0).floor() as i32, (axis.hi + 1.0).ceil() as i32);
    for midi in low..=high {
        let key = strip(midi, 1.0);
        // Outside the 88 keys of a piano (A0..C8) the keyboard is greyed out.
        if !(21..=108).contains(&midi) {
            painter.rect_filled(key, CornerRadius::ZERO, Color32::from_rgb(150, 150, 150));
        }
        let class = midi.rem_euclid(12);
        if pitch::is_black_key(midi) {
            let short = strip(midi, 0.6);
            let corners = if along_bottom {
                CornerRadius { nw: 0, ne: 0, sw: 2, se: 2 }
            } else {
                CornerRadius { nw: 0, ne: 2, sw: 0, se: 2 }
            };
            painter.rect_filled(short, corners, BLACK);
            // The gap between the white keys either side runs on from the black key.
            if along_bottom {
                line(short.center_bottom(), key.center_bottom());
            } else {
                line(short.right_center(), key.right_center());
            }
        } else if class == 0 || class == 5 {
            // B|C and E|F: white keys meeting with no black key between.
            if along_bottom {
                line(key.left_top(), key.left_bottom());
            } else {
                line(key.left_bottom(), key.right_bottom());
            }
        }
        if sounding == Some(midi) {
            painter.rect_filled(key, CornerRadius::ZERO, ACCENT);
        } else if hovered == Some(midi) {
            painter.rect_filled(key, CornerRadius::ZERO, ACCENT.gamma_multiply(0.6));
        }
    }
    // Labels last, so neighbouring keys do not paint over them.
    let named = sounding.or(hovered).filter(|m| m.rem_euclid(12) != 0);
    // Along the bottom, a C's label gives way to the named key beside it.
    let crowded = |midi: i32| along_bottom && named.is_some_and(|n| (n - midi).abs() <= 2);
    for midi in (low..=high).filter(|m| m.rem_euclid(12) == 0 && !crowded(*m)) {
        let size = if along_bottom { 10.5 } else { (key_size * 1.5).clamp(9.0, 12.0) };
        label(midi, size, Color32::from_rgb(40, 40, 40));
    }
    if let Some(midi) = named {
        label(midi, 11.0, Color32::BLACK);
    }
    if along_bottom {
        painter.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, Color32::BLACK));
    } else {
        painter.vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, Color32::BLACK));
    }
}

/// A distinct colour for each of the 16 MIDI channels.
fn channel_colour(channel: u8) -> Color32 {
    const COLOURS: [[u8; 3]; 16] = [
        [102, 178, 255], [255, 159, 67], [120, 220, 130], [255, 107, 129], [190, 140, 255], [255, 214, 80],
        [80, 220, 220], [255, 140, 200], [170, 200, 90], [200, 200, 200], [120, 150, 255], [255, 190, 150],
        [90, 200, 160], [230, 120, 230], [210, 170, 110], [150, 230, 255],
    ];
    let [r, g, b] = COLOURS[channel as usize & 15];
    Color32::from_rgb(r, g, b)
}

/// The piano roll: one bar per note, coloured by channel and shaded by velocity.
fn draw_midi(painter: &Painter, g: &Geometry, rect: Rect, pitch: Axis, song: &MidiSong, hidden: u16) {
    let along = g.along_range(rect);
    let strip = |a: f32, b: f32, from: f32, to: f32| Rect::from_two_pos(g.point(from, a), g.point(to, b));
    // Rows under the black keys are a shade darker, as in a sequencer.
    for key in (pitch.lo.floor() as i32..=pitch.hi.ceil() as i32).filter(|k| pitch::is_black_key(*k)) {
        let row = strip(pitch.at(key as f64 - 0.5), pitch.at(key as f64 + 0.5), along.min, along.max);
        painter.rect_filled(row, CornerRadius::ZERO, Color32::from_black_alpha(70));
    }
    for note in &song.notes {
        let key_size = pitch.unit_size(note.key as f64);
        if note.start > g.time.hi {
            break; // sorted by start
        }
        if note.end < g.time.lo || hidden >> note.channel & 1 == 1 {
            continue;
        }
        let (a, b) = (g.time.at(note.start), g.time.at(note.end));
        // Keep very short notes (drum hits) visible.
        let b = if (b - a).abs() < 2.5 { a + 2.5 * g.time.length.signum() } else { b };
        let inset = if key_size > 5.0 { 0.5 } else { 0.0 };
        let (lo, hi) = (pitch.at(note.key as f64 - 0.5), pitch.at(note.key as f64 + 0.5));
        let bar = strip(lo.min(hi) + inset, lo.max(hi) - inset, a, b);
        let colour = channel_colour(note.channel).gamma_multiply(0.45 + 0.55 * note.velocity as f32 / 127.0);
        painter.rect_filled(bar, CornerRadius::same(if key_size > 6.0 { 2 } else { 0 }), colour);
        // A bright leading edge marks where each note starts.
        let edge = strip(lo.min(hi) + inset, lo.max(hi) - inset, a, a + 1.5 * g.time.length.signum());
        painter.rect_filled(edge, CornerRadius::ZERO, Color32::from_white_alpha(150));
    }
}

// --------------------------------------------------------------- tiles

/// Spectrogram columns per tile texture.
const TILE: usize = 1024;
/// Texture memory allowed for tiles, in texels.
const TILE_BUDGET: usize = 40 * TILE * 400;
const MAX_LEVEL: u32 = 12;

/// Everything that decides a tile's pixels besides the analysis itself.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    peak: u8,
    range_db: u32,
    brightness_db: i32,
    scale: LevelScale,
    /// Hundredths, so the look can be compared exactly.
    contrast: i32,
    theme: Theme,
    smooth: bool,
}

struct Tile {
    texture: TextureHandle,
    /// Finished analysis blocks and look the texture was built from.
    ready: usize,
    look: Look,
    built: f64,
    used: u64,
}

/// Cache of spectrogram textures. Level `n` tiles hold one texel per `2^n`
/// columns, so any zoom needs only a handful of tiles on screen.
#[derive(Default)]
pub struct Tiles {
    tiles: HashMap<(u32, usize), Tile>,
    frame: u64,
}

impl Tiles {
    fn draw(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        g: &Geometry,
        spec: &Spectrogram,
        settings: &Settings,
        now: f64,
    ) {
        let (Some(pitch), time) = (g.pitch, g.time) else { return };
        self.frame += 1;
        let complete = spec.is_complete();
        let look = Look {
            peak: spec.peak(),
            range_db: settings.range_db as u32,
            brightness_db: settings.brightness_db as i32,
            scale: settings.level_scale,
            contrast: (settings.contrast * 100.0).round() as i32,
            theme: settings.theme,
            smooth: settings.smooth,
        };
        let lut = colour_table(&look);
        let options = if look.smooth { TextureOptions::LINEAR } else { TextureOptions::NEAREST };

        let cols_per_sec = spec.sample_rate as f64 / spec.hop as f64;
        let (c0, c1) = (time.lo * cols_per_sec, time.hi * cols_per_sec);
        let cols_per_pixel = (c1 - c0) / (time.size() * ctx.pixels_per_point()) as f64;
        let level = (cols_per_pixel.log2().floor().max(0.0) as u32).min(MAX_LEVEL);
        let tile_cols = TILE << level;

        let mut budget = 2;
        let first = (c0.max(0.0) as usize) / tile_cols;
        let last = ((c1.max(0.0) as usize) / tile_cols).min((spec.cols - 1) / tile_cols);
        for index in first..=last {
            let (a, b) = (index * tile_cols, (index + 1) * tile_cols);
            let ready = spec.blocks_ready(a, b);
            let total = b.min(spec.cols).div_ceil(BLOCK) - a / BLOCK;
            let tile = self.tiles.get(&(level, index));
            let stale = match tile {
                None => true,
                Some(t) => {
                    // While analysis runs, refresh partial tiles a few times a second.
                    (t.ready != ready || t.look != look) && (complete || now - t.built > 0.35)
                }
            };
            if stale && budget > 0 && (ready > 0 || tile.is_none()) {
                budget -= 1;
                let image = build_tile(spec, level, index, &lut);
                let tile = Tile {
                    texture: match self.tiles.remove(&(level, index)) {
                        Some(mut old) => {
                            old.texture.set(image, options);
                            old.texture
                        }
                        None => ctx.load_texture(format!("spectrogram-{level}-{index}"), image, options),
                    },
                    ready,
                    look,
                    built: now,
                    used: self.frame,
                };
                self.tiles.insert((level, index), tile);
            }
            if stale && (ready < total || budget == 0) {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            if let Some(tile) = self.tiles.get_mut(&(level, index)) {
                tile.used = self.frame;
                // Column c is centred on frame c * hop. The texture has time
                // along u and pitch along v, whichever way the display runs.
                let along = |col: usize| time.at((col as f64 - 0.5) / cols_per_sec);
                let mut mesh = egui::Mesh::with_texture(tile.texture.id());
                // The texture's rows are even in pitch, so on a mel or hertz
                // axis it is drawn as a stack of strips, each stretched to fit.
                // (A linear axis over the linear grid needs no stretching either.)
                let even = if spec.linear { FreqScale::Linear } else { FreqScale::Pitch };
                let strips: u32 = if pitch.warp == even { 1 } else { 96 };
                for i in 0..=strips {
                    let p = pitch.lo + (pitch.hi - pitch.lo) * i as f64 / strips as f64;
                    for (col, u) in [(a, 0.0), (b, 1.0)] {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos: g.point(along(col), pitch.at(p)),
                            uv: pos2(u, spec.v(p)),
                            color: Color32::WHITE,
                        });
                    }
                    if i > 0 {
                        let k = i * 2;
                        mesh.indices.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
                    }
                }
                painter.add(Shape::mesh(mesh));
            }
        }
        let max_tiles = (TILE_BUDGET / (TILE * spec.rows)).max(12);
        if self.tiles.len() > max_tiles {
            let mut ages: Vec<u64> = self.tiles.values().map(|t| t.used).collect();
            ages.sort_unstable();
            let cutoff = ages[ages.len() - max_tiles];
            self.tiles.retain(|_, t| t.used >= cutoff);
        }
    }
}

fn build_tile(spec: &Spectrogram, level: u32, index: usize, lut: &[Color32; 256]) -> ColorImage {
    let step = 1usize << level;
    let first = index * TILE * step;
    let rows = spec.rows;
    let mut pixels = vec![lut[0]; TILE * rows];
    spec.with_data(|data| {
        let mut column = vec![0u8; rows];
        for x in 0..TILE {
            let a = first + x * step;
            if a >= spec.cols {
                break;
            }
            let b = (a + step).min(spec.cols);
            column.copy_from_slice(&data[a * rows..][..rows]);
            for c in a + 1..b {
                for (max, value) in column.iter_mut().zip(&data[c * rows..][..rows]) {
                    *max = (*max).max(*value);
                }
            }
            for (row, &value) in column.iter().enumerate() {
                pixels[(rows - 1 - row) * TILE + x] = lut[value as usize];
            }
        }
    });
    ColorImage::new([TILE, rows], pixels)
}

/// Map stored levels to colours: the loudest level seen (lowered by the
/// brightness setting) is the top of the scale, and `range_db` below that is
/// the theme's colour for silence.
fn colour_table(look: &Look) -> [Color32; 256] {
    let per_db = 255.0 / DB_RANGE;
    let top = look.peak.max(1) as f32 - look.brightness_db as f32 * per_db;
    let range = look.range_db as f32;
    std::array::from_fn(|value| {
        let below_top = (top - value as f32) / per_db;
        let [r, g, b] = look.theme.colour(look.scale.position(below_top, range, look.contrast as f32 / 100.0));
        Color32::from_rgb(r, g, b)
    })
}
