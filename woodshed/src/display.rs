//! The zoomable display: time ruler with bookmarks and loop region, waveform,
//! and the pitch-axis spectrogram with its piano ribbon and hover readout.

use std::collections::HashMap;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Painter, PointerButton, Pos2,
    Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui,
};

use crate::analysis::{Peaks, Spectrogram, BLOCK, DB_RANGE, PITCH_MAX, PITCH_MIN, ROWS};
use crate::app::{format_time, App, View, ACCENT, BOOKMARK, LOOP};
use crate::pitch;
use crate::session::{LoopMode, Theme, ViewMode};
use crate::track::Track;

const RULER_HEIGHT: f32 = 38.0;
const FLAG_HEIGHT: f32 = 15.0;
const RIBBON_WIDTH: f32 = 54.0;
const BACKGROUND: Color32 = Color32::from_rgb(14, 14, 18);
const RULER_BACKGROUND: Color32 = Color32::from_rgb(28, 28, 34);
const WAVEFORM: Color32 = Color32::from_rgb(120, 172, 232);
/// Pointer distance within which a selection edge can be grabbed.
const GRAB: f32 = 5.0;

/// What a pointer drag in the display is doing.
pub enum Drag {
    None,
    /// Selecting a loop region from `anchor` seconds.
    Select { anchor: f64 },
    /// Moving one edge of the selection.
    Edge { start: bool },
    /// Moving a bookmark.
    Bookmark { id: u64 },
    /// Panning time (and pitch).
    Pan,
    /// Panning the pitch axis from the piano ribbon.
    Pitch,
}

/// Maps time to x within the content area.
#[derive(Clone, Copy)]
struct TimeAxis {
    left: f32,
    width: f32,
    start: f64,
    span: f64,
}

impl TimeAxis {
    fn new(view: &View, left: f32, width: f32) -> Self {
        TimeAxis { left, width, start: view.start, span: view.span }
    }

    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.start) / self.span) as f32 * self.width
    }

    fn t(&self, x: f32) -> f64 {
        self.start + ((x - self.left) / self.width) as f64 * self.span
    }
}

/// Maps MIDI pitch to y within the spectrogram.
#[derive(Clone, Copy)]
struct PitchAxis {
    top: f32,
    height: f32,
    lo: f64,
    hi: f64,
}

impl PitchAxis {
    fn new(view: &View, rect: Rect) -> Self {
        PitchAxis { top: rect.top(), height: rect.height(), lo: view.pitch_lo, hi: view.pitch_hi }
    }

    fn y(&self, pitch: f64) -> f32 {
        self.top + ((self.hi - pitch) / (self.hi - self.lo)) as f32 * self.height
    }

    fn pitch(&self, y: f32) -> f64 {
        self.hi - ((y - self.top) / self.height) as f64 * (self.hi - self.lo)
    }
}

struct Flag {
    id: u64,
    time: f64,
    rect: Rect,
    galley: std::sync::Arc<egui::Galley>,
}

impl App {
    pub fn display(&mut self, ui: &mut Ui) {
        let Some(loaded) = &self.loaded else { return };
        let (track, peaks, spectrogram) = (loaded.track.clone(), loaded.peaks.clone(), loaded.spectrogram.clone());
        let duration = track.duration();
        let now = ui.input(|i| i.time);

        // ------------------------------------------------------- layout
        let full = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(full, Sense::click_and_drag());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, CornerRadius::ZERO, BACKGROUND);

        let mode = self.settings.view;
        let left = full.left() + if mode == ViewMode::Waveform { 0.0 } else { RIBBON_WIDTH };
        let width = (full.right() - left).max(10.0);
        let ruler = Rect::from_min_max(pos2(left, full.top()), pos2(full.right(), full.top() + RULER_HEIGHT));
        let body = Rect::from_min_max(pos2(left, ruler.bottom()), full.max);
        let (wave_rect, spec_rect) = match mode {
            ViewMode::Waveform => (Some(body), None),
            ViewMode::Spectrogram => (None, Some(body)),
            ViewMode::Both => {
                let split = body.top() + (body.height() * 0.28).clamp(50.0, 240.0);
                (
                    Some(Rect::from_min_max(body.min, pos2(body.right(), split))),
                    Some(Rect::from_min_max(pos2(body.left(), split + 1.0), body.max)),
                )
            }
        };
        let ribbon = spec_rect.map(|r| Rect::from_min_max(pos2(full.left(), r.top()), pos2(left, r.bottom())));
        let timed = Rect::from_min_max(ruler.min, full.max); // everything with a time axis

