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
use crate::metadata::{self, Meta, MetaCache, SortColumn};
use crate::midi::{self, MidiSong};
use crate::playlist_io;
use crate::session::{Follow, KeySound, Library, LoopMode, Session, Settings, SidebarTab, Theme, ViewMode};
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

enum PlaylistAction {
    Open(PathBuf),
    Remove(usize),
    Move(usize, usize),
    Sort(SortColumn),
}

pub struct Loaded {
    pub track: Arc<Track>,
    pub peaks: Arc<Peaks>,
    pub spectrogram: Arc<Spectrogram>,
    /// The notes, if this is a MIDI file.
    pub midi: Option<Arc<MidiSong>>,
    /// A MIDI file rendered with the built-in tone, for want of a SoundFont.
    basic_synth: bool,
}

/// What the loading thread hands back: audio, waveform peaks, and for a
/// MIDI file its notes and whether the built-in tone had to be used.
type Decoded = (Track, Peaks, Option<(MidiSong, bool)>);

struct Loading {
    name: String,
    progress: Arc<AtomicU32>,
    rx: Receiver<Result<Decoded, String>>,
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
    meta: MetaCache,
    /// The column the playlist was last sorted by, and whether ascending.
    playlist_sort: Option<(SortColumn, bool)>,
    export_dialog: Option<Receiver<Option<PathBuf>>>,
    show_preferences: bool,

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
    /// A loop start placed with Ctrl+click, waiting for its end.
    pub loop_anchor: Option<f64>,
    /// The piano key being played with the mouse.
    pub sounding: Option<i32>,
    /// Height of the sidebar's display settings on the last frame.
    settings_height: f32,

