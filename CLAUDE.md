# Nullamp

A fast, retro-inspired desktop music player with first-class DJ mix (setlist) support and a metadata editor. Written in Rust. The original product brief is `writing-block.md`; the full development record (status, decisions, measurements, lessons, to-do list) is `docs/HANDOFF.md`. Read both before starting larger work.

## Current focus (user preference)

- **macOS only for now.** The user wants a working, likeable macOS app before any Linux or Windows work. Do not run Docker, cross-builds or other-OS packaging unless asked.
- **No online tracklist search for now.** It was removed from the app on purpose; see "Online sources" in `docs/HANDOFF.md` before proposing it again.
- Disk space is tight: `/` and `/Volumes/Development` share one APFS container. Keep `target/` lean and never create large Docker artifacts.

## Layout

| Path | What it is |
|---|---|
| `crates/nullamp-core` | Pure logic, no I/O: playlists, play order (shuffle/repeat), setlists and validation, M3U, cue sheets, pasted-tracklist parser, commands and media-key dedupe |
| `crates/nullamp-audio` | Audio engine: symphonia decoding on its own thread, rubato resampling, rtrb ring buffer, cpal output, gapless preload, device recovery |
| `crates/nullamp-meta` | Tags via lofty, ID3 chapters via id3, artwork, technical info, safe tag writing with backups |
| `crates/nullamp-online` | HTTP client with rate limiting, disk cache and host allowlist; MusicBrainz, Cover Art Archive, Mixcloud (Mixcloud is unused by the app) |
| `crates/nullamp-sysmedia` | OS media keys and Now Playing via souvlaki |
| `app/` | The `nullamp` binary: egui UI (`src/ui/`), playback controller (`player.rs`), persistence (`store.rs`), background workers (`workers.rs`) |
| `tests/fixtures/` | Small generated audio files (ffmpeg) used by tests |
| `packaging/` | macOS bundle script, Info.plist, icon generator; unused Linux Dockerfile and .desktop file |

## Commands

```sh
cargo test --workspace                       # all tests (about 120, all pass on macOS)
cargo test -p nullamp ui_snapshots -- --ignored --test-threads=1   # renders the real UI offscreen to target/screenshots/*.png
cargo test -p nullamp-online --test live -- --ignored   # hits MusicBrainz/CAA/Mixcloud for real
cargo run --release -p nullamp-audio --example probe -- FILE [NEXT_FILE] [--audible]   # engine latency probe, silent by default
cargo run --release -p nullamp-audio --example waveform FILE   # times the waveform pass for a file
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
- Nothing has been committed to git yet. Ask before committing.