        // -------------------------------------------------------- input
        let axis = TimeAxis::new(&self.view, left, width);
        let flags = self.flags(&painter, axis, ruler);
        let flag_at = |p: Pos2| flags.iter().rev().find(|f| f.rect.contains(p)).map(|f| (f.id, f.time));
        let edge_at = |p: Pos2, selection: Option<(f64, f64)>| {
            let (a, b) = selection?;
            if !timed.contains(p) {
                return None;
            }
            let (da, db) = ((axis.x(a) - p.x).abs(), (axis.x(b) - p.x).abs());
            (da.min(db) <= GRAB).then_some(da < db)
        };
        let hover = response.hover_pos();

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin()).or(hover).unwrap_or(full.center());
            let primary = ui.input(|i| i.pointer.button_down(PointerButton::Primary));
            self.drag = if !primary {
                Drag::Pan
            } else if ribbon.is_some_and(|r| r.contains(origin)) {
                Drag::Pitch
            } else if let Some((id, _)) = flag_at(origin) {
                Drag::Bookmark { id }
            } else if let Some(start) = edge_at(origin, self.session.selection) {
                Drag::Edge { start }
            } else if timed.contains(origin) {
                Drag::Select { anchor: axis.t(origin.x).clamp(0.0, duration) }
            } else {
                Drag::None
            };
        }
        if response.dragged() {
            let delta = response.drag_delta();
            let t = response.interact_pointer_pos().map(|p| axis.t(p.x).clamp(0.0, duration));
            let pan_pitch = |view: &mut View, rect: Option<Rect>| {
                if let Some(rect) = rect {
                    let shift = (delta.y / rect.height()) as f64 * (view.pitch_hi - view.pitch_lo);
                    view.pitch_lo += shift;
                    view.pitch_hi += shift;
                }
            };
            match (&mut self.drag, t) {
                (Drag::Select { anchor }, Some(t)) => {
                    self.session.selection = Some((anchor.min(t), anchor.max(t)));
                    self.settings.loop_mode = LoopMode::Selection;
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
                    self.view.start -= (delta.x / width) as f64 * self.view.span;
                    pan_pitch(&mut self.view, spec_rect);
                    self.last_pan = now;
                }
                (Drag::Pitch, _) => pan_pitch(&mut self.view, spec_rect),
                _ => {}
            }
        }
        if response.drag_stopped() {
            if let (Drag::Select { .. }, Some((a, b))) = (&self.drag, self.session.selection) {
                if axis.x(b) - axis.x(a) < 3.0 {
                    self.session.selection = None;
                }
            }
            self.drag = Drag::None;
        }
        if let Some(p) = response.interact_pointer_pos().filter(|_| response.clicked()) {
            if let Some((_, time)) = flag_at(p) {
                self.seek(time);
            } else if timed.contains(p) {
                self.seek(axis.t(p.x));
            }
        }
        if let Some((id, _)) = hover.filter(|_| response.double_clicked()).and_then(flag_at) {
            self.focus_label = Some(id);
        }

        if let Some(p) = hover {
            let (scroll, pinch, alt) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta() as f64, i.modifiers.alt));
            let over_ribbon = ribbon.is_some_and(|r| r.contains(p));
            let pitch_axis = spec_rect.map(|r| PitchAxis::new(&self.view, r));
            let zoom_pitch = |view: &mut View, factor: f64| {
                if let Some(pa) = pitch_axis {
                    let anchor = pa.pitch(p.y);
                    view.pitch_lo = anchor - (anchor - view.pitch_lo) * factor;
                    view.pitch_hi = anchor + (view.pitch_hi - anchor) * factor;
                }
            };
            let wheel = (-scroll.y as f64 * 0.004).exp();
            if over_ribbon {
                if let Some(pa) = pitch_axis {
                    let shift = (scroll.y / pa.height) as f64 * (pa.hi - pa.lo);
                    self.view.pitch_lo += shift;
                    self.view.pitch_hi += shift;
                }
                zoom_pitch(&mut self.view, 1.0 / pinch);
            } else if alt && spec_rect.is_some_and(|r| r.contains(p)) {
                zoom_pitch(&mut self.view, wheel);
            } else {
                if scroll.y != 0.0 || pinch != 1.0 {
                    self.zoom_time(wheel / pinch, axis.t(p.x.max(left)));
                }
                if scroll.x != 0.0 {
                    self.view.start -= (scroll.x / width) as f64 * self.view.span;
                    self.last_pan = now;
                }
            }
            // Cursor hints.
            if !over_ribbon {
                if flag_at(p).is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                } else if edge_at(p, self.session.selection).is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            }
        }
        match self.drag {
            Drag::Edge { .. } => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            Drag::Bookmark { .. } | Drag::Pan | Drag::Pitch => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ => {}
        }
        self.clamp_view();

        // ------------------------------------------------------ drawing
        let axis = TimeAxis::new(&self.view, left, width);
        let content = painter.with_clip_rect(timed);
        if let Some(rect) = wave_rect {
            draw_waveform(&content.with_clip_rect(rect), rect, axis, &track, &peaks);
        }
        if let Some(rect) = spec_rect {
            let pitch_axis = PitchAxis::new(&self.view, rect);
            let clipped = content.with_clip_rect(rect);
            let (range, theme) = (self.settings.range_db, self.settings.theme);
            self.tiles.draw(ui.ctx(), &clipped, rect, axis, pitch_axis, &spectrogram, range, theme, now);
            // Octave lines at every C.
            let grid = if theme.is_light() { Color32::from_black_alpha(40) } else { Color32::from_white_alpha(22) };
            for midi in (12..=120).step_by(12).filter(|_| self.settings.octave_lines) {
                let y = pitch_axis.y(midi as f64 - 0.5);
                clipped.hline(rect.x_range(), y, Stroke::new(1.0, grid));
            }
            if !spectrogram.is_complete() {
                clipped.text(
                    rect.right_bottom() - vec2(8.0, 6.0),
                    Align2::RIGHT_BOTTOM,
                    format!("Analysing… {:.0}%", spectrogram.progress() * 100.0),
                    FontId::proportional(12.0),
                    if theme.is_light() { Color32::from_black_alpha(170) } else { Color32::from_white_alpha(170) },
                );
            }
            if let (Some(_), Some(ribbon)) = (wave_rect, ribbon) {
                painter.hline(full.x_range(), ribbon.top() - 0.5, Stroke::new(1.0, RULER_BACKGROUND));
            }
        }

        // Selection and active loop region.
        let active = match self.settings.loop_mode {
            LoopMode::Off => None,
            LoopMode::Selection => self.session.selection,
            LoopMode::Section => Some(self.session.section(self.pos, duration)),
            LoopMode::Track => Some((0.0, duration)),
        };
        if let Some((a, b)) = self.session.selection.filter(|s| Some(*s) != active) {
            let band = Rect::from_x_y_ranges(axis.x(a)..=axis.x(b), body.y_range());
            content.rect_filled(band, CornerRadius::ZERO, Color32::from_white_alpha(10));
            content.rect_stroke(band, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_white_alpha(50)), StrokeKind::Inside);
        }
        painter.rect_filled(ruler, CornerRadius::ZERO, RULER_BACKGROUND);
        if let Some((a, b)) = active {
            let (xa, xb) = (axis.x(a), axis.x(b));
            content.rect_filled(Rect::from_x_y_ranges(xa..=xb, body.y_range()), CornerRadius::ZERO, LOOP.gamma_multiply(0.13));
            for x in [xa, xb] {
                content.vline(x, body.y_range(), Stroke::new(1.0, LOOP.gamma_multiply(0.8)));
            }
            let bar = Rect::from_x_y_ranges(xa..=xb, ruler.bottom() - 4.0..=ruler.bottom());
            content.rect_filled(bar, CornerRadius::ZERO, LOOP);
        }

        draw_ruler_ticks(&content, ruler, axis, duration);

        // Bookmarks: a flag in the ruler and a line down the display.
        for flag in self.flags(&painter, axis, ruler) {
            let x = flag.rect.left();
            content.vline(x, flag.rect.bottom()..=ruler.bottom(), Stroke::new(1.0, BOOKMARK));
            content.vline(x, body.y_range(), Stroke::new(1.0, BOOKMARK.gamma_multiply(0.45)));
            content.rect_filled(flag.rect, CornerRadius { nw: 0, ne: 3, sw: 0, se: 3 }, BOOKMARK);
            content.galley(flag.rect.min + vec2(4.0, 1.0), flag.galley, Color32::BLACK);
        }

        // Playhead.
        let x = axis.x(self.pos);
        if (left - 1.0..=full.right() + 1.0).contains(&x) {
            content.vline(x, ruler.top() + FLAG_HEIGHT + 2.0..=full.bottom(), Stroke::new(1.5, ACCENT));
            let tip = pos2(x, ruler.bottom() - 1.0);
            content.add(Shape::convex_polygon(
                vec![tip + vec2(-5.0, -8.0), tip + vec2(5.0, -8.0), tip],
                ACCENT,
                Stroke::NONE,
            ));
        }

        let hover = hover.filter(|_| matches!(self.drag, Drag::None | Drag::Select { .. }));
        let hover_pitch = hover.zip(spec_rect).filter(|(p, r)| r.contains(*p) || ribbon.is_some_and(|rb| rb.contains(*p)));
        let hover_pitch = hover_pitch.map(|(p, r)| PitchAxis::new(&self.view, r).pitch(p.y));
        if let (Some(rect), Some(ribbon)) = (spec_rect, ribbon) {
            draw_piano(&painter.with_clip_rect(ribbon), ribbon, PitchAxis::new(&self.view, rect), hover_pitch);
        }
        if let Some(p) = hover.filter(|p| timed.contains(*p)) {
            // On a light spectrogram the crosshair needs a dark line.
            let light = self.settings.theme.is_light() && spec_rect.is_some_and(|r| r.contains(p));
            let colour = if light { Color32::from_black_alpha(140) } else { Color32::from_white_alpha(90) };
            let line = Stroke::new(1.0, colour);
            content.vline(p.x, ruler.bottom()..=full.bottom(), line);
            let mut text = format_time(axis.t(p.x), if axis.span < 2.0 { 3 } else { 2 });
            if let Some(midi) = hover_pitch.filter(|_| p.y > ruler.bottom()) {
                content.hline(left..=full.right(), p.y, line);
                text = format!("{}   {:.1} Hz\n{text}", pitch::describe(midi), pitch::midi_to_freq(midi));
            }
            let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::WHITE);
            let size = galley.size() + vec2(12.0, 8.0);
            let mut corner = p + vec2(14.0, -size.y - 10.0);
            if corner.x + size.x > full.right() {
                corner.x = p.x - size.x - 14.0;
            }
            corner.y = corner.y.max(ruler.bottom() + 2.0);
            let bubble = Rect::from_min_size(corner, size);
            painter.rect_filled(bubble, CornerRadius::same(4), Color32::from_black_alpha(215));
            painter.rect_stroke(bubble, CornerRadius::same(4), Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);
            painter.galley(corner + vec2(6.0, 4.0), galley, Color32::WHITE);
        }
    }

    fn flags(&self, painter: &Painter, axis: TimeAxis, ruler: Rect) -> Vec<Flag> {
        self.session
            .bookmarks
            .iter()
            .map(|bookmark| {
                let mut label: String = bookmark.label.chars().take(24).collect();
                if label.len() < bookmark.label.len() {
                    label.push('…');
                }
                let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), Color32::BLACK);
                let min = pos2(axis.x(bookmark.time), ruler.top() + 1.0);
                let rect = Rect::from_min_size(min, vec2(galley.size().x + 8.0, FLAG_HEIGHT));
                Flag { id: bookmark.id, time: bookmark.time, rect, galley }
            })
            .collect()
    }
}

