//! Application state, hotkeys, and the chrome around the zoomable display:
//! top bar, bookmark sidebar and the transport bar with the full-song seek bar.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Modifiers, Rect, RichText, Sense,
    Shape, Stroke, Ui,
};

use crate::analysis::{Peaks, Spectrogram, PITCH_MAX, PITCH_MIN};
use crate::display::{Drag, Tiles};
use crate::engine::{Engine, Output, MAX_SPEED, MIN_SPEED};
use crate::session::{Follow, Library, LoopMode, Session, Settings, SidebarTab, ViewMode};
use crate::track::{self, Track};

pub const ACCENT: Color32 = Color32::from_rgb(255, 184, 76);
pub const BOOKMARK: Color32 = Color32::from_rgb(110, 220, 170);
pub const LOOP: Color32 = Color32::from_rgb(96, 160, 255);
/// Shortest stretch of time the display can be zoomed in to, seconds.
const MIN_SPAN: f64 = 0.004;

/// "m:ss.cc" (or "m:ss" with `decimals` = 0).
pub fn format_time(secs: f64, decimals: usize) -> String {
    let scale = 10f64.powi(decimals as i32);
    let t = (secs.max(0.0) * scale).floor() / scale;
    let minutes = (t / 60.0).floor();
    let width = if decimals == 0 { 2 } else { decimals + 3 };
    format!("{}:{:0width$.decimals$}", minutes as u64, t - minutes * 60.0)
}

pub struct Loaded {
    pub track: Arc<Track>,
    pub peaks: Arc<Peaks>,
    pub spectrogram: Arc<Spectrogram>,
}

struct Loading {
    name: String,
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<(Track, Peaks), String>>,
}

/// The visible part of the timeline and pitch axis.
pub struct View {
    pub start: f64,
    pub span: f64,
    pub pitch_lo: f64,
    pub pitch_hi: f64,
}

pub struct App {
    engine: Arc<Mutex<Engine>>,
    output: Output,
    pub settings: Settings,
    pub session: Session,
    session_dirty: bool,
    /// The settings as last written to disk.
    saved_settings: String,
    pub loaded: Option<Loaded>,
    loading: Option<Loading>,
    error: Option<String>,
    dialog: Option<Receiver<Option<PathBuf>>>,
    /// Pending "add files to the playlist" dialog.
    add_dialog: Option<Receiver<Option<Vec<PathBuf>>>>,

    library: Library,
    library_dirty: bool,
    /// The file that is loaded, or being loaded.
    current_path: Option<PathBuf>,
    /// Start playing as soon as the file being loaded is ready.
    autoplay: bool,
    confirm_delete: bool,

    /// Heard playback position this frame, seconds.
    pub pos: f64,
    pub playing: bool,
    /// Where playback last started, for "return on pause".
    cue: f64,

    pub view: View,
    pub tiles: Tiles,
    pub drag: Drag,
    /// When the user last panned the display by hand.
    pub last_pan: f64,
    fullscreen: bool,
    show_help: bool,
    /// Bookmark whose label should grab keyboard focus.
    pub focus_label: Option<u64>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> App {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let settings = Settings::load();
        let saved_settings = serde_json::to_string(&settings).unwrap_or_default();
        let engine = Arc::new(Mutex::new(Engine::new()));
        engine.lock().unwrap().set_volume(settings.volume);
        engine.lock().unwrap().set_keep_pitch(settings.keep_pitch);
        let output = Output::open(&engine, None);
        let mut app = App {
            engine,
            output,
            settings,
            session: Session::default(),
            session_dirty: false,
            saved_settings,
            loaded: None,
            loading: None,
            error: None,
            dialog: None,
            add_dialog: None,
            library: Library::load(),
            library_dirty: false,
            current_path: None,
            autoplay: false,
            confirm_delete: false,
            pos: 0.0,
            playing: false,
            cue: 0.0,
            view: View { start: 0.0, span: 30.0, pitch_lo: 33.0, pitch_hi: 96.0 },
            tiles: Tiles::default(),
            drag: Drag::None,
            last_pan: f64::NEG_INFINITY,
            fullscreen: false,
            show_help: false,
            focus_label: None,
        };
        app.open_files(files, &cc.egui_ctx);
        app
    }

    pub fn duration(&self) -> f64 {
        self.loaded.as_ref().map_or(0.0, |l| l.track.duration())
    }

    // ------------------------------------------------------------ loading

    /// One file is simply opened; several are added to the playlist (and
    /// the first opened if nothing is loaded yet).
    fn open_files(&mut self, files: Vec<PathBuf>, ctx: &egui::Context) {
        let files: Vec<PathBuf> = files.iter().map(|p| absolute(p)).collect();
        match files.as_slice() {
            [] => {}
            [one] => self.open(one, ctx),
            [first, ..] => {
                if self.current_path.is_none() {
                    self.open(first, ctx);
                }
                self.add_to_playlist(files);
            }
        }
    }

