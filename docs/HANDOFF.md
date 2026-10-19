# Nullamp development record

Everything learned and decided in the first development session (2026-10-18), written so that work can resume in a new conversation. The product brief is `../writing-block.md`. Working conventions and commands are in `../CLAUDE.md`.

## 1. Status at a glance

Working on macOS: playback, playlists, media keys, setlists, metadata editing, persistence. The app bundle builds with `packaging/macos/bundle.sh`. The user has seen it running and asked to continue on macOS only.

| Brief milestone | State |
|---|---|
| 1. Technical validation | Done. Choices and reasons in section 3. |
| 2. Core player | Done on macOS. Manual check of media keys, file dialogs and drag-and-drop is still owed. |
| 3. DJ mix support | Done: chapters, embedded and adjacent cue sheets, plain track lists in the comment or description tag (reference only when untimed), pasted tracklists, timeline segments, editing, persistence. |
| 4. Metadata management | Done: inspector, batch edit, artwork, safe writes, backups and restore. |
| 5. Online enrichment | Partly. MusicBrainz and Cover Art Archive lookups for single tracks work. Online tracklist search was built and then removed by request (section 7). |
| 6. Hardening and release | Not started: no README, no CI, Linux and Windows untested (the Windows exe cross-builds and links, but has never been run), no signing or notarisation. |

Tests: core 54, audio 9, meta 15, online 14 (2 live, ignored by default), sysmedia 2, app 32 (1 screenshot render, ignored). All pass on macOS. Nothing is committed to git yet (branch `master`, no commits).

## 2. Definition-of-done checklist (brief section 17)

1. Play local MP3 reliably: yes.
2. Media keys when unfocused: implemented via souvlaki (MPRemoteCommandCenter); **not yet verified by hand**.
3. Multiple persisted playlists: yes.
4. Inspect and edit metadata and artwork safely: yes.
5. Single-file mix with cue sheet or chapters: yes.
6. See the current constituent track: yes (LCD, timeline, setlist panel, OS Now Playing).
7. Jump to a setlist entry: yes (double-click, Shift+arrows).
8. Correct and persist tracklists: yes.
9. Online tracklists and artwork: artwork yes; online tracklists deliberately removed for now.
10. Works offline: yes; online lookups are optional and toggled in the status bar.
11. macOS and Linux: macOS yes; Linux compiled in Docker but was never linked or tested (disk filled up).
12. No delays or freezes: measured, see section 4.

## 3. Architecture and decisions

### Crates and responsibilities

- `nullamp-core`: pure, fully unit-tested logic. No I/O, so most behaviour is testable without hardware.
- `nullamp-audio`: the engine, behind a small command/event API with tokens.
- `nullamp-meta`, `nullamp-online`, `nullamp-sysmedia`: one concern each.
- `app`: the egui UI, the playback controller, persistence and workers.

### Decisions and why