fn draw_ruler_ticks(painter: &Painter, ruler: Rect, axis: TimeAxis, duration: f64) {
    const STEPS: [f64; 20] = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
        600.0, 1800.0,
    ];
    let per_point = axis.span / axis.width as f64;
    let step = STEPS.into_iter().find(|s| s / per_point >= 80.0).unwrap_or(3600.0);
    let decimals = STEPS.iter().position(|&s| s == step).map_or(0, |i| 3usize.saturating_sub(i / 3));
    let minor = step / 5.0;
    let colour = Color32::from_white_alpha(150);
    let first = (axis.start / minor).floor() as i64;
    let last = ((axis.start + axis.span).min(duration) / minor).ceil() as i64;
    for i in first.max(0)..=last {
        let x = axis.x(i as f64 * minor);
        let major = i % 5 == 0;
        let top = ruler.bottom() - if major { 9.0 } else { 4.0 };
        painter.vline(x, top..=ruler.bottom(), Stroke::new(1.0, colour.gamma_multiply(if major { 1.0 } else { 0.5 })));
        if major {
            painter.text(
                pos2(x + 3.0, ruler.bottom() - 6.0),
                Align2::LEFT_BOTTOM,
                format_time(i as f64 * minor, decimals),
                FontId::monospace(10.5),
                colour,
            );
        }
    }
}

