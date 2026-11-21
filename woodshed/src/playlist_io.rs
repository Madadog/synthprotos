//! Reading and writing playlist files: M3U / M3U8, PLS and XSPF.
//!
//! Only local files are kept: entries that are web streams are skipped.
//! Relative entries are resolved against the playlist's own directory.

use std::path::{Path, PathBuf};

pub const EXTENSIONS: &[&str] = &["m3u8", "m3u", "pls", "xspf"];

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