- **Rust + egui (eframe 0.37, wgpu renderer)** rather than Slint. The UI is mostly custom painting (seven-segment LCD, mix timeline, dense virtualized table), which egui does directly. Immediate mode means no binding layer between model and view. egui is MIT/Apache, whereas Slint is GPLv3, royalty-free with attribution, or commercial.
- **Own engine on symphonia 0.6 + cpal 0.18 + rubato 5 + rtrb**, not rodio. rodio 0.23 was a week-old stopgap release ahead of a rewrite, and it decodes inside the audio callback, which glitches when files sit on slow or network volumes. Our engine decodes on its own thread into a 2-second ring buffer. The callback only copies samples and applies gain. It never blocks: it uses `try_lock` and plays silence for one period on contention.
- **Tokens everywhere.** Each load gets a fresh token; positions and events carry it; the controller ignores other tokens. This is what keeps rapid skips, seeks and edits coherent.
- **Controller on the UI thread**, in eframe's `logic()` hook, which also runs while the window is hidden. Engine, workers and OS media callbacks wake it with `request_repaint`. Simpler than a separate controller thread, and frames are short.
- **Gapless preload.** The controller tells the engine which entry comes next; the engine opens it on a loader thread and decodes into the same ring buffer. If the preload is still opening at end of track, the engine waits up to 2 s, then ends normally and the controller loads the next track.
- **File opening on short-lived loader threads**, so a cold disk or stalled network share never blocks pause, stop or a newer load. Stale results are dropped.
- **Hybrid MP3 seeking.** symphonia's accurate MP3 seek parses every frame header from the current position, and rescans from the start for backward seeks. That reads hundreds of MB for a far seek in a long mix. Forward seeks of 20 s or less stay accurate; far seeks use coarse byte-position seeking. That is exact for CBR (nearly all DJ mixes) and approximate for VBR without a seek table. Frames before the target are dropped using packet timestamps.
- **Device handling.** The stream is paused after 3 s idle (paused or stopped) so macOS can sleep. It is rebuilt on cpal `DeviceChanged`, `StreamInvalidated` or `DeviceNotAvailable` errors, and by a watchdog when the callback stops firing for 2 s. Playback resumes at the same position.
- **Output device choice.** `AudioEngine::set_output_device(Option<id>)` rebuilds the stream on that device and resumes at the same position (the same path as device recovery); `None` follows the system default. Ids are cpal `DeviceId` strings (`coreaudio:BuiltInSpeakerDevice`), saved as `output_device` in settings. A device that is gone falls back to the default (`OutputOpened.fell_back`) and is tried again on every reopen, so the choice sticks across unplugging. `request_output_devices()` enumerates on the engine thread and answers with `EngineEvent::OutputDevices`. The device list is requested on every `OutputOpened`, so the picker is already full on the first click. Each device has an `OutputKind` (`kind.rs`; on macOS CoreAudio's transport type read through objc2-core-audio, since cpal reports Unknown there; elsewhere cpal's description). The UI shows `OUT <NAME>` with a vector icon at the right end of LCD line 3; clicking it opens the picker (`deck.rs::output_label`, `output_menu`): real hardware first, virtual devices dimmed under a divider, icons from `widgets::paint_output_icon` (painted, so no font changes). `render_output_menu` in `ui_snapshots.rs` renders the open menu and an icon sheet. Tested by hand on macOS across eight devices; not yet on Windows or Linux.
- **Visualiser tap.** The callback writes each period as mono, pre-volume samples into a lock-free 8192-frame history (`output::Scope`), together with the frame index and the instant that period reaches the speaker (cpal's playback minus callback timestamp). `AudioEngine::scope(out, lead)` returns the frames ending at what is audible `lead` from now; the UI passes 12 ms for display delay. Without that alignment the picture runs a whole period plus device latency ahead of the sound. The UI side (`app/src/ui/visualizer.rs`) runs a hand-written 1024-point FFT every frame (no extra crate) and keeps its tunables (fall speed, dB floor, bar size) as constants at the top. While playing, the visualiser requests a repaint every frame, so the whole UI redraws at display refresh (up to 120 Hz) instead of the previous 33 ms tick; CPU cost not measured yet.
- **Waveform overview.** `nullamp_audio::compute_waveform` decodes the file once at its own rate and channel layout (`Decoding::open_native`, no resampling) into 10 ms blocks, then merges them into at most 2048 buckets of peak and RMS (u8 each, about 4 KB). It checks a cancel closure per packet. The app runs it on its own `nullamp-waveform` thread (a 32-minute 256 kbps MP3 takes 1.4 s in a release build), where only the newest request is kept and a newer one cancels the running decode, so skipping through a playlist costs nothing. Results are cached in `cache/waveforms/` as `<hash of store::fingerprint>.wf` (magic `NWF1`; any other format is a miss and gets recomputed), so they survive retagging and renaming. A cache hit refreshes the file's modified time at most once a day, and the thread removes entries unused for `waveform_cache::MAX_AGE` (90 days) when the app starts. The timeline draws it under the setlist boundaries and the progress tint (`deck::draw_waveform`, one mesh, one column per point, scaled to the file's loudest peak). Files on network volumes are decoded like any other; nothing detects them.
- **Pause, resume and volume** are atomics read by the callback, with a 15 ms gain ramp (no clicks). They take effect within one audio period.
- **Media keys** through souvlaki 0.8 (macOS MPRemoteCommandCenter and Now Playing; MPRIS via zbus on Linux; SMTC on Windows, which needs the HWND). A deduplicator drops the same transport command arriving from two different sources within 150 ms.
- **OS Now Playing for mixes** shows the constituent track as title and artist, with "Mix title, DJ" as the album, so the relationship stays visible.
- **Persistence**: JSON files with atomic writes (temp file, then rename); unreadable files are renamed `.broken` and reported. Locations come from `directories` (`~/Library/Application Support/Nullamp` etc.) or `NULLAMP_HOME`. Window geometry is stored by eframe in `window.ron`.
- **Setlist identity**: saved setlists are keyed by a content fingerprint. It hashes 64 KiB after any leading ID3v2 tag and 64 KiB before any trailing ID3v1 tag, so it survives moving, renaming and retagging. Fallback lookup is by last known path.
- **Safe tag writing** (`nullamp_meta::write`):
  1. Validate the values.
  2. Check that size and mtime are unchanged, to catch external edits.
  3. Copy the file to a hidden temp file in the same directory.
  4. Write the tags, keeping the original ID3v2 version (lofty defaults to v2.4; we keep v2.3 files as v2.3).
  5. Mirror the edits to any ID3v1 or APE tags.
  6. Verify: tags read back as written, and duration and sample rate are unchanged.
  7. Check the original again for external edits.
  8. Optionally back up the original. The backup directory has a size budget, oldest deleted first, default 2 GB.
  9. Atomically rename the temp file over the original.

  Tests prove the audio bytes, unknown frames (PRIV, TXXX) and CHAP/CTOC chapters survive.