fn draw_waveform(painter: &Painter, rect: Rect, axis: TimeAxis, track: &Track, peaks: &Peaks) {
    let mid = rect.center().y;
    let amp = rect.height() * 0.47;
    painter.hline(rect.x_range(), mid, Stroke::new(1.0, Color32::from_white_alpha(30)));
    let sr = track.sample_rate as f64;
    let frames_per_point = axis.span * sr / axis.width as f64;
    if frames_per_point < 0.5 {
        // Zoomed in to individual samples: join them up.
        let first = (axis.start * sr).floor() as i64;
        let last = ((axis.start + axis.span) * sr).ceil() as i64;
        let points: Vec<Pos2> =
            (first..=last).map(|f| pos2(axis.x(f as f64 / sr), mid - track.mono(f) * amp)).collect();
        if frames_per_point < 0.12 {
            for p in &points {
                painter.circle_filled(*p, 2.0, WAVEFORM);
            }
        }
        painter.line(points, Stroke::new(1.5, WAVEFORM));
        return;
    }
    let stroke = Stroke::new(1.0, WAVEFORM);
    let columns = rect.width().ceil() as usize;
    for i in 0..columns {
        let x = rect.left() + i as f32;
        let (a, b) = (axis.t(x) * sr, axis.t(x + 1.0) * sr);
        if b < 0.0 || a >= track.frames() as f64 {
            continue;
        }
        let (lo, hi) = peaks.range(track, a, b);
        let (top, bottom) = (mid - hi * amp, mid - lo * amp);
        painter.vline(x + 0.5, top.min(bottom - 1.0)..=bottom, stroke);
    }
}

