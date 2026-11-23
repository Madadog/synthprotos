//! Reading and writing playlist files: M3U / M3U8, PLS and XSPF; reading
//! Windows Media playlists (ASX, WPL) and CUE sheets.
//!
//! Only local files are kept: entries that are web streams are skipped.
//! Relative entries are resolved against the playlist's own directory.

use std::path::{Path, PathBuf};

pub const EXTENSIONS: &[&str] = &["m3u8", "m3u", "pls", "xspf", "asx", "wax", "wvx", "wpl"];
pub const CUE_EXTENSION: &str = "cue";

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_playlist(path: &Path) -> bool {
    EXTENSIONS.contains(&extension(path).as_str())
}

/// The audio files a playlist file lists, as absolute paths.
pub fn read(path: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read playlist: {e}"))?;
    // M3U8 is UTF-8 by definition; plain M3U and PLS are often Latin-1.
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    let text = text.trim_start_matches('\u{feff}');
    let entries = match extension(path).as_str() {
        "pls" => parse_pls(text),
        "xspf" => parse_xspf(text),
        "asx" | "wax" | "wvx" => attribute_values(text, "ref", "href"),
        "wpl" => attribute_values(text, "media", "src"),
        _ => parse_m3u(text),
    };
    let base = path.parent().unwrap_or(Path::new(""));
    let tracks: Vec<PathBuf> = entries.iter().filter_map(|e| resolve(e, base)).collect();
    if tracks.is_empty() {
        return Err(format!("No local audio files listed in {}", path.display()));
    }
    Ok(tracks)
}

/// Write a playlist in the format given by the file extension.
pub fn write(path: &Path, tracks: &[PathBuf]) -> Result<(), String> {
    let base = path.parent().unwrap_or(Path::new(""));
    let text = match extension(path).as_str() {
        "pls" => format_pls(tracks),
        "xspf" => format_xspf(tracks),
        _ => format_m3u(tracks, base),
    };
    std::fs::write(path, text).map_err(|e| format!("Cannot write playlist: {e}"))
}

fn parse_m3u(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect()
}

fn parse_pls(text: &str) -> Vec<String> {
    // "File12=/path/to/song.mp3", in numerical order.
    let mut files: Vec<(u32, String)> = text
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            let key = key.trim();
            let number = key.get(..4).filter(|k| k.eq_ignore_ascii_case("file")).and(key.get(4..))?;
            Some((number.parse().ok()?, value.trim().to_owned()))
        })
        .collect();
    files.sort_by_key(|f| f.0);
    files.into_iter().map(|f| f.1).collect()
}

fn parse_xspf(text: &str) -> Vec<String> {
    // Each <track> has one <location>; no other element uses that name
    // inside the track list, so a scan for the tag is enough.
    let Some(start) = text.find("<trackList") else { return Vec::new() };
    let mut rest = &text[start..];
    let mut locations = Vec::new();
    while let Some(open) = rest.find("<location") {
        let after = &rest[open..];
        let (Some(begin), Some(end)) = (after.find('>'), after.find("</location>")) else { break };
        if begin < end {
            locations.push(xml_unescape(after[begin + 1..end].trim()));
        }
        rest = &after[end + "</location>".len()..];
    }
    locations
}

/// Values of `attribute` on every `<tag ...>` element, whatever the case
/// (ASX files are written every which way).
fn attribute_values(text: &str, tag: &str, attribute: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut values = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find(&format!("<{tag}")) {
        let start = from + found + tag.len() + 1;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        from = end;
        // A longer tag name that merely starts the same.
        if lower[start..].starts_with(|c: char| c.is_ascii_alphanumeric()) {
            continue;
        }
        let Some(at) = lower[start..end].find(&format!("{attribute}=")) else { continue };
        let value = &text[start + at + attribute.len() + 1..end];
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
        if let Some(close) = value[1..].find(quote) {
            values.push(xml_unescape(&value[1..1 + close]));
        }
    }
    values
}

/// One audio file named by a CUE sheet, and the tracks marked within it.
#[derive(Debug, PartialEq)]
pub struct CueFile {
    pub path: PathBuf,
    /// Start time in seconds and title of each track.
    pub marks: Vec<(f64, String)>,
}

pub fn is_cue(path: &Path) -> bool {
    extension(path) == CUE_EXTENSION
}

/// The audio files a CUE sheet describes, with their track start times.
pub fn read_cue(path: &Path) -> Result<Vec<CueFile>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read CUE sheet: {e}"))?;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    let files = parse_cue(text.trim_start_matches('\u{feff}'), path.parent().unwrap_or(Path::new("")));
    if files.is_empty() {
        return Err(format!("No audio file named in {}", path.display()));
    }
    Ok(files)
}