    fn add_to_playlist(&mut self, files: Vec<PathBuf>) {
        self.library.playlist_mut().add(files);
        self.library_dirty = true;
        self.settings.sidebar = SidebarTab::Playlist;
    }

    /// Open the playlist track `step` places from the current one.
    fn playlist_step(&mut self, step: isize, ctx: &egui::Context) -> bool {
        let Some(path) = self.library.playlist().neighbour(self.current_path.as_deref(), step).cloned() else {
            return false;
        };
        self.autoplay = self.playing;
        self.open(&path, ctx);
        true
    }

    fn open(&mut self, path: &Path, ctx: &egui::Context) {
        let path = &absolute(path);
        self.current_path = Some(path.clone());
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = channel();
        let (path_buf, thread_progress, ctx) = (path.to_path_buf(), progress.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = track::decode(&path_buf, &thread_progress).map(|track| {
                let peaks = Peaks::build(&track);
                (track, peaks)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.loading = Some(Loading { name, progress, rx });
        self.error = None;
        self.settings.last_dir = path.parent().map(Path::to_path_buf);
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (dir, ctx) = (self.settings.last_dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().add_filter("Audio", track::AUDIO_EXTENSIONS);
            if let Some(dir) = dir {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_file());
            ctx.request_repaint();
        });
        self.dialog = Some(rx);
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        if self.add_dialog.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (dir, ctx) = (self.settings.last_dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().add_filter("Audio", track::AUDIO_EXTENSIONS);
            if let Some(dir) = dir {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_files());
            ctx.request_repaint();
        });
        self.add_dialog = Some(rx);
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(choice) = self.add_dialog.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.add_dialog = None;
            if let Some(files) = choice {
                self.settings.last_dir = files.first().and_then(|f| f.parent()).map(Path::to_path_buf);
                self.add_to_playlist(files);
            }
        }
        if let Some(rx) = &self.dialog {
            if let Ok(choice) = rx.try_recv() {
                self.dialog = None;
                if let Some(path) = choice {
                    self.open(&path, ctx);
                }
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        self.open_files(dropped, ctx);
        let Some(result) = self.loading.as_ref().and_then(|l| l.rx.try_recv().ok()) else { return };
        self.loading = None;
        match result {
            Err(e) => {
                self.error = Some(e);
                self.autoplay = false;
            }
            Ok((track, peaks)) => {
                self.save_session();
                if let Some(old) = self.loaded.take() {
                    old.spectrogram.cancel();
                }
                let track = Arc::new(track);
                self.session = Session::load(&track.path);
                {
                    let mut engine = self.engine.lock().unwrap();
                    engine.set_track(Some(track.clone()));
                    engine.set_speed(self.session.speed);
                    if std::mem::take(&mut self.autoplay) {
                        engine.play();
                    }
                }
                // Reopen the device at the track's rate where possible.
                self.output = Output::open(&self.engine, Some(track.sample_rate));
                let wake = ctx.clone();
                let spectrogram = Spectrogram::start(track.clone(), move || {
                    wake.request_repaint_after(std::time::Duration::from_millis(100))
                });
                self.view.start = 0.0;
                self.view.span = track.duration().min(30.0);
                self.tiles = Tiles::default();
                self.pos = 0.0;
                self.cue = 0.0;
                self.drag = Drag::None;
                self.loaded = Some(Loaded { track, peaks: Arc::new(peaks), spectrogram });
            }
        }
    }

    fn save_session(&mut self) {
        if let Some(loaded) = &self.loaded {
            if std::mem::take(&mut self.session_dirty) {
                self.session.save(&loaded.track.path);
            }
        }
    }

    fn save_settings(&mut self) {
        let current = serde_json::to_string(&self.settings).unwrap_or_default();
        if current != self.saved_settings {
            self.settings.save();
            self.saved_settings = current;
        }
    }

    pub fn touch_session(&mut self) {
        self.session_dirty = true;
    }

    // ---------------------------------------------------------- transport

    /// Seek, and make this the point "return on pause" comes back to.
    pub fn seek(&mut self, secs: f64) {
        let secs = secs.clamp(0.0, self.duration());
        self.engine.lock().unwrap().seek(secs);
        self.pos = secs;
        self.cue = secs;
    }

    /// Seek from outside the zoomed display, scrolling it to show the playhead.
    fn seek_and_show(&mut self, secs: f64) {
        self.seek(secs);
        if self.pos < self.view.start || self.pos > self.view.start + self.view.span {
            self.view.start = self.pos - self.view.span * 0.2;
            self.clamp_view();
        }
    }

    fn toggle_play(&mut self) {
        if self.loaded.is_none() {
            return;
        }
        let mut engine = self.engine.lock().unwrap();
        if engine.is_playing() {
            engine.pause();
            if self.settings.return_on_pause {
                engine.seek(self.cue);
                self.pos = self.cue;
            }
        } else {
            engine.play();
            self.cue = engine.position();
        }
        self.playing = engine.is_playing();
    }

    fn add_bookmark(&mut self, edit_label: bool) {
        if self.loaded.is_none() {
            return;
        }
        let id = self.session.add_bookmark(self.pos);
        if edit_label {
            self.focus_label = Some(id);
        }
        self.touch_session();
    }

    fn previous_bookmark(&mut self) {
        // While playing, the bookmark just passed keeps slipping behind the
        // playhead, so allow a second to press again and skip further back.
        let grace = if self.playing { 1.0 } else { 0.01 };
        let target = self.session.previous(self.pos, grace).map_or(0.0, |b| b.time);
        self.seek_and_show(target);
    }

    fn next_bookmark(&mut self) {
        if let Some(target) = self.session.next(self.pos).map(|b| b.time) {
            self.seek_and_show(target);
        }
    }

    fn set_speed(&mut self, speed: f64) {
        self.session.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        self.engine.lock().unwrap().set_speed(self.session.speed);
        self.touch_session();
    }

    fn set_keep_pitch(&mut self, keep_pitch: bool) {
        self.settings.keep_pitch = keep_pitch;
        self.engine.lock().unwrap().set_keep_pitch(keep_pitch);
    }

    /// The region the engine should loop, given the loop mode.
    fn loop_region(&self) -> Option<(f64, f64)> {
        match self.settings.loop_mode {
            LoopMode::Off => None,
            LoopMode::Selection => self.session.selection,
            LoopMode::Section => Some(self.session.section(self.pos, self.duration())),
            LoopMode::Track => Some((0.0, self.duration())),
        }
    }

    // --------------------------------------------------------------- view

    pub fn clamp_view(&mut self) {
        let duration = self.duration().max(MIN_SPAN);
        self.view.span = self.view.span.clamp(MIN_SPAN, duration);
        self.view.start = self.view.start.clamp(0.0, duration - self.view.span);
        let range = (self.view.pitch_hi - self.view.pitch_lo).clamp(6.0, PITCH_MAX - PITCH_MIN);
        self.view.pitch_lo = self.view.pitch_lo.clamp(PITCH_MIN, PITCH_MAX - range);
        self.view.pitch_hi = self.view.pitch_lo + range;
    }

    /// Zoom the timeline by `factor` (> 1 zooms out) keeping `anchor` in place.
    pub fn zoom_time(&mut self, factor: f64, anchor: f64) {
        let rel = (anchor - self.view.start) / self.view.span;
        self.view.span = (self.view.span * factor).clamp(MIN_SPAN, self.duration().max(MIN_SPAN));
        self.view.start = anchor - rel * self.view.span;
        self.clamp_view();
    }

    fn follow_playhead(&mut self, now: f64) {
        if !self.playing || !matches!(self.drag, Drag::None) || now - self.last_pan < 2.0 {
            return;
        }
        match self.settings.follow {
            Follow::Off => {}
            Follow::Page => {
                if self.pos < self.view.start || self.pos > self.view.start + self.view.span * 0.97 {
                    self.view.start = self.pos - self.view.span * 0.03;
                }
            }
            Follow::Centred => self.view.start = self.pos - self.view.span * 0.5,
        }
        self.clamp_view();
    }

    fn set_fullscreen(&mut self, on: bool, ctx: &egui::Context) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    // ------------------------------------------------------------ hotkeys

    fn hotkeys(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.open_dialog(ctx);
        }
        if ctx.text_edit_focused() {
            return;
        }
        let key = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let shift = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, k));