    /// The SoundFont for MIDI files and (optionally) the piano.
    soundfont: Option<Arc<rustysynth::SoundFont>>,
    soundfont_loading: Option<Receiver<Result<Arc<rustysynth::SoundFont>, String>>>,
    soundfont_error: Option<String>,
    soundfont_dialog: Option<Receiver<Option<PathBuf>>>,
    /// Bit per MIDI channel hidden in the MIDI view.
    pub midi_hidden: u16,
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
            meta: {
                let ctx = cc.egui_ctx.clone();
                MetaCache::new(move || ctx.request_repaint())
            },
            playlist_sort: None,
            export_dialog: None,
            show_preferences: false,
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
            loop_anchor: None,
            sounding: None,
            settings_height: 60.0,
            soundfont: None,
            soundfont_loading: None,
            soundfont_error: None,
            soundfont_dialog: None,
            midi_hidden: 0,
        };
        app.load_soundfont(&cc.egui_ctx);
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
        let (lists, files): (Vec<PathBuf>, Vec<PathBuf>) =
            files.iter().map(|p| absolute(p)).partition(|p| playlist_io::is_playlist(p));
        // A playlist file becomes a playlist of its own, named after the file.
        for list in lists {
            match playlist_io::read(&list) {
                Err(e) => self.error = Some(e),
                Ok(tracks) => {
                    if files.is_empty() && self.current_path.is_none() {
                        self.open(&tracks[0], ctx);
                    }
                    let name = list.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.library.import(&name, tracks);
                    self.library_dirty = true;
                    self.settings.sidebar = SidebarTab::Playlist;
                    self.settings.last_dir = list.parent().map(Path::to_path_buf);
                }
            }
        }
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

    /// Add files to the showing playlist; playlist files add what they list.
    fn add_to_playlist(&mut self, files: Vec<PathBuf>) {
        let mut tracks = Vec::new();
        for file in files {
            if !playlist_io::is_playlist(&file) {
                tracks.push(file);
                continue;
            }
            match playlist_io::read(&file) {
                Ok(listed) => tracks.extend(listed),
                Err(e) => self.error = Some(e),
            }
        }
        self.library.playlist_mut().add(tracks);
        self.playlist_sort = None;
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
        let font = self.soundfont.clone();
        std::thread::spawn(move || {
            let result = if midi::is_midi(&path_buf) {
                midi::load(&path_buf, font.as_ref(), &thread_progress).map(|(track, song)| (track, Some((song, font.is_none()))))
            } else {
                track::decode(&path_buf, &thread_progress).map(|track| (track, None))
            };
            let result = result.map(|(track, song)| (Peaks::build(&track), track, song)).map(|(p, t, s)| (t, p, s));
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
            let mut dialog = rfd::FileDialog::new()
                .add_filter("Audio and playlists", &[track::AUDIO_EXTENSIONS, playlist_io::EXTENSIONS].concat())
                .add_filter("Audio", track::AUDIO_EXTENSIONS)
                .add_filter("Playlists", playlist_io::EXTENSIONS);
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
            let mut dialog = rfd::FileDialog::new()
                .add_filter("Audio and playlists", &[track::AUDIO_EXTENSIONS, playlist_io::EXTENSIONS].concat())
                .add_filter("Audio", track::AUDIO_EXTENSIONS)
                .add_filter("Playlists", playlist_io::EXTENSIONS);
            if let Some(dir) = dir {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_files());
            ctx.request_repaint();
        });
        self.add_dialog = Some(rx);
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        if self.export_dialog.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (dir, ctx, name) = (self.settings.last_dir.clone(), ctx.clone(), self.library.playlist().name.clone());
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("M3U playlist", &["m3u8", "m3u"])
                .add_filter("PLS playlist", &["pls"])
                .add_filter("XSPF playlist", &["xspf"])
                .set_file_name(format!("{name}.m3u8"));
            if let Some(dir) = dir {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.save_file());
            ctx.request_repaint();
        });
        self.export_dialog = Some(rx);
    }

    fn has_midi(&self) -> bool {
        self.loaded.as_ref().is_some_and(|l| l.midi.is_some())
    }

    /// (Re)load the SoundFont named in the settings, or the system's.
    fn load_soundfont(&mut self, ctx: &egui::Context) {
        self.soundfont = None;
        self.soundfont_error = None;
        self.soundfont_loading = None;
        self.apply_key_sound();
        let Some(path) = self.settings.soundfont.clone().or_else(midi::system_soundfont) else {
            self.soundfont_changed(ctx);
            return;
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(midi::load_soundfont(&path));
            ctx.request_repaint();
        });
        self.soundfont_loading = Some(rx);
    }

    /// Re-render the open MIDI file with the new SoundFont.
    fn soundfont_changed(&mut self, ctx: &egui::Context) {
        self.apply_key_sound();
        if let Some(path) = self.current_path.clone().filter(|p| midi::is_midi(p)) {
            self.autoplay = false;
            self.open(&path, ctx);
        }
    }

    fn apply_key_sound(&mut self) {
        let font = self.soundfont.clone().filter(|_| self.settings.key_sound == KeySound::SoundFont);
        self.engine.lock().unwrap().set_key_sound(font, self.settings.key_program);
    }

    fn soundfont_dialog(&mut self, ctx: &egui::Context) {
        if self.soundfont_dialog.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(rfd::FileDialog::new().add_filter("SoundFont", &["sf2"]).pick_file());
            ctx.request_repaint();
        });
        self.soundfont_dialog = Some(rx);
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(choice) = self.soundfont_dialog.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.soundfont_dialog = None;
            if let Some(path) = choice {
                self.settings.soundfont = Some(path);
                self.load_soundfont(ctx);
            }
        }
        if let Some(result) = self.soundfont_loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.soundfont_loading = None;
            match result {
                Ok(font) => self.soundfont = Some(font),
                Err(e) => self.soundfont_error = Some(e),
            }
            self.soundfont_changed(ctx);
        }
        if let Some(choice) = self.export_dialog.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.export_dialog = None;
            if let Some(mut path) = choice {
                if !playlist_io::is_playlist(&path) {
                    path.as_mut_os_string().push(".m3u8");
                }
                self.error = playlist_io::write(&path, &self.library.playlist().tracks).err();
            }
        }
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
                self.open_files(choice.into_iter().collect(), ctx);
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
            Ok((track, peaks, song)) => {
                self.save_session();
                let had_midi = self.has_midi();
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
                self.loop_anchor = None;
                // A MIDI file brings its own view with it.
                self.midi_hidden = 0;
                let basic_synth = song.as_ref().is_some_and(|s| s.1);
                let midi = song.map(|s| Arc::new(s.0));
                if midi.is_some() && !had_midi && self.settings.view != ViewMode::Playlist {
                    self.settings.view = ViewMode::Midi;
                }
                self.loaded = Some(Loaded { track, peaks: Arc::new(peaks), spectrogram, midi, basic_synth });
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
        if self.vertical() && self.settings.follow != Follow::Off {
            self.align_to_piano();
        } else if self.pos < self.view.start || self.pos > self.view.start + self.view.span {
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

    /// In piano mode: put the playhead on the edge of the piano, so what is
    /// coming falls onto the keys (or, reversed, what was played rises from them).
    fn align_to_piano(&mut self) {
        self.view.start = if self.settings.reverse { self.pos - self.view.span } else { self.pos };
        self.clamp_view();
    }

    /// Hold a piano key down (or release it, with `None`).
    pub fn set_key(&mut self, key: Option<i32>) {
        self.engine.lock().unwrap().set_key(key, self.settings.tone_volume * self.settings.volume);
    }

    pub fn clamp_view(&mut self) {
        let duration = self.duration().max(MIN_SPAN);
        self.view.span = self.view.span.clamp(MIN_SPAN, duration);
        if self.vertical() {
            // The playhead can sit on the piano edge at either end of the track.
            self.view.start = self.view.start.clamp(-self.view.span, duration);
        } else {
            self.view.start = self.view.start.clamp(0.0, duration - self.view.span);
        }
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
        if self.vertical() {
            if self.settings.follow != Follow::Off {
                self.align_to_piano();
            }
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
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
            self.show_preferences = !self.show_preferences;
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
            } else if self.loop_anchor.take().is_none() && self.session.selection.take().is_some() {
                self.touch_session();
            }
        }
        if key(Key::V) {
            let views = ViewMode::choices(self.settings.playlist_view, self.has_midi());
            let i = views.iter().position(|&m| m == self.settings.view).unwrap_or(0);
            self.settings.view = views[(i + 1) % views.len()];
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
                self.seek_and_show(self.pos + self.settings.seek_step * step);
            }
            if shift(k) {
                self.seek_and_show(self.pos + self.settings.seek_step_large * step);
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
            if ui.button("Open…").on_hover_text("Open an audio file or a playlist (Ctrl+O)").clicked() {
                self.open_dialog(ui.ctx());
            }
            match (&self.loading, &self.loaded) {
                (Some(loading), _) => {
                    ui.label(format!("Loading {}…", loading.name));
                }
                (None, Some(loaded)) => {
                    ui.label(RichText::new(&loaded.track.name).strong());
                    ui.weak(format!("{} Hz", loaded.track.sample_rate));
                    if loaded.basic_synth {
                        ui.colored_label(ui.visuals().warn_fg_color, "basic tones").on_hover_text(
                            "No SoundFont was found, so this MIDI file is played with a plain built-in tone.\n\
                             Choose a SoundFont (.sf2) in Preferences for real instrument sounds.",
                        );
                    }
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
                if ui.button("Preferences").on_hover_text("Preferences (Ctrl+,)").clicked() {
                    self.show_preferences = !self.show_preferences;
                }
                for &mode in ViewMode::choices(self.settings.playlist_view, self.has_midi()).iter().rev() {
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
        // With the playlist as a view of its own, the sidebar is all bookmarks.
        let tab = if self.settings.playlist_view { SidebarTab::Bookmarks } else { self.settings.sidebar };
        ui.horizontal(|ui| {
            if self.settings.playlist_view {
                ui.label(RichText::new("Bookmarks").size(15.0).strong());
                return;
            }
            ui.selectable_value(&mut self.settings.sidebar, SidebarTab::Bookmarks, RichText::new("Bookmarks").size(15.0));
            ui.selectable_value(&mut self.settings.sidebar, SidebarTab::Playlist, RichText::new("Playlist").size(15.0));
        });
        ui.separator();
        // Display settings live at the bottom, folded away until wanted. The
        // list above gets whatever height they left free on the last frame.
        let total = ui.available_height();
        let list_height = (total - self.settings_height - 14.0).max(80.0);
        match tab {
            SidebarTab::Bookmarks => self.bookmark_list(ui, list_height),
            SidebarTab::Playlist => self.playlist(ui, list_height),
        }
        ui.separator();
        let settings = egui::ScrollArea::vertical()
            .id_salt("display-settings")
            .max_height(ui.available_height())
            .show(ui, |ui| self.display_settings(ui));
        let height = settings.content_size.y.min(total - 94.0);
        if height != self.settings_height {
            self.settings_height = height;
            ui.ctx().request_repaint();
        }
    }

    fn playlist_controls(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.confirm_delete { "Really delete?" } else { "Delete" };
                let delete = ui.button(label).on_hover_text("Delete this playlist (the audio files are untouched)");
                if delete.clicked() {
                    if self.confirm_delete || self.library.playlist().tracks.is_empty() {
                        self.library.remove_playlist();
                        self.playlist_sort = None;
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
                    self.playlist_sort = None;
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
                    self.playlist_sort = None;
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
            if ui.button("+ Files…").on_hover_text("Add audio files or playlists (or drop several on the window)").clicked() {
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let enabled = !self.library.playlist().tracks.is_empty();
                if ui
                    .add_enabled(enabled, egui::Button::new("Export…"))
                    .on_hover_text("Save this playlist as M3U, PLS or XSPF")
                    .clicked()
                {
                    self.export_dialog(ui.ctx());
                }
            });
        });
    }

    /// The playlist as a compact list, for the sidebar.
    fn playlist(&mut self, ui: &mut Ui, height: f32) {
        self.playlist_controls(ui);
        ui.add_space(2.0);
        let mut action = None;
        let tracks = &self.library.playlist().tracks;
        let current = self.library.playlist().position(self.current_path.as_deref());
        let list_height = (height - 82.0).max(60.0);
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
                            action = Some(PlaylistAction::Remove(i));
                        }
                        if ui.add_enabled(i + 1 < tracks.len(), egui::Button::new(RichText::new("↓").monospace()).small()).clicked() {
                            action = Some(PlaylistAction::Move(i, i + 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new(RichText::new("↑").monospace()).small()).clicked() {
                            action = Some(PlaylistAction::Move(i, i - 1));
                        }
                        let name = path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy();
                        let mut text = RichText::new(name);
                        if playing {
                            text = text.color(ACCENT);
                        }
                        let row = egui::Button::new(text).frame(false).truncate();
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if ui.add(row).on_hover_text(path.to_string_lossy()).clicked() {
                                action = Some(PlaylistAction::Open(path.clone()));
                            }
                        });
                    });
                });
            }
        });
        self.apply_playlist_action(action, ui.ctx());
    }

    fn apply_playlist_action(&mut self, action: Option<PlaylistAction>, ctx: &egui::Context) {
        let Some(action) = action else { return };
        let was_sorted = self.playlist_sort.take();
        match action {
            PlaylistAction::Open(path) => {
                self.playlist_sort = was_sorted;
                self.autoplay = self.playing;
                self.open(&path, ctx);
                return;
            }
            PlaylistAction::Remove(i) => drop(self.library.playlist_mut().tracks.remove(i)),
            PlaylistAction::Move(from, to) => self.library.playlist_mut().tracks.swap(from, to),
            PlaylistAction::Sort(column) => {
                // A second click on the same column reverses it.
                let ascending = was_sorted != Some((column, true));
                let tracks = std::mem::take(&mut self.library.playlist_mut().tracks);
                let mut entries: Vec<_> = tracks.into_iter().map(|p| (self.meta.get(&p).cloned(), p)).map(|(m, p)| (p, m)).collect();
                metadata::sort(&mut entries, column, ascending);
                self.library.playlist_mut().tracks = entries.into_iter().map(|e| e.0).collect();
                self.playlist_sort = Some((column, ascending));
            }
        }
        self.library_dirty = true;
    }

    /// The playlist as a full view: a table with sortable metadata columns.
    fn playlist_table(&mut self, ui: &mut Ui) {
        use egui_extras::{Column, TableBuilder};
        self.meta.poll();
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.scope(|ui| {
                ui.set_max_width(460.0);
                self.playlist_controls(ui);
            });
            ui.separator();

            let tracks = self.library.playlist().tracks.clone();
            let metas: Vec<Option<Meta>> = tracks.iter().map(|p| self.meta.get(p).cloned()).collect();
            let current = self.library.playlist().position(self.current_path.as_deref());
            let sorted = self.playlist_sort;
            let mut action = None;
            if tracks.is_empty() {
                ui.weak("This playlist is empty. Add files, or drop several on the window.");
                return;
            }
            let text_column = |width: f32| Column::initial(width).at_least(60.0).clip(true);
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(34.0))
                .column(Column::remainder().at_least(120.0).clip(true))
                .column(text_column(170.0))
                .column(text_column(170.0))
                .column(Column::exact(58.0))
                .column(Column::exact(64.0))
                .column(Column::exact(58.0))
                .column(Column::exact(88.0))
                .header(24.0, |mut header| {
                    header.col(|ui| drop(ui.strong("#")));
                    for (label, column) in [
                        ("Title", SortColumn::Title),
                        ("Artist", SortColumn::Artist),
                        ("Album", SortColumn::Album),
                        ("Track", SortColumn::TrackNumber),
                        ("Length", SortColumn::Length),
                        ("Type", SortColumn::Kind),
                    ] {
                        let mut clicked = false;
                        let (_, cell) = header.col(|ui| {
                            let button = egui::Button::new(RichText::new(label).strong()).frame(false);
                            let response = ui.add(button).on_hover_text("Sort the playlist by this column");
                            clicked = response.clicked();
                            if let Some((_, ascending)) = sorted.filter(|s| s.0 == column) {
                                // A small arrow: up for ascending.
                                let c = response.rect.right_center() + vec2(9.0, 0.0);
                                let d = if ascending { -3.5 } else { 3.5 };
                                ui.painter().add(Shape::convex_polygon(
                                    vec![c + vec2(-4.0, -d), c + vec2(4.0, -d), c + vec2(0.0, d)],
                                    ACCENT,
                                    Stroke::NONE,
                                ));
                            }
                        });
                        // The whole header cell is the target, not just its text.
                        if clicked || cell.clicked() {
                            action = Some(PlaylistAction::Sort(column));
                        }
                    }
                    header.col(|_| {});
                })
                .body(|body| {
                    body.rows(24.0, tracks.len(), |mut row| {
                        let i = row.index();
                        let (path, meta) = (&tracks[i], metas[i].as_ref());
                        let playing = current == Some(i);
                        row.set_selected(playing);
                        let unreadable = meta.is_some_and(|m| m.unreadable);
                        let cell = |ui: &mut Ui, text: String| {
                            let mut text = RichText::new(text);
                            if unreadable {
                                text = text.weak();
                            }
                            ui.add(egui::Label::new(text).truncate().selectable(false));
                        };
                        let optional = |value: Option<&String>| value.cloned().unwrap_or_default();
                        row.col(|ui| {
                            let colour = if playing { ACCENT } else { ui.visuals().weak_text_color() };
                            ui.label(RichText::new(format!("{}", i + 1)).color(colour).monospace());
                        });
                        row.col(|ui| cell(ui, metadata::display_title(path, meta)));
                        row.col(|ui| cell(ui, optional(meta.and_then(|m| m.artist.as_ref()))));
                        row.col(|ui| cell(ui, optional(meta.and_then(|m| m.album.as_ref()))));
                        row.col(|ui| cell(ui, meta.and_then(|m| m.track_number).map(|n| n.to_string()).unwrap_or_default()));
                        row.col(|ui| {
                            let length = match meta {
                                None => "…".to_owned(),
                                Some(m) if m.unreadable => "missing".to_owned(),
                                Some(m) => m.duration.map(|d| format_time(d, 0)).unwrap_or_default(),
                            };
                            cell(ui, length);
                        });
                        row.col(|ui| cell(ui, metadata::kind(path)));
                        row.col(|ui| {
                            let arrow = |glyph: &str| egui::Button::new(RichText::new(glyph).monospace()).small();
                            if ui.add_enabled(i > 0, arrow("↑")).on_hover_text("Move up").clicked() {
                                action = Some(PlaylistAction::Move(i, i - 1));
                            }
                            if ui.add_enabled(i + 1 < tracks.len(), arrow("↓")).on_hover_text("Move down").clicked() {
                                action = Some(PlaylistAction::Move(i, i + 1));
                            }
                            if ui.small_button("×").on_hover_text("Remove from the playlist").clicked() {
                                action = Some(PlaylistAction::Remove(i));
                            }
                        });
                        if row.response().on_hover_text(path.to_string_lossy()).clicked() && action.is_none() {
                            action = Some(PlaylistAction::Open(path.clone()));
                        }
                    });
                });
            self.apply_playlist_action(action, ui.ctx());
        });
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

    fn preferences_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_preferences;
        egui::Window::new("Preferences").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let section = |ui: &mut Ui, title: &str| {
                ui.add_space(6.0);
                ui.label(RichText::new(title).strong());
            };
            let grid = |ui: &mut Ui, id: &str, contents: &mut dyn FnMut(&mut Ui)| {
                egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).min_col_width(150.0).show(ui, |ui| contents(ui));
            };
            let settings = &mut self.settings;

            ui.label(RichText::new("Playback").strong());
            let mut keep_pitch = settings.keep_pitch;
            grid(ui, "playback", &mut |ui| {
                ui.label("Speed changes");
                ui.checkbox(&mut keep_pitch, "Keep pitch (time-stretch)")
                    .on_hover_text("Off: plain resampling like a tape machine, so pitch follows speed");
                ui.end_row();
                ui.label("When a track ends");
                ui.checkbox(&mut settings.auto_advance, "Play the next one in the playlist")
                    .on_hover_text("Unless a loop is on");
                ui.end_row();
                ui.label("Gap between loop repeats");
                ui.add(egui::Slider::new(&mut settings.loop_gap, 0.0..=4.0).step_by(0.25).suffix(" s"));
                ui.end_row();
                ui.label("Arrow keys seek");
                ui.add(egui::Slider::new(&mut settings.seek_step, 0.5..=10.0).step_by(0.5).suffix(" s"));
                ui.end_row();
                ui.label("Shift + arrow keys seek");
                ui.add(egui::Slider::new(&mut settings.seek_step_large, 5.0..=60.0).step_by(5.0).suffix(" s"));
                ui.end_row();
            });

            section(ui, "Display");
            grid(ui, "display", &mut |ui| {
                ui.label("Follow the playhead");
                egui::ComboBox::from_id_salt("follow").width(190.0).selected_text(settings.follow.label()).show_ui(
                    ui,
                    |ui| {
                        for follow in Follow::ALL {
                            ui.selectable_value(&mut settings.follow, follow, follow.label());
                        }
                    },
                );
                ui.end_row();
                ui.label("Playlist");
                let hint = "On: the playlist is a view like the waveform and spectrogram (V cycles to it, F makes \
                            it full screen), shown as a table with title, artist, album and length columns that \
                            sort the playlist when clicked.\nOff: a compact list in the sidebar";
                if ui.checkbox(&mut settings.playlist_view, "Show as a full view with metadata columns").on_hover_text(hint).changed() {
                    // Show the result of the change straight away.
                    if settings.playlist_view {
                        settings.view = ViewMode::Playlist;
                    } else {
                        settings.sidebar = SidebarTab::Playlist;
                    }
                }
                ui.end_row();
                ui.label("Interface size");
                egui::ComboBox::from_id_salt("scale")
                    .width(190.0)
                    .selected_text(format!("{:.0}%", settings.ui_scale * 100.0))
                    .show_ui(ui, |ui| {
                        for scale in [0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0] {
                            ui.selectable_value(&mut settings.ui_scale, scale, format!("{:.0}%", scale * 100.0));
                        }
                    });
                ui.end_row();
            });

            section(ui, "Sound");
            let font_label = match (&settings.soundfont, midi::system_soundfont()) {
                _ if self.soundfont_loading.is_some() => "Loading…".to_owned(),
                (Some(path), _) => path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                (None, Some(path)) => format!("{} (system)", path.file_name().unwrap_or_default().to_string_lossy()),
                (None, None) => "None found".to_owned(),
            };
            let has_font = self.soundfont.is_some();
            let (mut choose_font, mut clear_font) = (false, false);
            let key_before = (settings.key_sound, settings.key_program);
            grid(ui, "sound", &mut |ui| {
                ui.label("SoundFont").on_hover_text("The instrument sounds used to play MIDI files, and optionally the piano");
                ui.horizontal(|ui| {
                    ui.label(&font_label);
                    choose_font |= ui.button("Choose…").on_hover_text("Pick a SoundFont (.sf2) file").clicked();
                    if settings.soundfont.is_some() {
                        clear_font |= ui.button("Use default").on_hover_text("Go back to the system's SoundFont, if any").clicked();
                    }
                });
                ui.end_row();
                ui.label("Piano keys play");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut settings.key_sound, KeySound::Sine, "Sine wave");
                    ui.add_enabled_ui(has_font, |ui| {
                        ui.selectable_value(&mut settings.key_sound, KeySound::SoundFont, "SoundFont instrument")
                            .on_disabled_hover_text("Needs a SoundFont");
                    });
                });
                ui.end_row();
                ui.label("Piano instrument");
                ui.add_enabled_ui(has_font && settings.key_sound == KeySound::SoundFont, |ui| {
                    egui::ComboBox::from_id_salt("key-program")
                        .width(190.0)
                        .selected_text(midi::GM_NAMES[settings.key_program as usize & 127])
                        .show_ui(ui, |ui| {
                            for (program, name) in midi::GM_NAMES.iter().enumerate() {
                                ui.selectable_value(&mut settings.key_program, program as u8, *name);
                            }
                        });
                });
                ui.end_row();
            });
            if let Some(error) = &self.soundfont_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let key_changed = key_before != (settings.key_sound, settings.key_program);

            ui.add_space(8.0);
            ui.weak("Spectrogram and waveform settings are at the bottom of the sidebar.");
            ui.add_space(10.0);
            ui.separator();
            if ui.button("Reset to defaults").clicked() {
                let defaults = Settings::default();
                // Not preferences as such: where you are, not how you like it.
                let keep = (settings.last_dir.take(), settings.sidebar, settings.view, settings.volume, settings.loop_mode);
                *settings = Settings { last_dir: keep.0, sidebar: keep.1, view: keep.2, volume: keep.3, loop_mode: keep.4, ..defaults };
                keep_pitch = settings.keep_pitch;
            }
            if keep_pitch != self.settings.keep_pitch {
                self.set_keep_pitch(keep_pitch);
            }
            if choose_font {
                self.soundfont_dialog(ui.ctx());
            }
            if clear_font {
                self.settings.soundfont = None;
                self.load_soundfont(ui.ctx());
            } else if key_changed {
                self.apply_key_sound();
            }
        });
        self.show_preferences = open;
    }

    fn display_settings(&mut self, ui: &mut Ui) {
        let settings = &mut self.settings;
        let mut show_piano_range = false;
        let slider_width = (ui.available_width() - 150.0).max(60.0);
        ui.spacing_mut().slider_width = slider_width;
        egui::CollapsingHeader::new(RichText::new("Spectrogram").strong()).id_salt("spectrogram-settings").show(ui, |ui| {
            egui::ComboBox::from_id_salt("theme").width(ui.available_width() - 8.0).selected_text(settings.theme.label()).show_ui(
                ui,
                |ui| {
                    for theme in Theme::ALL {
                        ui.horizontal(|ui| {
                            theme_swatch(ui, theme, 48.0);
                            ui.selectable_value(&mut settings.theme, theme, theme.label());
                        });
                    }
                },
            );
            theme_swatch(ui, settings.theme, ui.available_width() - 8.0);
            ui.add(egui::Slider::new(&mut settings.range_db, 30.0..=100.0).step_by(1.0).suffix(" dB").text("range"))
                .on_hover_text("Dynamic range shown: lower hides quiet detail");
            ui.add(egui::Slider::new(&mut settings.brightness_db, -20.0..=30.0).step_by(1.0).suffix(" dB").text("brightness"))
                .on_hover_text("Raise to bring out quiet notes, lower if loud ones wash out");
            ui.checkbox(&mut settings.smooth, "Smooth").on_hover_text("Blend between cells instead of showing them as blocks");
            ui.checkbox(&mut settings.octave_lines, "Octave lines").on_hover_text("A line at every C");
            ui.checkbox(&mut settings.semitone_lines, "Semitone lines")
                .on_hover_text("A faint line between every pair of notes, when zoomed in far enough");
            ui.checkbox(&mut settings.harmonics, "Harmonics of the hovered pitch")
                .on_hover_text("Marks where overtones 2 to 8 of the pitch under the cursor would lie,\nto tell a played note from an overtone of a lower one");
            ui.separator();
            ui.checkbox(&mut settings.piano_mode, "Piano mode").on_hover_text(
                "Piano along the bottom, low notes on the left, with time running vertically above it.\n\
                 With playhead following on, the playhead stays on the piano's edge",
            );
            ui.add_enabled(settings.piano_mode, egui::Checkbox::new(&mut settings.reverse, "Reverse direction")).on_hover_text(
                "Off: what is coming falls from the top towards the piano.\n\
                 On: what has been played rises from the piano",
            );
            ui.add(egui::Slider::new(&mut settings.tone_volume, 0.0..=1.0).show_value(false).text("piano tone volume"))
                .on_hover_text("Click or drag on the piano to hear a note");
            show_piano_range = ui.button("Show all 88 keys").clicked();
        });
        egui::CollapsingHeader::new(RichText::new("Waveform").strong()).id_salt("waveform-settings").show(ui, |ui| {
            ui.add(egui::Slider::new(&mut settings.wave_zoom, 1.0..=32.0).logarithmic(true).suffix("×").text("zoom"))
                .on_hover_text("Magnify quiet passages; loud ones clip at the edge");
            ui.checkbox(&mut settings.wave_log, "Decibel scale")
                .on_hover_text("Shows level over a 60 dB range, so quiet detail is visible next to loud");
            ui.checkbox(&mut settings.wave_centre_line, "Centre line");
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut settings.wave_colour);
                ui.label("Colour");
                if ui.small_button("Reset").clicked() {
                    settings.wave_colour = Settings::default().wave_colour;
                }
            });
            if settings.view == ViewMode::Both {
                ui.weak("Drag the divider between waveform and spectrogram to resize them.");
            }
        });
        if show_piano_range {
            self.view.pitch_lo = PITCH_MIN;
            self.view.pitch_hi = 108.5;
        }
        ui.add_space(2.0);
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
            ui.checkbox(&mut self.settings.return_on_pause, "Return")
                .on_hover_text("When pausing, put the playhead back where playback started");
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
            ("← / →", "Seek back / forward (Shift: further)"),
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
            ("V", "Cycle the views"),
            ("F", "Full-screen display"),
            ("+ / −", "Zoom in / out"),
            ("Z", "Zoom out to the whole track"),
            ("Ctrl+O", "Open a file or playlist"),
            ("Ctrl+,", "Preferences"),
        ];
        const MOUSE: &[(&str, &str)] = &[
            ("Click or drag", "Move the playhead"),
            ("Ctrl+click", "Place loop points: start, then end; after that, move the nearer one"),
            ("Ctrl+drag", "Select a loop region (drag its edges to adjust)"),
            ("Click the piano", "Play that note"),
            ("Drag the divider", "Resize waveform against spectrogram"),
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
        let gone = match self.settings.view {
            ViewMode::Playlist => !self.settings.playlist_view,
            // The MIDI view only exists while a MIDI file is open.
            ViewMode::Midi => !self.has_midi() && self.loading.is_none(),
            _ => false,
        };
        if gone {
            self.settings.view = ViewMode::default();
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
            if self.settings.view == ViewMode::Playlist {
                self.playlist_table(ui);
            } else if self.loaded.is_some() && self.loading.is_none() {
                self.display(ui);
            } else {
                self.empty_state(ui);
            }
        });
        if self.show_help {
            self.help_window(&ctx);
        }
        if self.show_preferences {
            self.preferences_window(&ctx);
        }
        if ctx.zoom_factor() != self.settings.ui_scale {
            ctx.set_zoom_factor(self.settings.ui_scale);
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

/// A strip showing a spectrogram theme from quiet to loud.
fn theme_swatch(ui: &mut Ui, theme: Theme, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
    let steps = 48;
    for i in 0..steps {
        let [r, g, b] = theme.colour(i as f32 / (steps - 1) as f32);
        let x = |k: usize| rect.left() + rect.width() * k as f32 / steps as f32;
        let cell = Rect::from_x_y_ranges(x(i)..=x(i + 1) + 0.5, rect.y_range());
        ui.painter().rect_filled(cell, CornerRadius::ZERO, Color32::from_rgb(r, g, b));
    }
    ui.painter().rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_white_alpha(60)), egui::StrokeKind::Outside);
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