/// The piano ribbon: one row per semitone, black keys short, every C labelled.
fn draw_piano(painter: &Painter, rect: Rect, axis: PitchAxis, hover: Option<f64>) {
    const WHITE: Color32 = Color32::from_rgb(232, 232, 226);
    const BLACK: Color32 = Color32::from_rgb(24, 24, 28);
    const LINE: Color32 = Color32::from_rgb(120, 120, 120);
    painter.rect_filled(rect, CornerRadius::ZERO, WHITE);
    let key_height = axis.height / (axis.hi - axis.lo) as f32;
    let hovered = hover.map(|p| p.round() as i32);
    for midi in (axis.lo - 1.0).floor() as i32..=(axis.hi + 1.0).ceil() as i32 {
        let (top, bottom) = (axis.y(midi as f64 + 0.5), axis.y(midi as f64 - 0.5));
        let row = Rect::from_x_y_ranges(rect.x_range(), top..=bottom);
        // Outside the 88 keys of a piano (A0..C8) the ribbon is greyed out.
        if !(21..=108).contains(&midi) {
            painter.rect_filled(row, CornerRadius::ZERO, Color32::from_rgb(150, 150, 150));
        }
        let class = midi.rem_euclid(12);
        if pitch::is_black_key(midi) {
            let key = Rect::from_min_max(row.min, pos2(rect.left() + rect.width() * 0.6, bottom));
            painter.rect_filled(key, CornerRadius { nw: 0, ne: 2, sw: 0, se: 2 }, BLACK);
            // The gap between the white keys either side runs on from the black key.
            painter.hline(key.right()..=rect.right(), row.center().y, Stroke::new(1.0, LINE));
        } else if class == 0 || class == 5 {
            // B|C and E|F: white keys meeting with no black key between.
            painter.hline(rect.x_range(), bottom, Stroke::new(1.0, LINE));
        }
        if hovered == Some(midi) {
            painter.rect_filled(row, CornerRadius::ZERO, ACCENT.gamma_multiply(0.75));
        }
        if class == 0 {
            let size = (key_height * 1.5).clamp(9.0, 12.0);
            painter.text(
                pos2(rect.right() - 3.0, row.center().y),
                Align2::RIGHT_CENTER,
                pitch::note_name(midi),
                FontId::proportional(size),
                Color32::from_rgb(40, 40, 40),
            );
        }
    }
    if let Some(midi) = hovered.filter(|m| m.rem_euclid(12) != 0) {
        // Name the hovered key too.
        let y = axis.y(midi as f64);
        painter.text(
            pos2(rect.right() - 3.0, y),
            Align2::RIGHT_CENTER,
            pitch::note_name(midi),
            FontId::proportional(11.0),
            Color32::BLACK,
        );
    }
    painter.vline(rect.right() - 0.5, rect.y_range(), Stroke::new(1.0, Color32::BLACK));
}

