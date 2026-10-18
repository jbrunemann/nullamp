# Project Brief: A Retro-Inspired, Cross-Platform Music and DJ Mix Player

## 1. Vision and product identity

Build a lightweight, polished desktop music player inspired by the spirit of classic Winamp and the workflow and visual language of professional DJ software.

The application is primarily intended for macOS and Linux, with Windows support as an important secondary target.

It should combine the immediacy, compactness, personality, and tactile controls of classic desktop music players with the track awareness, metadata capabilities, and long-form playback experience associated with DJ applications.

This is not a streaming client, a full digital audio workstation, or an elaborate music-library management system.

The core product is a fast local music player with an excellent playlist editor, first-class support for continuous DJ mixes, and a comprehensive metadata viewer and editor.

The application must feel responsive and dependable. Opening a track, changing songs, editing a playlist, seeking through a mix, or pressing a media key should produce an immediate and predictable response.

Create an original visual identity. Use Winamp and DJ software as sources of inspiration rather than copying either product literally.

## 2. Platform priorities and implementation language

Target platforms in this order:

1. macOS.
2. Linux.
3. Windows.

Prefer a single shared application codebase.

### Preferred technology direction

Use Rust for the application logic and audio-related components unless a documented technical evaluation demonstrates a compelling reason not to.

Evaluate a cross-platform GUI framework such as Slint or egui. Choose the framework based on its ability to produce the intended interface, rather than choosing solely on familiarity or implementation speed.

Evaluate a suitable audio playback library, such as Rodio, and verify its current API stability, maintenance status, decoding capabilities, seeking behavior, and platform compatibility before adoption.

Keep the audio engine, playlist model, DJ setlist model, metadata system, and user interface separated by clear interfaces.

Avoid unnecessary use of a web-based UI, heavyweight application frameworks, or multiple independent implementations.

Platform-specific code is acceptable where required for native media keys, system integration, audio devices, file handling, and packaging.

Document the reasons for major architectural decisions.

## 3. User experience and design philosophy

The visual direction should combine two influences:

- **Classic Winamp:** compact controls, strong personality, LCD-inspired information displays, dense playlists, tactile buttons, and the feeling of a dedicated music player.
- **DJ software:** clear track information, waveform- or timeline-inspired visualizations where useful, setlist awareness, accurate time displays, and efficient navigation through long mixes.

The result should be a modern interpretation, not a recreation of a specific existing application.

### Visual principles

- One cohesive primary application window.
- A distinctive, fixed visual theme for the initial release.
- Dark graphite or charcoal surfaces with restrained accent colors.
- High information density without sacrificing legibility.
- Clear playback status and immediate visual feedback.
- Compact, purposeful controls.
- Crisp typography and correct high-DPI rendering.
- A playlist that remains a central part of the application.
- Consistent interaction patterns and sensible keyboard navigation.

Avoid generic business-dashboard styling, oversized cards, excessive whitespace, oversized album artwork, and decorative animation that interferes with usability.

The initial release does not require a skin engine, theme marketplace, or elaborate customization system.

However, these principles are guidance rather than rigid rules. The agent may make different UI decisions when they materially improve usability, accessibility, visual quality, or consistency with the product's purpose.

Prefer a convincing, cohesive interface over literal adherence to any reference screenshot.

### Window and layout model

Use one primary application window containing the playback interface and playlist editor.

The layout should adapt gracefully to different window sizes and display scaling settings.

Playback controls and essential track information must remain immediately accessible, while the playlist receives sufficient space to show meaningful track details.

A metadata inspector, setlist view, artwork panel, or other supporting interface may be implemented as an integrated panel, tab, drawer, or dialog when appropriate.

Do not introduce multiple floating windows unless a clear usability benefit justifies the decision.

## 4. Core playback functionality

Implement reliable local audio playback with:

- Play and pause.
- Stop.
- Next and previous track.
- Seeking within a track.
- Elapsed time and total duration.
- Volume adjustment and mute.
- Playback progress.
- Shuffle.
- Repeat current track and repeat playlist.
- Automatic progression to the next track.
- Current-track highlighting.
- Clear handling of missing, unsupported, or corrupted files.
- Recovery from audio-device errors where possible.

Playback state must remain coherent when the user switches tracks, seeks, edits the playlist, changes playback modes, or navigates a DJ setlist.

Do not introduce unnecessary delays when changing tracks.

Ensure that seeking, playback controls, and track transitions remain responsive during metadata scanning, artwork retrieval, and playlist editing.

## 5. Operating-system media keys and media integration

Hardware and system media controls are a core requirement.

Support the standard media controls exposed by the operating system and hardware, including:

- Play/pause.
- Next track.
- Previous track.
- Stop, where supported.

Integrate with the relevant system media-control facilities on macOS, Linux desktop environments, and Windows.

Examples of platform integration to investigate include macOS media command handling and Now Playing integration, MPRIS on Linux, and the Windows System Media Transport Controls APIs.

The application should expose meaningful playback metadata to the operating system, including the current track title, artist, album, artwork when available, and playback state.

For DJ mixes, the system-facing metadata should reflect the currently playing constituent track when that information is known, while retaining an understandable relationship to the underlying mix.

Ensure that media-key presses are handled promptly, including when the application window is not focused.

Prevent duplicate handling when a media key is delivered through more than one supported integration path.

Respect operating-system conventions and coexist reasonably with other media applications.

Where system media controls support seeking or playback position, expose those capabilities when practical.

Do not implement platform integration as a superficial final-stage addition. Design the playback-state model so that system media controls can be integrated cleanly from the start.

## 6. Responsiveness and performance

Responsiveness is a defining product requirement.

The application must feel immediate during normal interaction.

### Requirements

- Keep the UI responsive during file operations, metadata parsing, artwork retrieval, and playlist scans.
- Avoid blocking the UI thread with network access, filesystem traversal, or expensive decoding work.
- Use asynchronous or background processing where appropriate.
- Make play/pause and media-key commands high priority within the application state model.
- Avoid unnecessary audio interruptions when changing track metadata or refreshing artwork.
- Update playback progress smoothly without excessive UI work.
- Avoid full playlist redraws when a single track changes state.
- Use caching for frequently accessed metadata and artwork.
- Cancel or supersede obsolete background requests when their results are no longer needed.
- Avoid race conditions between playback, seeking, and track changes.
- Prevent stale asynchronous results from being applied to the wrong track.
- Use efficient data structures and incremental UI updates for large playlists.

Define measurable responsiveness targets and validate them on real builds. As an initial engineering goal, aim for media-key commands and ordinary UI actions to produce visible or audible feedback within approximately 100 ms under normal conditions, without promising that every audio-device transition can meet that target.

Prioritize reliable playback and predictable state transitions over unnecessary architectural complexity.

## 7. Playlist editor

The playlist editor is one of the application's primary features.

It must be usable for both ordinary music collections and long DJ mixes.

### Playlist operations

- Add individual audio files.
- Add multiple files at once.
- Add folders recursively where appropriate.
- Drag and drop files into the application.
- Reorder entries by dragging.
- Select one or multiple entries.
- Remove selected entries.
- Clear the playlist.
- Move entries up and down.
- Start playback from a selected entry.
- Display useful track information.
- Highlight the current item.
- Import and export M3U and M3U8 playlists.
- Handle missing files and invalid entries gracefully.

Support keyboard shortcuts for common playlist and playback operations.

The playlist must remain responsive when containing hundreds or thousands of entries.

### Playlist persistence

- Restore saved playlists after restarting the application.
- Remember relevant playback preferences and volume.
- Persist playlist ordering.
- Remember window geometry where supported.
- Handle deleted, moved, or unavailable files without crashing.
- Keep ordinary playlists and DJ mix setlists logically distinct.

Support multiple named playlists saved locally, without requiring a full media-library database.

Do not automatically resume playback after restart unless the user explicitly enables this behavior.

## 8. DJ mix playback and setlist intelligence

This is a core differentiating feature and must be included in the initial architecture.

A DJ mix is often one continuous audio file containing multiple musical tracks. The application must understand the relationship between the underlying file and its constituent tracks.

### Tracklist discovery

When a suitable file is opened, attempt to discover its setlist from available local and remote sources.

Investigate sources in this order:

1. Embedded chapter metadata, including supported ID3 CHAP and CTOC frames.
2. Adjacent cue sheets, including matching `.cue` files.
3. Tracklist URLs and identifiers explicitly embedded in metadata or supplied by the user.
4. Online DJ-set listings, including 1001Tracklists where access is permitted and technically reliable.
5. Other suitable online music metadata providers.
6. Previously saved or cached tracklist information.

Use available filename, artist, title, event, date, and other metadata to improve discovery.

Do not assume every long audio file is a DJ mix.

Provide a manual way to search for, import, or associate a tracklist with a file.

Tracklist discovery must never delay the start of playback.

### Tracklist data model

Represent the physical audio file, the overall mix, and its constituent tracks as separate entities.

Each setlist entry should support:

- Track title.
- Artist.
- Remix or mix version.
- Start time.
- Optional end time.
- Optional release or album information.
- Optional artwork.
- Source URL and provenance.
- Confidence or verification status.
- User corrections and overrides.