fn parse_cue(text: &str, base: &Path) -> Vec<CueFile> {
    // The text of a command's argument, without its quotes.
    let argument = |rest: &str| match rest.trim().strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or("").to_owned(),
        None => rest.trim().to_owned(),
    };
    let mut files: Vec<CueFile> = Vec::new();
    let mut track: Option<(u32, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                // FILE "name with spaces.flac" WAVE
                let name = match rest.trim().strip_prefix('"') {
                    Some(_) => argument(rest),
                    None => rest.trim().rsplit_once(char::is_whitespace).map_or(rest.trim(), |(name, _)| name).to_owned(),
                };
                if let Some(path) = resolve(&name, base) {
                    files.push(CueFile { path, marks: Vec::new() });
                }
                track = None;
            }
            "TRACK" => track = Some((rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0), None)),
            "TITLE" => {
                if let Some((_, title)) = &mut track {
                    *title = Some(argument(rest));
                }
            }
            "INDEX" => {
                // INDEX 01 mm:ss:ff, with 75 frames to the second. Index 00 is the pre-gap.
                let mut parts = rest.split_whitespace();
                let is_start = parts.next().and_then(|n| n.parse::<u32>().ok()) == Some(1);
                let fields: Vec<f64> = parts.next().unwrap_or("").split(':').filter_map(|f| f.parse().ok()).collect();
                if let (true, [minutes, seconds, frames], Some((number, title)), Some(file)) =
                    (is_start, fields.as_slice(), &track, files.last_mut())
                {
                    let label = title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| format!("Track {number}"));
                    file.marks.push((minutes * 60.0 + seconds + frames / 75.0, label));
                }
            }
            _ => {}
        }
    }
    files
}

/// Turn a playlist entry into an absolute local path; `None` for streams.
fn resolve(entry: &str, base: &Path) -> Option<PathBuf> {
    let lower = entry.to_ascii_lowercase();
    let path = if let Some(rest) = lower.strip_prefix("file://").map(|r| &entry[entry.len() - r.len()..]) {
        // file:///home/x, file://localhost/home/x, file:///C:/x
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let decoded = percent_decode(rest);
        let windows_drive = decoded.as_bytes().get(2) == Some(&b':') && decoded.starts_with('/');
        PathBuf::from(if windows_drive { &decoded[1..] } else { &decoded[..] })
    } else if lower.contains("://") {
        return None;
    } else if cfg!(windows) {
        PathBuf::from(entry)
    } else {
        // Playlists written on Windows use backslashes.
        PathBuf::from(entry.replace('\\', "/"))
    };
    Some(if path.is_absolute() { path } else { base.join(path) })
}

fn format_m3u(tracks: &[PathBuf], base: &Path) -> String {
    let mut out = String::from("#EXTM3U\n");
    for track in tracks {
        // Files beside or below the playlist are written relative to it, so
        // the folder can be moved as a whole.
        let shown = track.strip_prefix(base).unwrap_or(track);
        out += &format!("{}\n", shown.display());
    }
    out
}

fn format_pls(tracks: &[PathBuf]) -> String {
    let mut out = String::from("[playlist]\n");
    for (i, track) in tracks.iter().enumerate() {
        out += &format!("File{}={}\n", i + 1, track.display());
    }
    out + &format!("NumberOfEntries={}\nVersion=2\n", tracks.len())
}