// --------------------------------------------------------------- tiles

/// Spectrogram columns per tile texture.
const TILE: usize = 1024;
const MAX_TILES: usize = 40;
const MAX_LEVEL: u32 = 12;

struct Tile {
    texture: TextureHandle,
    /// Finished analysis blocks and colour mapping the texture was built from.
    ready: usize,
    colours: (u8, u32, Theme),
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
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        ctx: &egui::Context,
        painter: &Painter,
        rect: Rect,
        time: TimeAxis,
        pitch: PitchAxis,
        spec: &Spectrogram,
        range_db: f32,
        theme: Theme,
        now: f64,
    ) {
        self.frame += 1;
        let complete = spec.is_complete();
        let colours = (spec.peak(), range_db as u32, theme);
        let lut = colour_table(spec.peak(), range_db, theme);

        let cols_per_sec = spec.sample_rate as f64 / spec.hop as f64;
        let (c0, c1) = (time.start * cols_per_sec, (time.start + time.span) * cols_per_sec);
        let cols_per_pixel = (c1 - c0) / (rect.width() * ctx.pixels_per_point()) as f64;
        let level = (cols_per_pixel.log2().floor().max(0.0) as u32).min(MAX_LEVEL);
        let tile_cols = TILE << level;

        let v = |p: f64| ((PITCH_MAX - p) / (PITCH_MAX - PITCH_MIN)) as f32;
        let uv_y = v(pitch.hi)..=v(pitch.lo);
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
                    (t.ready != ready || t.colours != colours) && (complete || now - t.built > 0.35)
                }
            };
            if stale && budget > 0 && (ready > 0 || tile.is_none()) {
                budget -= 1;
                let image = build_tile(spec, level, index, &lut);
                let tile = Tile {
                    texture: match self.tiles.remove(&(level, index)) {
                        Some(mut old) => {
                            old.texture.set(image, TextureOptions::LINEAR);
                            old.texture
                        }
                        None => ctx.load_texture(format!("spectrogram-{level}-{index}"), image, TextureOptions::LINEAR),
                    },
                    ready,
                    colours,
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
                // Column c is centred on frame c * hop.
                let x = |col: usize| time.x((col as f64 - 0.5) / cols_per_sec);
                painter.image(
                    tile.texture.id(),
                    Rect::from_x_y_ranges(x(a)..=x(b), rect.y_range()),
                    Rect::from_x_y_ranges(0.0..=1.0, uv_y.clone()),
                    Color32::WHITE,
                );
            }
        }
        if self.tiles.len() > MAX_TILES {
            let mut ages: Vec<u64> = self.tiles.values().map(|t| t.used).collect();
            ages.sort_unstable();
            let cutoff = ages[ages.len() - MAX_TILES];
            self.tiles.retain(|_, t| t.used >= cutoff);
        }
    }
}

fn build_tile(spec: &Spectrogram, level: u32, index: usize, lut: &[Color32; 256]) -> ColorImage {
    let step = 1usize << level;
    let first = index * TILE * step;
    let mut pixels = vec![lut[0]; TILE * ROWS];
    spec.with_data(|data| {
        let mut column = [0u8; ROWS];
        for x in 0..TILE {
            let a = first + x * step;
            if a >= spec.cols {
                break;
            }
            let b = (a + step).min(spec.cols);
            column.copy_from_slice(&data[a * ROWS..][..ROWS]);
            for c in a + 1..b {
                for (max, value) in column.iter_mut().zip(&data[c * ROWS..][..ROWS]) {
                    *max = (*max).max(*value);
                }
            }
            for (row, &value) in column.iter().enumerate() {
                pixels[(ROWS - 1 - row) * TILE + x] = lut[value as usize];
            }
        }
    });
    ColorImage::new([TILE, ROWS], pixels)
}

/// Map stored levels to colours: the loudest level seen is the top of the
/// scale and `range_db` below it is the theme's colour for silence.
fn colour_table(peak: u8, range_db: f32, theme: Theme) -> [Color32; 256] {
    let top = peak.max(1) as f32;
    let span = range_db / DB_RANGE * 255.0;
    std::array::from_fn(|value| {
        let [r, g, b] = theme.colour((value as f32 - (top - span)) / span);
        Color32::from_rgb(r, g, b)
    })
}