Support incomplete metadata and unknown tracks.

Validate timestamps, detect duplicate or conflicting entries, and report problems without preventing the mix from playing.

### Current-track identification

During playback, determine which setlist entry corresponds to the current position in the audio file.

At each track boundary:

- Update the displayed constituent track title and artist.
- Highlight the active setlist entry.
- Update relevant artwork and metadata.
- Update system-facing media metadata when appropriate.
- Preserve continuous playback without restarting the file.
- Avoid unnecessary buffering or audio interruptions.

Handle approximate timestamps honestly. Do not imply sample-accurate track boundaries when the source only provides estimates.

Allow the user to correct timestamps and retain those corrections.

### Setlist interaction

Users must be able to:

- View the full setlist.
- Jump directly to a selected entry.
- See the current track highlighted during playback.
- Edit track titles, artists, and timestamps.
- Add, remove, and reorder entries.
- Import and export supported setlist formats.
- Review the source of discovered information.
- Replace an incorrect online match.
- Save corrections locally.
- Refresh online metadata without silently overwriting manual edits.

Provide clear visual differentiation between a DJ mix's overall identity and its currently playing constituent track.

## 9. Online music information and artwork

Online enrichment is an important feature, but the application must remain fully usable offline.

Build a provider-independent lookup layer so that individual services can be added, removed, or replaced without rewriting the player.

Potential sources include:

- 1001Tracklists for DJ mixes and setlists.
- MusicBrainz for recording, artist, and release metadata.
- Cover Art Archive for supported release artwork.
- Other suitable providers with authorized access and appropriate terms.

Verify current API availability, rate limits, attribution requirements, licensing, and terms of service before implementation.

Do not assume that an unofficial endpoint or third-party library will remain available.

### Online lookup behavior

- Search using the strongest available metadata.
- Present ambiguous matches for user selection.
- Show source attribution and links where appropriate.
- Distinguish confirmed information from suggestions.
- Cache successful lookups.
- Avoid repeated unnecessary requests.
- Provide retry and refresh controls.
- Handle network failures gracefully.
- Never require an account or cloud service for basic playback.
- Never block playback while searching online.

### Artwork

Support embedded artwork and online artwork suggestions.

Allow users to inspect available images, select preferred artwork, and optionally embed an image into the audio file through the metadata editor.

Do not silently overwrite existing artwork.

Respect source licensing and attribution requirements. Do not assume that every online image is available for unrestricted redistribution.

## 10. Metadata viewer and editor

Metadata inspection and editing are major product features, not secondary utilities.

Provide a dedicated metadata inspector accessible from the player and playlist.

It should be useful for ordinary music files, DJ mixes, and individual setlist entries.

### File metadata

Display and support editing of available fields, including:

- Title.
- Artist.
- Album.
- Album artist.
- Release date or year.
- Genre.
- Track number and total tracks.
- Disc number and total discs.
- Composer.
- Comment and description.
- Copyright.
- Relevant URL fields.

Support common MP3 ID3 tags and evaluate appropriate metadata support for additional audio formats.

### Technical information

Display available technical properties:

- Audio format and codec.
- Duration.
- Bitrate and constant/variable bitrate status.
- Sample rate.
- Channel count.
- File size.
- Tag versions.
- Embedded artwork.
- Embedded chapter information.

Clearly distinguish editable metadata from read-only properties derived from the audio stream.

### Metadata editing workflow

- Provide explicit Save and Cancel actions.
- Write changes to disk only when requested.
- Preserve audio data when updating tags.
- Preserve unrelated metadata and unknown frames wherever possible.
- Report unsupported fields and failed writes clearly.
- Validate values and handle text encoding correctly.
- Refresh displayed values after successful writes.
- Detect external file modifications where practical.
- Provide a safe recovery or backup mechanism for destructive operations.
- Avoid automatically rewriting metadata during playback.

Allow batch editing of common fields across multiple selected files where practical.

### Artwork editing

Users must be able to:

- View embedded images.
- Import artwork from local files.
- Remove or replace embedded artwork.
- Review online artwork suggestions.
- Choose which artwork to use when several images are available.

Avoid destructive changes without explicit user action.

### DJ mix metadata

Keep overall mix metadata separate from the metadata of constituent tracks.

Editing the DJ or title of the overall mix must not accidentally overwrite individual track artists and titles.

Store timestamps, provenance, and user corrections in a structured form appropriate to the data.

## 11. Audio formats

MP3 is mandatory.

Evaluate support for:

- MP3.
- FLAC.
- WAV.
- Ogg Vorbis.
- AAC/M4A.

