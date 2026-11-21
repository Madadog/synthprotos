//! Persistent state: global settings, and per-track bookmarks and loop selection.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    Waveform,
    #[default]
    Spectrogram,
    Both,
}

impl ViewMode {
    pub const ALL: [ViewMode; 3] = [ViewMode::Waveform, ViewMode::Spectrogram, ViewMode::Both];

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Waveform => "Waveform",
            ViewMode::Spectrogram => "Spectrogram",
            ViewMode::Both => "Both",
        }
    }
}

/// How the zoomed display keeps up with the playhead during playback.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Follow {
    Off,
    #[default]
    Page,
    Centred,
}

impl Follow {
    pub const ALL: [Follow; 3] = [Follow::Page, Follow::Centred, Follow::Off];

    pub fn label(self) -> &'static str {
        match self {
            Follow::Off => "Don't follow",
            Follow::Page => "Page by page",
            Follow::Centred => "Keep playhead centred",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LoopMode {
    #[default]
    Off,
    /// The dragged selection.
    Selection,
    /// The stretch between the bookmarks either side of the playhead.
    Section,
    Track,
}

impl LoopMode {
    pub const ALL: [LoopMode; 4] = [LoopMode::Off, LoopMode::Selection, LoopMode::Section, LoopMode::Track];

    pub fn label(self) -> &'static str {
        match self {
            LoopMode::Off => "Off",
            LoopMode::Selection => "Selection",
            LoopMode::Section => "Bookmark section",
            LoopMode::Track => "Whole track",
        }
    }
}

/// Colour scheme of the spectrogram, quietest to loudest.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Inferno,
    Magma,
    Viridis,
    Ice,
    Phosphor,
    Greyscale,
    Paper,
}

impl Theme {
    pub const ALL: [Theme; 7] =
        [Theme::Inferno, Theme::Magma, Theme::Viridis, Theme::Ice, Theme::Phosphor, Theme::Greyscale, Theme::Paper];