        if key(Key::Space) {
            self.toggle_play();
        }
        if key(Key::F) || key(Key::F11) {
            self.set_fullscreen(!self.fullscreen, ctx);
        }
        if key(Key::Escape) {
            if self.fullscreen {
                self.set_fullscreen(false, ctx);
            } else if self.session.selection.take().is_some() {
                self.touch_session();
            }
        }
        if key(Key::V) {
            let i = ViewMode::ALL.iter().position(|&m| m == self.settings.view).unwrap_or(0);
            self.settings.view = ViewMode::ALL[(i + 1) % ViewMode::ALL.len()];
        }
        if shift(Key::B) {
            self.add_bookmark(true);
        }
        if key(Key::B) {
            self.add_bookmark(false);
        }
        if key(Key::OpenBracket) || key(Key::PageUp) {
            self.previous_bookmark();
        }
        if key(Key::CloseBracket) || key(Key::PageDown) {
            self.next_bookmark();
        }
        const DIGITS: [Key; 9] =
            [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (n, k) in DIGITS.into_iter().enumerate() {
            if key(k) {
                if let Some(time) = self.session.bookmarks.get(n).map(|b| b.time) {
                    self.seek_and_show(time);
                }
            }
        }
        if key(Key::N) {
            self.playlist_step(1, ctx);
        }
        if key(Key::P) {
            self.playlist_step(-1, ctx);
        }
        if key(Key::T) {
            self.set_keep_pitch(!self.settings.keep_pitch);
        }
        if key(Key::L) {
            self.settings.loop_mode = match self.settings.loop_mode {
                LoopMode::Off if self.session.selection.is_some() => LoopMode::Selection,
                LoopMode::Off => LoopMode::Section,
                _ => LoopMode::Off,
            };
        }
        // Loop in / out points at the playhead.
        if key(Key::I) && self.loaded.is_some() {
            let end = self.session.selection.map_or(self.duration(), |s| s.1);
            let end = if end > self.pos { end } else { self.duration() };
            self.session.selection = Some((self.pos, end));
            self.settings.loop_mode = LoopMode::Selection;
            self.touch_session();
        }
        if key(Key::O) && self.loaded.is_some() {
            let start = self.session.selection.map_or(0.0, |s| s.0);
            let start = if start < self.pos { start } else { 0.0 };
            self.session.selection = Some((start, self.pos));
            self.settings.loop_mode = LoopMode::Selection;
            self.touch_session();
        }
        for (k, step) in [(Key::ArrowLeft, -1.0), (Key::ArrowRight, 1.0)] {
            if key(k) {
                self.seek_and_show(self.pos + 2.0 * step);
            }
            if shift(k) {
                self.seek_and_show(self.pos + 10.0 * step);
            }
        }
        if key(Key::Home) {
            self.seek_and_show(0.0);
        }
        if key(Key::ArrowUp) {
            self.set_speed(((self.session.speed + 0.05) * 20.0).round() / 20.0);
        }
        if key(Key::ArrowDown) {
            self.set_speed(((self.session.speed - 0.05) * 20.0).round() / 20.0);
        }
        if key(Key::Equals) || key(Key::Plus) || shift(Key::Plus) || shift(Key::Equals) {
            self.zoom_time(0.5, self.pos.clamp(self.view.start, self.view.start + self.view.span));
        }
        if key(Key::Minus) {
            self.zoom_time(2.0, self.pos.clamp(self.view.start, self.view.start + self.view.span));
        }
        if key(Key::Z) {
            self.view.start = 0.0;
            self.view.span = self.duration();
            self.clamp_view();
        }
        if key(Key::F1) || shift(Key::Questionmark) || key(Key::Questionmark) || shift(Key::Slash) {
            self.show_help = !self.show_help;
        }
    }