Confirm decoder availability, licensing implications, seeking behavior, and platform consistency before committing to each additional format.

Do not allow optional formats to compromise reliable MP3 playback.

## 12. Audio engine and application architecture

Keep the following responsibilities separate:

- Audio playback and seeking.
- Playback-state management.
- Normal playlist management.
- DJ mix and setlist management.
- File metadata reading and writing.
- Online metadata providers.
- Artwork retrieval and caching.
- Operating-system media controls.
- UI presentation.
- Configuration and persistence.

Define clear interfaces between these components.

Use background workers or asynchronous tasks where appropriate, with explicit cancellation and error handling.

Ensure playback commands, seeking, track transitions, and media-key handling cannot leave the application in contradictory states.

Do not overengineer the application. Prefer a straightforward, testable architecture that supports the required features without introducing unnecessary services or abstractions.

## 13. Reliability, security, and privacy

- Never execute code or embedded scripts from media files.
- Treat metadata, URLs, and online responses as untrusted input.
- Validate file paths and external data.
- Avoid unsafe assumptions about malformed tags or cue sheets.
- Handle unsupported or corrupt media gracefully.
- Keep network requests separate from playback.
- Do not upload local music files to online services.
- Do not fingerprint or analyze audio remotely without explicit user consent.
- Store application settings and cached data locally.
- Explain when online lookup is unavailable or a source cannot be accessed.

## 14. Testing requirements

Provide automated tests for:

- Playlist ordering and persistence.
- M3U/M3U8 import and export.
- Cue-sheet parsing.
- Embedded chapter parsing.
- Setlist timestamp validation.
- Current-track identification during playback.
- Metadata reading and writing.
- Artwork handling.
- Playback-state transitions.
- Seeking and track changes.
- Error recovery.
- Asynchronous metadata updates and cancellation.

Test system media keys and media-session integration on actual supported operating systems.

Include manual validation for drag-and-drop, keyboard navigation, window resizing, large playlists, and audio-device changes.

Test long DJ mixes and transitions between constituent tracks.

Do not consider a feature complete merely because it compiles or a mock UI behaves correctly.

## 15. Packaging and distribution

Deliver reproducible builds for macOS and Linux first, then Windows.

Investigate appropriate packaging formats:

- macOS application bundle.
- Linux AppImage and/or native packages.
- Windows executable and installer.

Provide version information, application icons, build instructions, and a concise README.

Document platform-specific dependencies and any limitations.

Verify that the packaged application behaves consistently with development builds.

## 16. Development milestones

### Milestone 1: Technical validation

- Compare GUI frameworks.
- Verify the audio library and format support.
- Test system media controls on each target platform.
- Validate metadata parsing and writing.
- Prototype the intended visual style.

### Milestone 2: Core player

- Implement the single-window interface.
- Add MP3 playback and transport controls.
- Implement the playlist editor.
- Add keyboard shortcuts.
- Integrate operating-system media keys and playback metadata.
- Establish responsive playback-state updates.

### Milestone 3: DJ mix support

- Implement cue-sheet parsing.
- Implement embedded chapter parsing.
- Build the setlist data model.
- Highlight the currently playing constituent track.
- Add setlist editing and seeking.

### Milestone 4: Metadata management

- Implement the metadata inspector.
- Add safe tag editing and artwork management.
- Add persistence and recovery behavior.
- Test metadata preservation.

### Milestone 5: Online enrichment

- Implement provider adapters.
- Add tracklist discovery and source attribution.
- Add artwork lookup and caching.
- Handle ambiguity, offline use, and failed requests.

### Milestone 6: Hardening and release

- Complete automated tests.
- Validate responsiveness and memory use.
- Test long DJ mixes and large playlists.
- Validate media keys on macOS, Linux, and Windows.
- Package and document supported platforms.

Deliver working increments. Do not build the entire application in one untested pass.

## 17. Definition of done

The first production-quality release must allow users to:

1. Play local MP3 files reliably.
2. Use hardware and system media keys even when the application is not focused.
3. Manage and persist multiple playlists.
4. Inspect and edit metadata and embedded artwork safely.
5. Play a single-file DJ mix with a local cue sheet or embedded chapter information.
6. See which constituent track is playing.
7. Navigate directly to a setlist entry.
8. Correct and persist tracklist information.
9. Discover online tracklists and artwork where suitable services are available.
10. Continue using the application without network access.
11. Run the application on macOS and Linux, with Windows support following the same architectural approach.
12. Use the application without unnecessary delays, audio interruptions, or UI freezes.

The finished product should feel like a distinctive, fast, purpose-built desktop music player: the character of classic Winamp, the track awareness of DJ software, and the reliability expected of a modern native application.