    pub fn label(self) -> &'static str {
        match self {
            Theme::Inferno => "Inferno",
            Theme::Magma => "Magma",
            Theme::Viridis => "Viridis",
            Theme::Ice => "Ice",
            Theme::Phosphor => "Phosphor",
            Theme::Greyscale => "Greyscale",
            Theme::Paper => "Paper (dark on white)",
        }
    }

    /// Evenly spaced colour stops from silence to the loudest level.
    pub fn stops(self) -> &'static [[u8; 3]] {
        match self {
            Theme::Inferno => &[
                [0, 0, 4], [31, 12, 72], [85, 15, 109], [136, 34, 106], [186, 54, 85], [227, 89, 51], [249, 140, 10],
                [249, 201, 50], [252, 255, 164],
            ],
            Theme::Magma => &[
                [0, 0, 4], [28, 16, 68], [79, 18, 123], [129, 37, 129], [181, 54, 122], [229, 80, 100],
                [251, 135, 97], [254, 194, 135], [252, 253, 191],
            ],
            Theme::Viridis => &[
                [68, 1, 84], [71, 44, 122], [59, 81, 139], [44, 113, 142], [33, 144, 141], [39, 173, 129],
                [92, 200, 99], [170, 220, 50], [253, 231, 37],
            ],
            Theme::Ice => &[
                [0, 0, 0], [8, 16, 60], [16, 48, 120], [24, 90, 170], [40, 140, 200], [90, 190, 220],
                [160, 225, 235], [215, 243, 247], [255, 255, 255],
            ],
            Theme::Phosphor => &[[0, 0, 0], [0, 48, 20], [0, 110, 48], [20, 180, 80], [90, 235, 130], [225, 255, 215]],
            Theme::Greyscale => &[[0, 0, 0], [255, 255, 255]],
            Theme::Paper => &[[255, 255, 255], [0, 0, 0]],
        }
    }

    /// Whether silence is light, so overlays need dark lines.
    pub fn is_light(self) -> bool {
        self == Theme::Paper
    }

    /// Colour at `t` in 0..=1.
    pub fn colour(self, t: f32) -> [u8; 3] {
        let stops = self.stops();
        let x = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
        let i = (x as usize).min(stops.len() - 2);
        let f = x - i as f32;
        std::array::from_fn(|k| (stops[i][k] as f32 + (stops[i + 1][k] as f32 - stops[i][k] as f32) * f).round() as u8)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SidebarTab {
    #[default]
    Bookmarks,
    Playlist,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub view: ViewMode,
    pub follow: Follow,
    /// Pausing puts the playhead back where playback started.
    pub return_on_pause: bool,
    pub loop_mode: LoopMode,
    /// Seconds of silence between loop repeats.
    pub loop_gap: f64,
    /// Dynamic range shown by the spectrogram, dB.
    pub range_db: f32,
    pub theme: Theme,
    /// Faint lines across the spectrogram at every C.
    pub octave_lines: bool,
    /// Seconds the arrow keys seek, without and with Shift.
    pub seek_step: f64,
    pub seek_step_large: f64,
    /// Size of the whole interface.
    pub ui_scale: f32,
    /// Speed changes time-stretch (keeping pitch) rather than resample.
    pub keep_pitch: bool,
    /// When a track ends, play the next one in the playlist.
    pub auto_advance: bool,
    pub sidebar: SidebarTab,
    pub last_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume: 0.8,
            view: ViewMode::default(),
            follow: Follow::default(),
            return_on_pause: false,
            loop_mode: LoopMode::default(),
            loop_gap: 0.0,
            range_db: 70.0,
            theme: Theme::default(),
            octave_lines: true,
            seek_step: 2.0,
            seek_step_large: 10.0,
            ui_scale: 1.0,
            keep_pitch: true,
            auto_advance: true,
            sidebar: SidebarTab::default(),
            last_dir: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    /// Seconds from the start of the track.
    pub time: f64,
    pub label: String,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Sorted by time.
    pub bookmarks: Vec<Bookmark>,
    pub next_id: u64,
    pub selection: Option<(f64, f64)>,
    pub speed: f64,
}

impl Default for Session {
    fn default() -> Self {
        Session { bookmarks: Vec::new(), next_id: 1, selection: None, speed: 1.0 }
    }
}

impl Session {
    /// Add a bookmark at `time` and return its id.
    pub fn add_bookmark(&mut self, time: f64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.bookmarks.push(Bookmark { id, time, label: format!("Mark {id}") });
        self.sort();
        id
    }

    pub fn sort(&mut self) {
        self.bookmarks.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    /// The bookmark to jump back to from `pos`. Like a CD player's "previous":
    /// a bookmark only just passed counts as the current one and is skipped.
    pub fn previous(&self, pos: f64, grace: f64) -> Option<&Bookmark> {
        self.bookmarks.iter().rev().find(|b| b.time < pos - grace)
    }

    pub fn next(&self, pos: f64) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| b.time > pos + 1e-3)
    }

    /// The bookmark-delimited section containing `pos`.
    pub fn section(&self, pos: f64, duration: f64) -> (f64, f64) {
        let start = self.bookmarks.iter().rev().find(|b| b.time <= pos).map_or(0.0, |b| b.time);
        let end = self.bookmarks.iter().find(|b| b.time > pos).map_or(duration, |b| b.time);
        (start, end)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    pub tracks: Vec<PathBuf>,
}

impl Playlist {
    /// Append the files that are not already in the playlist.
    pub fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if !self.tracks.contains(&path) {
                self.tracks.push(path);
            }
        }
    }

    pub fn position(&self, path: Option<&Path>) -> Option<usize> {
        self.tracks.iter().position(|t| Some(t.as_path()) == path)
    }

    /// The track `step` places after `current`. From a track that is not in
    /// the playlist, "next" is the first track.
    pub fn neighbour(&self, current: Option<&Path>, step: isize) -> Option<&PathBuf> {
        match self.position(current) {
            Some(i) => self.tracks.get(i.checked_add_signed(step)?),
            None if step > 0 => self.tracks.first(),
            None => None,
        }
    }
}

/// All playlists, and which one is showing.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub playlists: Vec<Playlist>,
    pub current: usize,
}

impl Default for Library {
    fn default() -> Self {
        Library { playlists: vec![Playlist { name: "Playlist 1".into(), tracks: Vec::new() }], current: 0 }
    }
}

impl Library {
    pub fn load() -> Library {
        let mut library: Library = load(data_dir().map(|d| d.join("playlists.json")));
        if library.playlists.is_empty() {
            library = Library::default();
        }
        library.current = library.current.min(library.playlists.len() - 1);
        library
    }

    pub fn save(&self) {
        save(data_dir().map(|d| d.join("playlists.json")), self);
    }

    pub fn playlist(&self) -> &Playlist {
        &self.playlists[self.current]
    }

    pub fn playlist_mut(&mut self) -> &mut Playlist {
        &mut self.playlists[self.current]
    }

    /// Add an empty playlist with an unused name and show it.
    pub fn add_playlist(&mut self) {
        self.import("Playlist", Vec::new());
    }

    /// Add a playlist, numbering the name if it is taken, and show it.
    pub fn import(&mut self, name: &str, tracks: Vec<PathBuf>) {
        let taken = |name: &str| self.playlists.iter().any(|p| p.name == name);
        let numbered = name == "Playlist";
        let name = std::iter::once(name.to_owned())
            .filter(|_| !numbered)
            .chain((if numbered { 1 } else { 2 }..).map(|n| format!("{name} {n}")))
            .find(|candidate| !taken(candidate))
            .unwrap();
        self.playlists.push(Playlist { name, tracks });
        self.current = self.playlists.len() - 1;
    }