- **Workers**: an interactive lane (now-playing info, inspector, writes) always served before a bulk lane (playlist scans, folder expansion), plus a separate online thread so network never delays local work.
- **Themes**: dark (default), light, or follow the OS, cycled by the "theme:" label in the status bar and saved as `appearance` in settings. `ui/theme.rs` holds a `Palette` per theme; UI code calls `theme::text()`, `theme::panel()` etc., which read the palette synced from egui each frame. The LCD (`LCD_*` consts) stays dark in both themes; chrome that borrows LCD hues uses `now()`, `mix_tag()`, `key()` for legibility on light surfaces. The palette flag is process-global, so run snapshot tests with `--test-threads=1`. `render_main_window_light` writes the `*-light.png` shots.
- **App icon**: `packaging/icon.py` (standard-library Python, signed-distance drawing so any size is anti-aliased) renders a graphite plate with an LCD showing a lit mint spectrum with amber peaks above a mix timeline with an orange playhead. It feeds the macOS `.icns` (`bundle.sh` uses the committed `packaging/icon-1024.png`), the Linux icon sizes and the Windows `.res`. The window icon and the About dialog use the committed `app/assets/icon-256.png`. After changing `icon.py`, regenerate both PNGs: `python3 packaging/icon.py packaging/icon-1024.png` and `python3 packaging/icon.py app/assets/icon-256.png 256`.
- **Fonts**: egui 0.37's bundled fonts lack arrows, ⌘ ⌥ ⇧, ⚠ ✓ ✎ ✕ ▲▼▶▸ ◎ and similar. A 3.9 KB subset of DejaVu Sans (`app/assets/fonts/NullampSymbols.ttf`, licence beside it) is installed as a fallback for both families. To add glyphs, install fonttools in a venv and re-subset DejaVuSans.ttf 2.37 with `pyftsubset DejaVuSans.ttf --text="<all symbols>" --no-hinting --layout-features='' --output-file=NullampSymbols.ttf`.
- **Native menu (macOS)**: `ui/menu.rs` replaces winit's default menu bar through `muda` with a Nullamp menu (About, Open Config File, Services, Hide, Quit) and a Window menu. `App::install_native_menu` runs from the eframe creator in `main.rs`, not `App::new`, because muda needs the main thread and tests build the app elsewhere. Open Config File writes `settings.json` if missing, then runs `open -t` on it. The app rewrites that file on its next save, so edits made while it runs can be overwritten. About opens an egui window (`dialogs.rs`), not the system panel, so it can show the last 50 log lines kept by `logbuf.rs` (a tee in front of env_logger, so it holds whatever `RUST_LOG` lets through; default is info for nullamp). `render_about` in `ui_snapshots.rs` renders it. The menu bar itself is not checked by eye: screen capture is not permitted here.