    // ----------------------------------------------------------------- UI

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("Open…").on_hover_text("Open an audio file (Ctrl+O)").clicked() {
                self.open_dialog(ui.ctx());
            }
            match (&self.loading, &self.loaded) {
                (Some(loading), _) => {
                    ui.label(format!("Loading {}…", loading.name));
                }
                (None, Some(loaded)) => {
                    ui.label(RichText::new(&loaded.track.name).strong());
                    ui.weak(format!("{} Hz", loaded.track.sample_rate));
                    if !loaded.spectrogram.is_complete() {
                        ui.weak(format!("· analysing {:.0}%", loaded.spectrogram.progress() * 100.0));
                    }
                }
                _ => {}
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("?").on_hover_text("Keyboard shortcuts (F1)").clicked() {
                    self.show_help = !self.show_help;
                }
                for mode in ViewMode::ALL.into_iter().rev() {
                    ui.selectable_value(&mut self.settings.view, mode, mode.label());
                }
                ui.weak("View (V)");
                if let Some(error) = &self.output.error {
                    ui.colored_label(ui.visuals().warn_fg_color, "No sound").on_hover_text(error);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings.sidebar, SidebarTab::Bookmarks, RichText::new("Bookmarks").size(15.0));
            ui.selectable_value(&mut self.settings.sidebar, SidebarTab::Playlist, RichText::new("Playlist").size(15.0));
        });
        ui.separator();
        let options_height = 250.0;
        let list_height = (ui.available_height() - options_height).max(80.0);
        match self.settings.sidebar {
            SidebarTab::Bookmarks => self.bookmark_list(ui, list_height),
            SidebarTab::Playlist => self.playlist(ui, list_height),
        }
        self.options(ui);
    }

    fn playlist(&mut self, ui: &mut Ui, height: f32) {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.confirm_delete { "Really delete?" } else { "Delete" };
                let delete = ui.button(label).on_hover_text("Delete this playlist (the audio files are untouched)");
                if delete.clicked() {
                    if self.confirm_delete || self.library.playlist().tracks.is_empty() {
                        self.library.remove_playlist();
                        self.library_dirty = true;
                        self.confirm_delete = false;
                    } else {
                        self.confirm_delete = true;
                    }
                } else if !delete.hovered() {
                    self.confirm_delete = false;
                }
                if ui.button("New").on_hover_text("Start a new, empty playlist").clicked() {
                    self.library.add_playlist();
                    self.library_dirty = true;
                }
                let mut chosen = self.library.current;
                egui::ComboBox::from_id_salt("playlists")
                    .width(ui.available_width())
                    .selected_text(self.library.playlist().name.clone())
                    .show_ui(ui, |ui| {
                        for (i, playlist) in self.library.playlists.iter().enumerate() {
                            ui.selectable_value(&mut chosen, i, format!("{} ({})", playlist.name, playlist.tracks.len()));
                        }
                    });
                if chosen != self.library.current {
                    self.library.current = chosen;
                    self.library_dirty = true;
                }
            });
        });
        ui.horizontal(|ui| {
            ui.weak("Name");
            let name = &mut self.library.playlist_mut().name;
            if ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).changed() {
                self.library_dirty = true;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("+ Files…").on_hover_text("Add audio files (or drop several on the window)").clicked() {
                self.add_dialog(ui.ctx());
            }
            let current = self.current_path.clone().filter(|p| self.library.playlist().position(Some(p)).is_none());
            if ui
                .add_enabled(current.is_some(), egui::Button::new("+ Current track"))
                .on_hover_text("Add the track that is open now")
                .clicked()
            {
                self.add_to_playlist(current.into_iter().collect());
            }
        });
        ui.add_space(2.0);

        enum Action {
            Open(PathBuf),
            Remove(usize),
            Move(usize, usize),
        }
        let mut action = None;
        let tracks = &self.library.playlist().tracks;
        let current = self.library.playlist().position(self.current_path.as_deref());
        let list_height = (height - 104.0).max(60.0);
        egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
            if tracks.is_empty() {
                ui.weak("This playlist is empty.\nAdd files, or drop several on the window.");
            }
            for (i, path) in tracks.iter().enumerate() {
                ui.horizontal(|ui| {
                    let playing = current == Some(i);
                    let colour = if playing { ACCENT } else { ui.visuals().weak_text_color() };
                    let number = RichText::new(format!("{}", i + 1)).color(colour).monospace();
                    ui.add_sized([18.0, 18.0], egui::Label::new(number));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Remove from the playlist").clicked() {
                            action = Some(Action::Remove(i));
                        }
                        if ui.add_enabled(i + 1 < tracks.len(), egui::Button::new(RichText::new("↓").monospace()).small()).clicked() {
                            action = Some(Action::Move(i, i + 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new(RichText::new("↑").monospace()).small()).clicked() {
                            action = Some(Action::Move(i, i - 1));
                        }
                        let name = path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy();
                        let mut text = RichText::new(name);
                        if playing {
                            text = text.color(ACCENT);
                        }
                        let row = egui::Button::new(text).frame(false).truncate();
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if ui.add(row).on_hover_text(path.to_string_lossy()).clicked() {
                                action = Some(Action::Open(path.clone()));
                            }
                        });
                    });
                });
            }
        });
        ui.checkbox(&mut self.settings.auto_advance, "Play the next track automatically")
            .on_hover_text("When a track ends and no loop is on (N / P skip by hand)");
        match action {
            Some(Action::Open(path)) => {
                self.autoplay = self.playing;
                self.open(&path, ui.ctx());
            }
            Some(Action::Remove(i)) => drop(self.library.playlist_mut().tracks.remove(i)),
            Some(Action::Move(from, to)) => self.library.playlist_mut().tracks.swap(from, to),
            None => return,
        }
        self.library_dirty = true;
    }

    fn bookmark_list(&mut self, ui: &mut Ui, height: f32) {
        ui.horizontal(|ui| {
            ui.weak("B drops one at the playhead");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(self.loaded.is_some(), egui::Button::new("+ Add"))
                    .on_hover_text("Drop a bookmark at the playhead (B; Shift+B to name it)")
                    .clicked()
                {
                    self.add_bookmark(true);
                }
            });
        });
        ui.add_space(2.0);
        let list_height = (height - 26.0).max(60.0);
        let mut seek = None;
        let mut delete = None;
        let mut edited = false;
        let current = self.session.bookmarks.iter().rposition(|b| b.time <= self.pos + 1e-6);
        egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
            if self.session.bookmarks.is_empty() {
                ui.weak("No bookmarks yet.");
            }
            for (i, bookmark) in self.session.bookmarks.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let number = if i < 9 { format!("{}", i + 1) } else { "·".to_owned() };
                    let colour = if current == Some(i) { BOOKMARK } else { ui.visuals().weak_text_color() };
                    ui.add_sized([14.0, 18.0], egui::Label::new(RichText::new(number).color(colour).monospace()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Delete bookmark").clicked() {
                            delete = Some(bookmark.id);
                        }
                        let time = RichText::new(format_time(bookmark.time, 1)).monospace();
                        if ui.add(egui::Button::new(time).frame(false)).on_hover_text("Jump here").clicked() {
                            seek = Some(bookmark.time);
                        }
                        let mut edit =
                            egui::TextEdit::singleline(&mut bookmark.label).desired_width(f32::INFINITY).show(ui);
                        if self.focus_label == Some(bookmark.id) {
                            // Focus with the label selected, so typing replaces it.
                            self.focus_label = None;
                            edit.response.request_focus();
                            let all = egui::text::CCursorRange::select_all(&edit.galley);
                            edit.state.cursor.set_char_range(Some(all));
                            edit.state.store(ui.ctx(), edit.response.id);
                        }
                        edited |= edit.response.changed();
                    });
                });
            }
        });
        if let Some(time) = seek {
            self.seek_and_show(time);
        }
        if let Some(id) = delete {
            self.session.bookmarks.retain(|b| b.id != id);
            edited = true;
        }
        if edited {
            self.touch_session();
        }
    }

    fn options(&mut self, ui: &mut Ui) {
        ui.separator();
        ui.label(RichText::new("Playhead").strong());
        egui::ComboBox::from_id_salt("follow")
            .width(ui.available_width() - 8.0)
            .selected_text(self.settings.follow.label())
            .show_ui(ui, |ui| {
                for follow in Follow::ALL {
                    ui.selectable_value(&mut self.settings.follow, follow, follow.label());
                }
            })
            .response
            .on_hover_text("How the display keeps up with the playhead while playing");
        ui.checkbox(&mut self.settings.return_on_pause, "Pause returns to start point")
            .on_hover_text("When pausing, put the playhead back where playback began");

        ui.add_space(6.0);
        ui.label(RichText::new("Loop").strong());
        self.loop_combo(ui, ui.available_width() - 8.0);
        ui.add(egui::Slider::new(&mut self.settings.loop_gap, 0.0..=4.0).step_by(0.25).suffix(" s").text("gap"))
            .on_hover_text("Silence between loop repeats");

        ui.add_space(6.0);
        ui.label(RichText::new("Spectrogram").strong());
        ui.add(egui::Slider::new(&mut self.settings.range_db, 30.0..=100.0).step_by(1.0).suffix(" dB").text("range"))
            .on_hover_text("Dynamic range shown: lower hides quiet detail");
    }

    fn loop_combo(&mut self, ui: &mut Ui, width: f32) {
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .width(width)
            .selected_text(self.settings.loop_mode.label())
            .show_ui(ui, |ui| {
                for mode in LoopMode::ALL {
                    ui.selectable_value(&mut self.settings.loop_mode, mode, mode.label());
                }
            })
            .response
            .on_hover_text(
                "What to repeat (L toggles)\nSelection: drag across the display, or press I / O\n\
                 Bookmark section: between the bookmarks around the playhead",
            );
    }

    fn transport(&mut self, ui: &mut Ui) {
        let duration = self.duration();
        ui.add_space(4.0);
        // The usual full-song seek bar: elapsed, bar, total.
        ui.horizontal(|ui| {
            let mono = |t: f64| RichText::new(format_time(t, 0)).monospace();
            ui.label(mono(self.pos));
            let width = (ui.available_width() - 52.0).max(50.0);
            let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click_and_drag());
            self.seek_bar(ui, rect, &response);
            ui.label(mono(duration));
        });
        ui.horizontal(|ui| {
            let enabled = self.loaded.is_some();
            if icon_button(ui, Icon::Previous, enabled).on_hover_text("Previous bookmark ( [ )").clicked() {
                self.previous_bookmark();
            }
            let icon = if self.playing { Icon::Pause } else { Icon::Play };
            if icon_button(ui, icon, enabled).on_hover_text("Play / pause (Space)").clicked() {
                self.toggle_play();
            }
            if icon_button(ui, Icon::Next, enabled).on_hover_text("Next bookmark ( ] )").clicked() {
                self.next_bookmark();
            }
            ui.separator();
            ui.label("Loop");
            self.loop_combo(ui, 130.0);
            ui.separator();
            ui.label("Speed");
            let mut percent = self.session.speed * 100.0;
            let slider = egui::Slider::new(&mut percent, MIN_SPEED * 100.0..=MAX_SPEED * 100.0)
                .step_by(1.0)
                .suffix("%");
            if ui.add(slider).on_hover_text("Playback speed (↑ / ↓)").changed() {
                self.set_speed(percent / 100.0);
            }
            if ui.add_enabled(self.session.speed != 1.0, egui::Button::new("1×")).on_hover_text("Back to normal speed").clicked() {
                self.set_speed(1.0);
            }
            let mut keep_pitch = self.settings.keep_pitch;
            let hint = "On: time-stretch, so slowing down keeps the pitch.\n\
                        Off: plain resampling like a tape machine, so pitch follows speed (T)";
            if ui.checkbox(&mut keep_pitch, "Keep pitch").on_hover_text(hint).changed() {
                self.set_keep_pitch(keep_pitch);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.fullscreen { "Exit full screen" } else { "Full screen" };
                if ui.button(label).on_hover_text("Toggle the full-screen display (F)").clicked() {
                    self.set_fullscreen(!self.fullscreen, ui.ctx());
                }
                ui.separator();
                let slider = egui::Slider::new(&mut self.settings.volume, 0.0..=1.0).show_value(false);
                if ui.add(slider).changed() {
                    self.engine.lock().unwrap().set_volume(self.settings.volume);
                }
                ui.label("Volume");
            });
        });
        ui.add_space(2.0);
    }

    fn seek_bar(&mut self, ui: &Ui, rect: Rect, response: &egui::Response) {
        let duration = self.duration();
        let painter = ui.painter_at(rect.expand(6.0));
        let groove = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        painter.rect_filled(groove, CornerRadius::same(2), ui.visuals().extreme_bg_color);
        if duration <= 0.0 {
            return;
        }
        let x = |t: f64| rect.left() + (t / duration) as f32 * rect.width();
        if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.is_pointer_button_down_on()) {
            let t = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
            self.seek_and_show(t);
        }
        // The stretch shown by the zoomed display.
        let window = Rect::from_x_y_ranges(
            x(self.view.start)..=x(self.view.start + self.view.span),
            groove.top() - 5.0..=groove.bottom() + 5.0,
        );
        painter.rect_filled(window, CornerRadius::same(2), Color32::from_white_alpha(18));
        if let Some((a, b)) = self.loop_region() {
            let band = Rect::from_x_y_ranges(x(a)..=x(b), groove.y_range());
            painter.rect_filled(band, CornerRadius::ZERO, LOOP.gamma_multiply(0.6));
        }
        let played = Rect::from_min_max(groove.min, pos2(x(self.pos), groove.bottom()));
        painter.rect_filled(played, CornerRadius::same(2), ACCENT.gamma_multiply(0.8));
        for bookmark in &self.session.bookmarks {
            let bx = x(bookmark.time);
            painter.vline(bx, groove.top() - 6.0..=groove.top() - 1.0, Stroke::new(1.5, BOOKMARK));
        }
        let radius = if response.hovered() || response.dragged() { 6.5 } else { 5.0 };
        painter.circle_filled(pos2(x(self.pos), groove.center().y), radius, ACCENT);
        if let Some(pointer) = response.hover_pos() {
            let t = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
            painter.text(
                pos2(pointer.x, rect.top() - 2.0),
                Align2::CENTER_BOTTOM,
                format_time(t, 0),
                FontId::monospace(10.0),
                ui.visuals().text_color(),
            );
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        const KEYS: &[(&str, &str)] = &[
            ("Space", "Play / pause"),
            ("← / →", "Seek 2 s (Shift: 10 s)"),
            ("Home", "Back to the start"),
            ("↑ / ↓", "Speed up / slow down by 5%"),
            ("T", "Speed keeps pitch (time-stretch) on / off"),
            ("N / P", "Next / previous track in the playlist"),
            ("B", "Drop a bookmark at the playhead (Shift+B: and name it)"),
            ("[ / ]", "Previous / next bookmark (also PgUp / PgDn)"),
            ("1 – 9", "Jump to bookmark 1 – 9"),
            ("L", "Loop on / off"),
            ("I / O", "Set the loop start / end at the playhead"),
            ("Esc", "Leave full screen, or clear the selection"),
            ("V", "Cycle waveform / spectrogram / both"),
            ("F", "Full-screen display"),
            ("+ / −", "Zoom in / out"),
            ("Z", "Zoom out to the whole track"),
            ("Ctrl+O", "Open a file"),
        ];
        const MOUSE: &[(&str, &str)] = &[
            ("Click", "Move the playhead"),
            ("Drag", "Select a loop region (drag its edges to adjust)"),
            ("Right-drag", "Pan the display"),
            ("Scroll", "Zoom the timeline (Shift: pan)"),
            ("Alt+scroll", "Zoom the pitch axis"),
            ("Scroll on the piano", "Pan the pitch axis (Ctrl: zoom)"),
            ("Drag a bookmark flag", "Move the bookmark"),
        ];
        let mut open = self.show_help;
        egui::Window::new("Shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            for (title, rows) in [("Keyboard", KEYS), ("Mouse, in the display", MOUSE)] {
                ui.label(RichText::new(title).strong());
                egui::Grid::new(title).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (keys, action) in rows {
                        ui.label(RichText::new(*keys).monospace().color(ACCENT));
                        ui.label(*action);
                        ui.end_row();
                    }
                });
                ui.add_space(8.0);
            }
        });
        self.show_help = open;
    }

    fn empty_state(&self, ui: &mut Ui) {
        ui.centered_and_justified(|ui| match &self.loading {
            Some(loading) => {
                let done = loading.progress.load(Ordering::Relaxed) as f32 / 10.0;
                ui.label(format!("Decoding {}… {done:.0}%", loading.name));
            }
            None => {
                ui.weak("Open an audio file (Ctrl+O), or drop one here.");
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.poll_background(&ctx);
        let finished;
        {
            let mut engine = self.engine.lock().unwrap();
            self.pos = engine.position();
            self.playing = engine.is_playing();
            finished = engine.take_finished();
        }
        if finished {
            // Move on through the playlist, unless a loop is being rehearsed.
            self.playing = true;
            let advance = self.settings.auto_advance && self.settings.loop_mode == LoopMode::Off;
            if !(advance && self.playlist_step(1, &ctx)) {
                self.playing = false;
                if self.settings.return_on_pause {
                    self.engine.lock().unwrap().seek(self.cue);
                    self.pos = self.cue;
                }
            }
        }
        self.hotkeys(&ctx);

        egui::Panel::bottom("transport").show(ui, |ui| self.transport(ui));
        if !self.fullscreen {
            egui::Panel::top("top").show(ui, |ui| {
                ui.add_space(3.0);
                self.top_bar(ui);
                ui.add_space(1.0);
            });
            egui::Panel::right("sidebar").default_size(260.0).size_range(200.0..=420.0).show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if self.loaded.is_some() && self.loading.is_none() {
                self.display(ui);
            } else {
                self.empty_state(ui);
            }
        });
        if self.show_help {
            self.help_window(&ctx);
        }

        self.follow_playhead(now);
        let region = self.loop_region();
        self.engine.lock().unwrap().set_loop(region, self.settings.loop_gap);

        if self.playing {
            ctx.request_repaint();
        } else if self.loading.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Persist edits once the pointer and keyboard are idle.
        if !ctx.egui_is_using_pointer() && !ctx.text_edit_focused() {
            self.save_session();
            self.save_settings();
            if std::mem::take(&mut self.library_dirty) {
                self.library.save();
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.session_dirty |= self.loaded.is_some();
        self.save_session();
        self.save_settings();
        self.library.save();
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Previous,
    Next,
}

/// Absolute form of a path, so playlist entries survive a change of directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A transport button with a painted icon (no dependence on font glyphs).
fn icon_button(ui: &mut Ui, icon: Icon, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 24.0), sense);
    let visuals = ui.style().interact(&response);
    let colour = if enabled { visuals.fg_stroke.color } else { ui.visuals().weak_text_color() };
    ui.painter().rect_filled(rect, CornerRadius::same(4), visuals.bg_fill);
    let c = rect.center();
    let triangle = |x: f32, dir: f32| {
        Shape::convex_polygon(
            vec![pos2(x - 5.0 * dir, c.y - 6.0), pos2(x + 5.0 * dir, c.y), pos2(x - 5.0 * dir, c.y + 6.0)],
            colour,
            Stroke::NONE,
        )
    };
    let bar = |x: f32, w: f32| Shape::rect_filled(Rect::from_center_size(pos2(x, c.y), vec2(w, 12.0)), 0.0, colour);
    let painter = ui.painter();
    match icon {
        Icon::Play => drop(painter.add(triangle(c.x + 1.0, 1.0))),
        Icon::Pause => {
            painter.add(bar(c.x - 3.5, 4.0));
            painter.add(bar(c.x + 3.5, 4.0));
        }
        Icon::Previous => {
            painter.add(bar(c.x - 6.0, 2.5));
            painter.add(triangle(c.x + 1.5, -1.0));
        }
        Icon::Next => {
            painter.add(triangle(c.x - 1.5, 1.0));
            painter.add(bar(c.x + 6.0, 2.5));
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0.0, 0), "0:00");
        assert_eq!(format_time(75.9, 0), "1:15");
        assert_eq!(format_time(75.25, 2), "1:15.25");
        assert_eq!(format_time(59.999, 1), "0:59.9");
        assert_eq!(format_time(3725.0, 0), "62:05");
    }
}
