<p align="center">
  <img src="docs/images/icon.png" width="128" height="128" alt="Nullamp icon">
</p>

<h1 align="center">Nullamp</h1>

<p align="center">
  A fast, retro-inspired desktop music player with first-class DJ mix support and a safe metadata editor.<br>
  Written in Rust.
</p>

<p align="center">
  <img src="docs/images/mix-setlist.png" alt="Nullamp playing a DJ mix, with the setlist panel open" width="900">
</p>

Nullamp is for people who keep a folder of music and a pile of long DJ mixes. It opens files in milliseconds, plays them gapless, and knows which track *inside* a one-hour mix is playing right now.

It is not a streaming client and not a library manager. It plays your files.

## Features

**Player**
- Instant response: a warm file is audible in about 40 ms, seeks land in a few milliseconds, and opening files never blocks the UI.
- Gapless playback, shuffle, repeat, volume ramping without clicks, and recovery when the output device changes (headphones, Bluetooth, sleep and wake).
- Seven-segment LCD with elapsed or remaining time, plus a spectrum or oscilloscope view aligned to what you actually hear.
- Multiple playlists in tabs with a virtualized table, drag to reorder, and persistence between sessions.
- Media keys and the macOS Now Playing widget, even when the window is in the background.
- Dark and light themes, or follow the system.

**DJ mixes**
- A mix is a single file with a *setlist*. Nullamp reads it from ID3 chapters, embedded or adjacent cue sheets, or a plain track list in the comment tag.
- The LCD, the timeline and the setlist panel always show the current track. Double-click an entry to jump to it, or use Shift+arrows to step between tracks.
- Paste a tracklist from a web page or a video description and Nullamp parses the timestamps. Approximate times are drawn dashed and marked with "≈" until you correct them with **→ playhead**.
- Import and export cue sheets and M3U.
- Setlists are tied to the audio content, so they survive renaming, moving and retagging the file.

**Metadata**
- Inspector for tags, technical info, chapters and artwork, with batch editing for up to 500 files.
- Optional MusicBrainz lookup and Cover Art Archive artwork. Everything else works offline.
- Tag writes are safe: the file is copied, edited, verified by reading it back, and then atomically swapped in. Chapters and unknown frames survive, and originals can be backed up and restored.

**Formats:** MP3, AAC and M4A, ALAC, FLAC, Ogg Vorbis, WAV and AIFF.

## Install

Download the latest build from the [Releases](https://github.com/jbrunemann/nullamp/releases) page.

| Platform | Status |
|---|---|
| macOS (Apple silicon) | Supported and tested. Unzip and drag `Nullamp.app` to Applications. The app is not signed or notarised yet, so the first launch needs a right-click and Open. |
| Linux (x86_64) | A tarball is built by CI but has not been tested yet. |
| Windows | Not started. |

## Build from source

You need a recent stable Rust toolchain (1.88 or newer).

```sh
cargo run --release                    # run the app
cargo test --workspace                 # run all tests
packaging/macos/bundle.sh              # build target/bundle/Nullamp.app
NULLAMP_HOME=/some/dir cargo run --release   # use an isolated config, data and cache directory
```

On Linux you also need the ALSA, xkbcommon and Wayland development packages (`libasound2-dev libxkbcommon-dev libwayland-dev` on Debian and Ubuntu).

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space | Play or pause |
| Z X C V B | Previous, play, pause, stop, next |
| ← / → | Seek 5 seconds |
| ⇧← / ⇧→ | Previous or next setlist entry |
| + / − / M | Volume up, down, mute |
| S / R | Shuffle, repeat |
| ⌘O / ⇧⌘O | Add files, add folder |
| ⌘P | Show or hide the playlist |
| F1 | Help |

## Project layout

| Path | What it is |
|---|---|
| `crates/nullamp-core` | Playlists, play order, setlists, M3U, cue sheets and the tracklist parser (pure logic) |
| `crates/nullamp-audio` | Decoding, resampling, ring buffer, output and gapless preload |
| `crates/nullamp-meta` | Tags, chapters, artwork and safe tag writing |
| `crates/nullamp-online` | Rate-limited HTTP client for MusicBrainz and Cover Art Archive |
| `crates/nullamp-sysmedia` | OS media keys and Now Playing |
| `app/` | The egui application |


## Known limitations

- Seeking far into a VBR MP3 without a seek table is approximate. CBR files, which covers most DJ mixes, seek exactly.
- The setlist panel follows the file that is playing. You cannot edit the setlist of a file that is not playing.
- Online tracklist search is not part of the app. Use the paste import instead.

## License

MIT or Apache-2.0, at your option.