### UI layout (single window)

- **Top deck**: the LCD (seven-segment time, which you click to switch between elapsed and remaining; a visualiser under the time, segmented spectrum, oscilloscope, then off on successive clicks (the choice is saved in `settings.json`), fed by a lock-free tap in the audio callback that records when each period reaches the speaker so the picture matches what is audible, not what was just buffered; the file name, dim, on the bottom line; a scrolling title, amber for the track inside a mix; a context line; the stream format; mode indicators), artwork (switches to setlist-entry artwork while that entry plays), the timeline (setlist segments: exact boundaries solid, approximate dashed, active in amber; hover tooltip; click or drag to seek on release), and the transport (a Playlist tab beside Setlist and Inspector, ⌘P, hides the playlist; with the side panel also hidden the window shrinks to just the deck and status bar and restores its height afterwards; line 3 of the LCD reads "FILE n/m" for the playlist position and "TRACK n/m" for the setlist entry; painted icons, shuffle, repeat cycle, mute, segmented volume meter that also responds to scrolling).
- **Centre**: playlist tabs, a toolbar (add, remove, move, Sort and List menus), and the virtualized table (the selection survives reordering; drag to reorder with an insertion line; external drops land at the pointer; right-click menu).
- **Right panel**, resizable: Setlist (mix card, source summary, validation problems, entry list, entry editor with "→ playhead", import and export, paste dialog) and Inspector (fields, batch edit, artwork, MusicBrainz lookup, technical info, chapters, other frames). The Setlist tab, the mix lines on the LCD and the timeline segments only show for a mix (`MixState::is_mix`): a file with a setlist of at least two entries (or one the user made), or longer than 20 minutes. Single tracks show the Inspector without changing the saved tab; ⌘L still opens the Setlist on one to add a setlist.
- **Status bar**: counts, messages, online on/off toggle, F1 help.
- **Shortcuts**: Space/C play-pause, Z X C V B (Winamp row), ←/→ seek 5 s, ⇧←/⇧→ previous/next setlist entry, +/−/M volume, S/R shuffle/repeat, ↑/↓/Home/End/Enter/Delete in the list, ⌥↑/⌥↓ move entries, ⌘A, ⌘O, ⇧⌘O, ⌘I, ⌘L, ⌘J, F1.

## 4. Measured performance (M-series Mac, two 32-minute 256 kbps MP3 mixes)

| Action | Result |
|---|---|
| Load to audible (file warm) | 37 to 43 ms |
| Seek anywhere (coarse) | about 1 ms, plus 3 to 7 ms until audible |
| Resume after pause | 2 to 10 ms |
| Gapless transition | seamless, 0 underruns |
| Cold first open of a 62 MB file | 2.8 s (disk or iCloud fetch); harmless now because opening is off-thread |

## 5. Library notes (versions current in October 2026)

