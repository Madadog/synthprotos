# Woodshed

A desktop music player for transcription and rehearsal, built with
[egui](https://github.com/emilk/egui). Open a recording, slow it down, loop
the hard bar, and read the notes off a pitch-axis spectrogram.

- **Zoomable display** of the waveform, a spectrogram, or both, with its own
  time ruler and playhead. `F` gives the display the whole screen.
- **Spectrogram with a pitch axis**: a piano ribbon down the left with every
  C labelled by octave; hovering shows the note, cents offset and frequency
  under the cursor.
- **Full-song seek bar** in the usual place, which also shows the bookmarks,
  the loop, and the stretch the zoomed display is looking at.
- **Loops**: a dragged selection, the section between two bookmarks, or the
  whole track, with an optional gap between repeats.
- **Playhead options**: the display can page along, keep the playhead
  centred, or stay put; pausing can return to where playback started.
- **Labelled bookmarks**: `B` drops one at the playhead, `[` and `]` skip
  between them, `1`–`9` jump straight to one. Bookmarks, the loop selection
  and speed are remembered per file.
- **Speed** from 25% to 150%. With "Keep pitch" on it time-stretches, so the
  notes stay where they are; with it off it resamples like a tape machine,
  so half speed is also an octave down.
- **Playlists**: any number of named playlists of files. Add files from the
  playlist tab, by dropping several on the window, or on the command line.
  `N` and `P` skip between tracks, and the next track starts by itself when
  one ends (unless a loop is on).
- **Playlist files**: M3U / M3U8, PLS and XSPF. Opening or dropping one adds
  it as a playlist named after the file, "+ Files…" merges it into the
  current playlist, and "Export…" writes any of the three formats. Entries
  that are web streams are skipped.
- **Playlist view** (a toggle in Preferences): the playlist becomes a view
  alongside waveform and spectrogram, so `V` cycles to it and `F` makes it
  full screen. It is a table with title, artist, album, track number, length
  and type read from the files' tags; clicking a column header sorts the
  playlist by it (again to reverse). Off, the playlist is a compact list in
  the sidebar.
- **Spectrogram themes**: Inferno, Magma, Viridis, Ice, Phosphor, Greyscale
  and Paper (dark on white).
- **Preferences** (`Ctrl+,`): playback behaviour, seek steps, playhead
  following, interface size, spectrogram theme and range.

Reads MP3, FLAC, WAV, Ogg Vorbis, AAC/M4A, ALAC, AIFF and CAF.

## Build and run

```sh
cd woodshed
cargo run --release -- path/to/song.mp3   # optional; several files go into the playlist
```

On Linux, the ALSA headers are needed to build (`libasound2-dev` on
Debian/Ubuntu). Without an audio device the player still runs, silently.

This crate is deliberately not part of the workspace at the repository root:
it has its own `Cargo.lock`.

## Controls

| Key | |
|---|---|
| `Space` | Play / pause |
| `←` `→` | Seek back / forward (`Shift`: further; steps set in Preferences) |
| `Home` | Back to the start |
| `↑` `↓` | Speed up / slow down by 5% |
| `T` | Speed keeps pitch (time-stretch) on / off |
| `N` `P` | Next / previous track in the playlist |
| `B` | Drop a bookmark at the playhead (`Shift+B`: and name it) |
| `[` `]` | Previous / next bookmark (also `PgUp` / `PgDn`) |
| `1`–`9` | Jump to bookmark 1–9 |
| `L` | Loop on / off |
| `I` `O` | Set the loop start / end at the playhead |
| `Esc` | Leave full screen, or clear the selection |
| `V` | Cycle waveform / spectrogram / both |
| `F` | Full-screen display |
| `+` `-` | Zoom in / out |
| `Z` | Zoom out to the whole track |
| `Ctrl+O` | Open a file or playlist (or drop one on the window) |
| `Ctrl+,` | Preferences |

| Mouse, in the display | |
|---|---|
| Click | Move the playhead |
| Drag | Select a loop region; drag its edges to adjust |
| Right-drag | Pan |
| Scroll | Zoom the timeline (`Shift`: pan) |
| `Alt`+scroll | Zoom the pitch axis |
| Scroll on the piano | Pan the pitch axis (`Ctrl`: zoom) |
| Drag a bookmark flag | Move the bookmark |

## How it works

- `track.rs` decodes the whole file into memory with Symphonia.
- `engine.rs` plays it: an optional WSOLA time-stretcher for the speed control, loop
  handling, and resampling to the output device, driven by a cpal stream.
- `analysis.rs` builds the waveform peak pyramid and, on background threads,
  the spectrogram: three FFT sizes blended onto a grid of four rows per
  semitone, so bass notes are resolved without smearing the treble in time.
- `playlist_io.rs` reads and writes M3U, PLS and XSPF playlists.
- `metadata.rs` reads tags and durations on a worker thread for the playlist
  table, and sorts by them.
- `display.rs` draws the zoomable view; the spectrogram is cached as texture
  tiles at power-of-two zoom levels.
- `app.rs` holds the state, hotkeys, transport bar and bookmark sidebar;
  `session.rs` saves settings, playlists and per-file bookmarks under the
  user data directory.
