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
        let name = (1..)
            .map(|n| format!("Playlist {n}"))
            .find(|name| self.playlists.iter().all(|p| &p.name != name))
            .unwrap();
        self.playlists.push(Playlist { name, tracks: Vec::new() });
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
        library.remove_playlist();
        library.remove_playlist();
        assert_eq!((library.playlists.len(), library.current), (1, 0));
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