- **symphonia 0.6.1**: new API (`AudioDecoderOptions`, `GenericAudioBufferRef`, `next_packet()` returns `Ok(None)` at the end). Decoders apply gapless trimming themselves. Accurate MP3 seeking is linear (see above). It reads ID3 CHAP frames too, but we use the `id3` crate.
- **cpal 0.18.2**: a unified `cpal::Error`/`ErrorKind`; streams no longer auto-start (call `play()`); `Stream` is now Send + Sync; CoreAudio reports `DeviceChanged` when the default output changes. On Linux enable the `pulseaudio` feature (pure Rust).
- **rubato 5**: buffers through `audioadapter`; we use `Fft` with `FixedSync::Input`, a 1024-frame chunk, output delay compensation and exact tail trimming (tested).
- **lofty 0.25**: errors are split per operation (`FileParseError`, `FileEncodingError`, etc.); `WriteOptions::use_id3v23`. The generic `Tag` view hides custom TXXX and PRIV frames but preserves them on save; we list them through the `id3` crate. `ItemKey` has no `Unknown` variant.
- **id3 1.17**: `read_from_path` also handles WAV and AIFF; remove chapters with `tag.remove("CHAP")` and `tag.remove("CTOC")`. ffmpeg writes ID3v2.3 strings with trailing NULs, so strip them for display.
- **egui / eframe 0.37**: `App::ui(&mut self, ui, frame)` plus `App::logic`; panels are `egui::Panel::top/bottom/left/right("id").show(ui, …)`; `Modal::new(Id::unique("…"))`; `Id::new` is deprecated; `ctx.all_styles_mut`; `DroppedFile` is a trait with `path()`; `ui.close()` closes menus; `ui.fonts_mut(|f| f.layout_job(job))`; `TextWrapping::truncate_at_width` for ellipsis.
- **egui_kittest 0.37** (dev-dependency with eframe, wgpu and snapshot features): `Harness::builder().with_size(…).with_pixels_per_point(2.0).wgpu().build_eframe(|cc| App::new(cc, store))`, then `step()` and `render()`. Missing glyphs panic in tests, which is how the font gap was found.
- **souvlaki 0.8.3**: uses the old `cocoa`/`objc` crates and is only lightly maintained, but it covers all three platforms. Linux uses the `use_zbus` feature. If it breaks, replace it behind `nullamp-sysmedia` (for example with `objc2-media-player`).
- **rfd 0.17**: on Linux the XDG portal is the default backend. Dialogs run with `AsyncFileDialog` on a helper thread via `pollster`, so the UI loop keeps handling media keys while a dialog is open.
- **ureq 3**: configured with `https_only`, `max_redirects(0)` (redirects are followed manually so each hop's host can be checked) and `http_status_as_error(false)`. Cover Art Archive JSON and images both redirect to `archive.org`.

## 6. Pitfalls we hit (do not repeat)

- **Disk full.** A Docker-based Linux build (rust image, cargo volume, 4.4 GB `target/linux`) filled the shared APFS container; Docker's daemon went read-only and the session could not run commands. Still left behind, and the user's to remove when they want: Docker image `nullamp-linux`, volume `nullamp-cargo`, directory `target/linux` (about 4.4 GB). The `macsplorer-*` volumes, `rust:latest` and `ubuntu:22.04` belong to another project of the user; never touch them.
- **Live window capture is not possible** here (no Screen Recording permission). Use the `ui_snapshots` test instead.
- The first seven-segment digit geometry was asymmetric and read "C:CB" instead of "0:03"; fixed. Check renders after touching `widgets.rs`.
- Cross-compiling for Linux or Windows from macOS fails on `ring` (rustls) without a C cross-toolchain.
- In tests, a second identical transport command from a *different* source within 150 ms is dropped by the deduplicator by design.
- The pause fade must limit how many samples it consumes, otherwise resuming skips ahead by up to one buffer (fixed; covered by a test).

## 7. Online sources (findings, October 2026)

Online tracklist search was removed from the app at the user's request because it rarely produced tracklists. Findings to revisit later:

- **1001Tracklists** and **mixid.fm**: no public API; both block automated access (mixid.fm returned HTTP 403). Do not scrape. The supported route is paste import: copy from the page in a browser, then Setlist → Import → Paste tracklist, with the page URL kept as the source.
- **Mixcloud**: the public API finds mixes (title, DJ, date, URL, artwork), but `sections` is always empty now and descriptions rarely hold tracklists. The provider code and tests remain in `crates/nullamp-online/src/mixcloud.rs`, unused by the app.
- **MixesDB**: shut down, then relaunched as Mixes.wiki; no API documentation or terms found.
- **AI search engines** find *where* tracklists are, but merge sources that disagree and fill gaps. Example: for "Miss Monique – Sunset DJ Mix in Mykonos, Greece 2024" they returned two different lists (watchthedj.com with 15 tracks; the YouTube description with 16). Not usable as an automatic source.
- **Best next option**: the YouTube Data API (free key, permitted). Search for the mix and parse timestamped tracklists from the description and top comments with `nullamp_core::tracklist_text`. Many of the user's mixes are YouTube downloads. Pair it with a "Search the web" button that opens the browser with "<mix> tracklist" for paste import.
- **Audio recognition** (the user likes it, on hold): AudD's enterprise endpoint for long files, billed per 12 s of audio; ACRCloud file scanning (hearthis.at uses it, with 20 s segments). It needs a user API key and explicit consent before uploading audio (brief section 13).
- **Still in the app**: MusicBrainz recording search plus Cover Art Archive artwork in the Inspector, for single tracks and for setlist entries ("Look up…" in the entry editor). MusicBrainz allows at most 1 request per second and requires a User-Agent with contact info. Set `NULLAMP_CONTACT` when building releases; it currently sends "contact not configured".

## 8. Open work (suggested order)

1. **Hands-on macOS review with the user**: visualiser (does it feel in sync with real music, does the spectrum look lively, CPU use at full refresh; cap repaints at 60 Hz if needed), media keys in the background, file dialogs, drag-and-drop from Finder, window resizing, a large playlist (thousands of entries), switching audio devices (headphones, Bluetooth), sleep and wake. Collect UI feedback.
2. Paste import against real pages the user copies (mixid.fm, YouTube descriptions, 1001Tracklists) and adjust `tracklist_text` to their formats.
3. README with build instructions and limitations; clippy pass; commit to git (ask first).
4. Polish ideas not yet built: column sorting by clicking headers, playlist search/filter, a settings dialog (backups, online toggle, resume-on-start; these exist in `settings.json` but have no UI except the online toggle), opening files from Finder ("Open with" is declared in Info.plist but file-open events are not handled yet).
5. Release work on macOS: universal binary (`bundle.sh --universal`), Developer ID signing and notarisation.
6. Later, when the user says so: Linux (Dockerfile and .desktop are in `packaging/linux`; AppImage not written), Windows (done as a build: `packaging/windows/package.sh` cross-builds with `cargo xwin` and zips; `app/build.rs` embeds the icon through a hand-written .res from `mkres.py`; release builds use the GUI subsystem; CI has a native `windows-x86_64` job; still to do on a real PC: run it, check audio devices, SMTC media keys, file dialogs and the tag-write-while-playing error; no code signing), the YouTube provider, audio recognition.

## 9. Known limitations

- Far seeks in VBR MP3s without a seek table are approximate.
- The setlist panel follows the playing file only; you cannot edit the setlist of a file that isn't playing.
- Batch editing is capped at 500 files.
- Writing tags to the file that is playing works on macOS and Linux (the engine keeps the old inode open). On Windows the rename would fail and is reported as an error.
- Approximate timestamps from pasted lists are shown with "≈" and dashed timeline boundaries. Use "→ playhead" while listening to correct them; that marks them exact.