fn format_xspf(tracks: &[PathBuf]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">\n  <trackList>\n",
    );
    for track in tracks {
        let path = track.to_string_lossy().replace('\\', "/");
        let slash = if path.starts_with('/') { "" } else { "/" };
        let uri = format!("file://{slash}{}", percent_encode(&path));
        out += &format!("    <track><location>{}</location></track>\n", xml_escape(&uri));
    }
    out + "  </trackList>\n</playlist>\n"
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match hex.filter(|_| bytes[i] == b'%').and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn resolved(entries: Vec<String>) -> Vec<String> {
        entries.iter().filter_map(|e| resolve(e, Path::new("/music/lists"))).map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn m3u() {
        let text = "#EXTM3U\r\n#EXTINF:123,Artist - Title\r\nsong one.mp3\r\n\r\n../other/two.flac\r\n\
                    /abs/three.ogg\r\nhttp://example.com/stream.mp3\r\nC:\\Music\\four.wav\r\nsub\\five.wav\r\n";
        assert_eq!(
            resolved(parse_m3u(text)),
            [
                "/music/lists/song one.mp3",
                "/music/lists/../other/two.flac",
                "/abs/three.ogg",
                "/music/lists/C:/Music/four.wav",
                "/music/lists/sub/five.wav"
            ]
        );
    }

    #[test]
    fn pls() {
        let text = "[playlist]\nNumberOfEntries=3\nFile2=b.mp3\nTitle2=B\nfile1=/x/a.mp3\nLength1=-1\n\
                    File10=file:///x/My%20Song.mp3\nFile3=https://example.com/live\n";
        assert_eq!(resolved(parse_pls(text)), ["/x/a.mp3", "/music/lists/b.mp3", "/x/My Song.mp3"]);
    }

    #[test]
    fn xspf() {
        let text = r#"<?xml version="1.0"?><playlist version="1" xmlns="http://xspf.org/ns/0/">
            <title>Set</title><location>ignored-outside-tracklist</location>
            <trackList>
              <track><title>One</title><location>file:///x/Rock%20%26%20Roll/caf%C3%A9.mp3</location></track>
              <track><location> rel/Tom &amp; Jerry.flac </location></track>
              <track><location>file://localhost/y/z.wav</location></track>
              <track><location>http://example.com/a.mp3</location></track>
            </trackList></playlist>"#;
        assert_eq!(
            resolved(parse_xspf(text)),
            ["/x/Rock & Roll/café.mp3", "/music/lists/rel/Tom & Jerry.flac", "/y/z.wav"]
        );
    }

    #[test]
    fn windows_media_playlists() {
        let asx = r#"<ASX Version="3.0"><Title>Set</Title>
            <Entry><REF HREF="one.mp3"/><Reference x="y"/></Entry>
            <entry><ref href='/abs/Tom &amp; Jerry.wma' /></entry>
            <Entry><Ref HREF="mms://example.com/stream"/></Entry></ASX>"#;
        assert_eq!(resolved(attribute_values(asx, "ref", "href")), ["/music/lists/one.mp3", "/abs/Tom & Jerry.wma"]);
        let wpl = r#"<?wpl version="1.0"?><smil><head><meta name="Generator" content="x"/></head><body><seq>
            <media src="..\Albums\a.wma" tid="{1}"/><media src="b.mp3"/></seq></body></smil>"#;
        assert_eq!(resolved(attribute_values(wpl, "media", "src")), ["/music/lists/../Albums/a.wma", "/music/lists/b.mp3"]);
    }

    #[test]
    fn cue_sheets() {
        let text = r#"REM GENRE Jazz
PERFORMER "Someone"
TITLE "The Album"
FILE "Side A.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Opening"
    PERFORMER "Someone"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Second Tune"
    INDEX 00 03:58:10
    INDEX 01 04:01:37
  TRACK 03 AUDIO
    INDEX 01 71:30:00
FILE side_b.wav WAVE
  TRACK 04 AUDIO
    TITLE "Closer"
    INDEX 01 00:00:00
"#;
        let files = parse_cue(text, Path::new("/music/album"));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("/music/album/Side A.flac"));
        let marks: Vec<(i64, &str)> = files[0].marks.iter().map(|(t, l)| ((t * 1000.0).round() as i64, l.as_str())).collect();
        // 4:01 and 37 frames of 75; the album title is not a track title.
        assert_eq!(marks, [(0, "Opening"), (241_493, "Second Tune"), (4_290_000, "Track 3")]);
        assert_eq!(files[1], CueFile { path: "/music/album/side_b.wav".into(), marks: vec![(0.0, "Closer".into())] });
    }

    #[test]
    fn written_playlists_read_back() {
        let dir = std::env::temp_dir().join(format!("woodshed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = vec![dir.join("sub/Tom & Jerry <live>.mp3"), PathBuf::from("/elsewhere/café 100%.flac")];
        for name in ["list.m3u8", "list.m3u", "list.pls", "list.xspf"] {
            let file = dir.join(name);
            write(&file, &tracks).unwrap();
            assert_eq!(read(&file).unwrap(), tracks, "{name}");
        }
        // M3U entries below the playlist's folder are relative.
        let m3u = std::fs::read_to_string(dir.join("list.m3u8")).unwrap();
        assert!(m3u.contains("\nsub/Tom & Jerry <live>.mp3\n"), "{m3u}");
        // Latin-1 bytes in a plain .m3u are accepted.
        std::fs::write(dir.join("latin.m3u"), b"caf\xe9.mp3\n").unwrap();
        assert_eq!(read(&dir.join("latin.m3u")).unwrap(), [dir.join("café.mp3")]);
        assert!(read(&dir.join("missing.m3u")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
