<p align="center">
  <img src="docs/images/icon.png" width="96" height="96" alt="Nullamp icon">
</p>

# Nullamp

A fast, retro-inspired desktop music player with first-class DJ mix (setlist) support and a metadata editor. Written in Rust. The original product brief is `writing-block.md`; the full development record (status, decisions, measurements, lessons, to-do list) is `docs/HANDOFF.md`. Read both before starting larger work.

## Current focus (user preference)

- **macOS only for now.** The user wants a working, likeable macOS app before any Linux or Windows work. Do not run Docker, cross-builds or other-OS packaging unless asked.
- **Almost no online features.** The first tracklist search (Mixcloud) and the MusicBrainz/Cover Art Archive lookup were both removed on purpose (too error prone and complex for how rarely they were used), and the `nullamp-online` crate is gone. Five things use the network: the user-initiated update check on the Settings Updates tab (`nullamp-update`), the cover lookup for the active track of a mix (`nullamp-artwork`, Deezer then iTunes, once per track, cached), and the user-initiated "Identify with Shazam" on a setlist entry (`nullamp-shazam`, signatures of short clips to Shazam's unofficial endpoint, chosen by the user knowing it is against Shazam's terms and may break), and the user-initiated "Search 1001Tracklists" in the setlist's Import menu (`nullamp-tracklists`, the site's search page and a tracklist page, read with the user's own `guid` cookie from Settings > Web; the site has no API and this was chosen by the user), and the user-initiated "Request on Telegram" on a setlist entry (`nullamp-telegram`, asks a bot's inline mode for "Artist - Title" as the user's own Telegram account over MTProto and saves the sound file among its results; the login is made on Settings > Telegram with the user's own API id and hash). All but the last run `curl`. See "Online sources" in `docs/HANDOFF.md` before proposing anything else online.
- Disk space is tight: `/` and `/Volumes/Development` share one APFS container. Keep `target/` lean and never create large Docker artifacts.

## Layout

| Path | What it is |
|---|---|
| `crates/nullamp-core` | Pure logic, no I/O: playlists, play order (shuffle/repeat), setlists and validation, M3U, cue sheets, pasted-tracklist parser, commands and media-key dedupe |
| `crates/nullamp-audio` | Audio engine: symphonia decoding on its own thread, rubato resampling, rtrb ring buffer, cpal output, gapless preload, device recovery; offline analysis (waveform, tempo, where the tracks of a mix start) |
| `crates/nullamp-meta` | Tags via lofty, ID3 chapters via id3, artwork, technical info, safe tag writing with backups |
| `crates/nullamp-sysmedia` | OS media keys and Now Playing via souvlaki |
| `crates/nullamp-artwork` | Cover of one track of a mix by artist and title: Deezer search, then iTunes Search, via `curl`; result matching |
| `crates/nullamp-shazam` | Names the tracks of a mix with Shazam: signatures of 12 s clips in the Shazam apps' format, posted via `curl`, and the rule that several clips must agree |
| `crates/nullamp-tracklists` | 1001Tracklists: the search page (form post, no cookie) and a tracklist page (needs the user's `guid` cookie), read via `curl` and parsed from the HTML into tracks with times |
| `crates/nullamp-telegram` | Logs in to Telegram as the user (MTProto, `grammers`, session in a private JSON file, no libsql) and asks a bot for a track: sends the text, waits for a sound file, downloads it, logs other replies. Own thread with its own tokio runtime; `examples/ask.rs` tries it without the UI |
| `crates/nullamp-update` | Update check: asks a release address (GitHub latest release by default) via `curl` and compares versions; copied from macsplorer's `update-check` |
| `crates/nullamp-rekordbox` | Read-only reader of rekordbox libraries: a USB export (tracks from `export.pdb`, waveforms from the analysis files) and the desktop `master.db` (tracks and playlists, SQLCipher, user-supplied key) |
| `crates/nullamp-mixxx` | Read-only reader of the Mixxx library (`mixxxdb.sqlite`, plain SQLite): tracks, crates, playlists and play history |
| `app/` | The `nullamp` binary: egui UI (`src/ui/`), playback controller (`player.rs`), persistence (`store.rs`), background workers (`workers.rs`) |
| `tests/fixtures/` | Small generated audio files (ffmpeg) used by tests |
| `packaging/` | macOS bundle script, Info.plist, icon generator; unused Linux Dockerfile and .desktop file |

## Commands

```sh
cargo test --workspace                       # all tests (about 120, all pass on macOS)
cargo test -p nullamp ui_snapshots -- --ignored --test-threads=1   # renders the real UI offscreen to target/screenshots/*.png
cargo run --release -p nullamp-audio --example probe -- FILE [NEXT_FILE] [--audible]   # engine latency probe, silent by default
cargo run --release -p nullamp-audio --example waveform FILE   # times the waveform pass for a file
cargo run --release -p nullamp-audio --example tempo FILE...   # BPM estimate and time per file
cargo run --release -p nullamp-audio --example split -- MIX [--tracks N]   # where the tracks of a mix start, offline (the setlist's Find tracks)
cargo run --release -p nullamp-shazam --example identify -- MIX CUE   # names the tracks at the cue's starts with Shazam (30 requests for 6 tracks)
packaging/macos/bundle.sh                    # builds target/bundle/Nullamp.app
NULLAMP_HOME=/some/dir cargo run --release   # run with an isolated config/data/cache directory
```

Screen capture of the live window is not permitted on this machine, so use the `ui_snapshots` test (egui_kittest + wgpu) to look at the UI. View the PNGs with the Read tool.

## Conventions

- Match the surrounding code: concise doc comments that explain *why*, no commented-out code, `thiserror` for library errors, no `unwrap` on I/O paths.
- No file or network I/O on the UI thread. Put it in a `workers.rs` job, give it a request id, and drop stale results.
- Every engine load carries a token; the controller ignores events and positions for other tokens. Keep that invariant.
- Tag writes go through `nullamp_meta::write` only (temp copy, verify, atomic rename, optional backup).
- Every non-ASCII character used in UI text must exist in the bundled fonts; the `ui_glyphs_exist_in_bundled_fonts` test enforces it. To add a symbol, regenerate `app/assets/fonts/NullampSymbols.ttf` (see `docs/HANDOFF.md`).
- Ask before committing.