    /// Delete the showing playlist; there is always at least one.
    pub fn remove_playlist(&mut self) {
        self.playlists.remove(self.current);
        if self.playlists.is_empty() {
            *self = Library::default();
        }
        self.current = self.current.min(self.playlists.len() - 1);
    }
}

fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("woodshed"))
}

fn session_path(track: &Path) -> Option<PathBuf> {
    let canonical = track.canonicalize().unwrap_or_else(|_| track.to_path_buf());
    // FNV-1a, so file names stay stable across compiler versions.
    let hash = canonical
        .to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    Some(data_dir()?.join("tracks").join(format!("{hash:016x}.json")))
}

fn load<T: Default + for<'de> Deserialize<'de>>(path: Option<PathBuf>) -> T {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save<T: Serialize>(path: Option<PathBuf>, value: &T) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(value) {
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("cannot save {}: {e}", path.display());
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        load(data_dir().map(|d| d.join("settings.json")))
    }

    pub fn save(&self) {
        save(data_dir().map(|d| d.join("settings.json")), self);
    }
}

impl Session {
    pub fn load(track: &Path) -> Session {
        let mut session: Session = load(session_path(track));
        session.sort();
        session
    }

    pub fn save(&self, track: &Path) {
        save(session_path(track), self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_navigation() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let mut list = Playlist { name: "Set".into(), tracks: Vec::new() };
        list.add(paths(&["a.mp3", "b.mp3"]));
        list.add(paths(&["b.mp3", "c.mp3"]));
        assert_eq!(list.tracks, paths(&["a.mp3", "b.mp3", "c.mp3"]));

        let at = |name: &str, step| list.neighbour(Some(Path::new(name)), step).map(|p| p.to_str().unwrap());
        assert_eq!(at("a.mp3", 1), Some("b.mp3"));
        assert_eq!(at("c.mp3", 1), None);
        assert_eq!(at("b.mp3", -1), Some("a.mp3"));
        assert_eq!(at("a.mp3", -1), None);
        // From a track outside the playlist, "next" starts the playlist.
        assert_eq!(at("x.mp3", 1), Some("a.mp3"));
        assert_eq!(at("x.mp3", -1), None);
        assert_eq!(list.neighbour(None, 1), Some(&PathBuf::from("a.mp3")));

        let mut library = Library::default();
        library.add_playlist();
        assert_eq!((library.current, library.playlist().name.as_str()), (1, "Playlist 2"));
        library.import("Gig", vec![PathBuf::from("a.mp3")]);
        library.import("Gig", Vec::new());
        let names: Vec<&str> = library.playlists.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Playlist 1", "Playlist 2", "Gig", "Gig 2"]);
        library.remove_playlist();
        library.remove_playlist();
        library.remove_playlist();
        library.remove_playlist();
        assert_eq!((library.playlists.len(), library.current), (1, 0));
    }

    #[test]
    fn theme_colours() {
        for theme in Theme::ALL {
            let stops = theme.stops();
            assert_eq!(theme.colour(0.0), stops[0]);
            assert_eq!(theme.colour(1.0), *stops.last().unwrap());
            assert_eq!(theme.colour(7.0), *stops.last().unwrap());
        }
        assert_eq!(Theme::Greyscale.colour(0.5), [128, 128, 128]);
        assert!(Theme::Paper.is_light() && !Theme::Inferno.is_light());
    }

    #[test]
    fn bookmark_navigation() {
        let mut s = Session::default();
        for t in [30.0, 10.0, 20.0] {
            s.add_bookmark(t);
        }
        let times: Vec<f64> = s.bookmarks.iter().map(|b| b.time).collect();
        assert_eq!(times, [10.0, 20.0, 30.0]);

        assert_eq!(s.next(5.0).unwrap().time, 10.0);
        assert_eq!(s.next(10.0).unwrap().time, 20.0);
        assert!(s.next(30.0).is_none());

        // Just after a bookmark, "previous" goes to the one before it...
        assert_eq!(s.previous(20.5, 1.0).unwrap().time, 10.0);
        // ...but well into the section it restarts the section.
        assert_eq!(s.previous(25.0, 1.0).unwrap().time, 20.0);
        assert!(s.previous(10.2, 1.0).is_none());

        assert_eq!(s.section(5.0, 60.0), (0.0, 10.0));
        assert_eq!(s.section(20.0, 60.0), (20.0, 30.0));
        assert_eq!(s.section(45.0, 60.0), (30.0, 60.0));
    }
}
