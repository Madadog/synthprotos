//! Tags and duration of audio files, read in the background for the
//! playlist table.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u64>,
    /// Seconds.
    pub duration: Option<f64>,
    /// The file could not be opened or is not audio.
    pub unreadable: bool,
}

/// Read a file's tags and duration without decoding it.
pub fn read(path: &Path) -> Meta {
    read_tags(path).unwrap_or(Meta { unreadable: true, ..Meta::default() })
}

fn read_tags(path: &Path) -> Option<Meta> {
    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .ok()?;
    let mut meta = Meta::default();
    let track = format.default_track(TrackType::Audio)?;
    let track_id = track.id;
    let rate = track.codec_params.as_ref().and_then(|p| p.audio()).and_then(|a| a.sample_rate);
    if let (Some(frames), Some(rate)) = (track.num_frames, rate) {
        meta.duration = Some(frames as f64 / rate as f64);
    }
    let mut metadata = format.metadata();
    if let Some(revision) = metadata.skip_to_latest() {
        let per_track = revision.per_track.iter().filter(|t| t.track_id == u64::from(track_id)).map(|t| &t.metadata);
        for tag in std::iter::once(&revision.media).chain(per_track).flat_map(|c| &c.tags) {
            let text = |s: &std::sync::Arc<String>| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
            match &tag.std {
                Some(StandardTag::TrackTitle(s)) => meta.title = meta.title.or(text(s)),
                Some(StandardTag::Artist(s)) => meta.artist = meta.artist.or(text(s)),
                Some(StandardTag::Album(s)) => meta.album = meta.album.or(text(s)),
                Some(StandardTag::TrackNumber(n)) => meta.track_number = meta.track_number.or(Some(*n)),
                _ => {}
            }
        }
    }
    Some(meta)
}

/// Metadata for the files the playlist view has asked about. Lookups return
/// at once; files not yet known are read on a worker thread.
pub struct MetaCache {
    known: HashMap<PathBuf, Option<Meta>>,
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, Meta)>,
}

impl MetaCache {
    /// `wake` is called when new metadata is ready.
    pub fn new(wake: impl Fn() + Send + 'static) -> MetaCache {
        let (requests, request_rx) = channel::<PathBuf>();
        let (result_tx, results) = channel();
        std::thread::spawn(move || {
            for path in request_rx {
                let meta = read(&path);
                if result_tx.send((path, meta)).is_err() {
                    break;
                }
                wake();
            }
        });
        MetaCache { known: HashMap::new(), requests, results }
    }

    /// Take in whatever the worker has finished.
    pub fn poll(&mut self) {
        for (path, meta) in self.results.try_iter() {
            self.known.insert(path, Some(meta));
        }
    }

    /// The file's metadata, if it has been read yet.
    pub fn get(&mut self, path: &Path) -> Option<&Meta> {
        if !self.known.contains_key(path) {
            self.known.insert(path.to_path_buf(), None);
            let _ = self.requests.send(path.to_path_buf());
        }
        self.known.get(path)?.as_ref()
    }
}

/// Columns of the playlist table that can be sorted by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortColumn {
    Title,
    Artist,
    Album,
    TrackNumber,
    Length,
    Kind,
}

/// The title to show: the tag, or failing that the file name.
pub fn display_title(path: &Path, meta: Option<&Meta>) -> String {
    meta.and_then(|m| m.title.clone())
        .unwrap_or_else(|| path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy().into_owned())
}

pub fn kind(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
}

/// Sort tracks by a column. Text compares case-insensitively; tracks missing
/// the value go last whichever the direction; ties keep their order.
pub fn sort(tracks: &mut [(PathBuf, Option<Meta>)], column: SortColumn, ascending: bool) {
    #[derive(PartialEq, PartialOrd)]
    enum Key {
        Text(String),
        Number(f64),
    }
    let key = |(path, meta): &(PathBuf, Option<Meta>)| -> Option<Key> {
        let meta = meta.as_ref();
        let text = |s: Option<&String>| s.map(|s| Key::Text(s.to_lowercase()));
        match column {
            SortColumn::Title => Some(Key::Text(display_title(path, meta).to_lowercase())),
            SortColumn::Artist => text(meta?.artist.as_ref()),
            SortColumn::Album => text(meta?.album.as_ref()),
            SortColumn::TrackNumber => meta?.track_number.map(|n| Key::Number(n as f64)),
            SortColumn::Length => meta?.duration.map(Key::Number),
            SortColumn::Kind => Some(Key::Text(kind(path))),
        }
    };
    tracks.sort_by(|a, b| match (key(a), key(b)) {
        (Some(x), Some(y)) => {
            let order = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
            if ascending { order } else { order.reverse() }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, artist: Option<&str>, number: Option<u64>, duration: Option<f64>) -> (PathBuf, Option<Meta>) {
        let meta = Meta { artist: artist.map(str::to_owned), track_number: number, duration, ..Meta::default() };
        (PathBuf::from(file), Some(meta))
    }

    fn names(tracks: &[(PathBuf, Option<Meta>)]) -> Vec<&str> {
        tracks.iter().map(|t| t.0.to_str().unwrap()).collect()
    }

    #[test]
    fn sorting() {
        let mut tracks = vec![
            entry("b.mp3", Some("zappa"), Some(2), Some(200.0)),
            entry("D.flac", None, None, None),
            entry("a.ogg", Some("Abba"), Some(10), Some(90.5)),
            (PathBuf::from("c.wav"), None), // not read yet
        ];
        sort(&mut tracks, SortColumn::Title, true);
        assert_eq!(names(&tracks), ["a.ogg", "b.mp3", "c.wav", "D.flac"]);
        sort(&mut tracks, SortColumn::Title, false);
        assert_eq!(names(&tracks), ["D.flac", "c.wav", "b.mp3", "a.ogg"]);
        // Missing values stay at the end in both directions, in their existing order.
        sort(&mut tracks, SortColumn::Artist, true);
        assert_eq!(names(&tracks), ["a.ogg", "b.mp3", "D.flac", "c.wav"]);
        sort(&mut tracks, SortColumn::Artist, false);
        assert_eq!(names(&tracks), ["b.mp3", "a.ogg", "D.flac", "c.wav"]);
        // Numbers sort numerically, not as text.
        sort(&mut tracks, SortColumn::TrackNumber, true);
        assert_eq!(names(&tracks), ["b.mp3", "a.ogg", "D.flac", "c.wav"]);
        sort(&mut tracks, SortColumn::Length, true);
        assert_eq!(names(&tracks), ["a.ogg", "b.mp3", "D.flac", "c.wav"]);
        sort(&mut tracks, SortColumn::Kind, true);
        assert_eq!(names(&tracks), ["D.flac", "b.mp3", "a.ogg", "c.wav"]);
    }

    #[test]
    fn title_falls_back_to_file_name() {
        let tagged = Meta { title: Some("Blue in Green".into()), ..Meta::default() };
        assert_eq!(display_title(Path::new("/m/03 take.mp3"), Some(&tagged)), "Blue in Green");
        assert_eq!(display_title(Path::new("/m/03 take.mp3"), None), "03 take");
        assert!(read(Path::new("/nonexistent/file.mp3")).unreadable);
    }
}
