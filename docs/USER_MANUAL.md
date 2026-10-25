<p align="center">
  <img src="images/icon.png" width="96" height="96" alt="Nullamp icon">
</p>

# Nullamp user manual

Version 1.7. This manual covers the macOS app. For building from source and the project layout, see the [README](../README.md).

<p align="center">
  <img src="images/nullamp-main.png" alt="Nullamp playing a DJ mix: the player deck on top, the playlist on the left and the setlist on the right" width="900">
</p>

## Contents

1. [What Nullamp is](#1-what-nullamp-is)
2. [Getting started](#2-getting-started)
3. [A tour of the window](#3-a-tour-of-the-window)
4. [Playing music](#4-playing-music)
5. [Playlists](#5-playlists)
6. [DJ mixes and setlists](#6-dj-mixes-and-setlists)
7. [Editing tags and artwork](#7-editing-tags-and-artwork)
8. [rekordbox and Mixxx](#8-rekordbox-and-mixxx)
9. [Settings](#9-settings)
10. [Keyboard shortcuts](#10-keyboard-shortcuts)
11. [Where Nullamp keeps your data](#11-where-nullamp-keeps-your-data)
12. [Troubleshooting](#12-troubleshooting)
13. [Known limitations](#13-known-limitations)

## 1. What Nullamp is

Nullamp plays the music files on your disk. It is built for two things:

- **Fast, plain playback.** Files open in a few tens of milliseconds, seeking is instant, and nothing you do blocks the window.
- **DJ mixes.** A mix is one long audio file that contains many tracks. Nullamp reads the track list (the *setlist*) and always shows which track is playing right now.

It also has a metadata editor for tags and artwork that edits files safely, and it can read the libraries of rekordbox and Mixxx.

Nullamp is not a streaming service and not a library manager. It never moves, renames or scans your music folders on its own.

**Supported formats:** MP3, AAC and M4A, ALAC, FLAC, Ogg Vorbis, WAV and AIFF.

## 2. Getting started

### Install

1. Download the latest `Nullamp` zip from the [Releases](https://github.com/jbrunemann/nullamp/releases) page and unzip it.
2. Drag `Nullamp.app` to your Applications folder.
3. The app is not signed or notarised yet. The first time, right-click `Nullamp.app`, choose **Open**, and confirm.

### Add music

Do any of the following:

- Drag files or folders from Finder onto the window. They land where you drop them.
- Click **Add files…** (⌘O) or **Add folder…** (⇧⌘O) above the playlist.
- Use **List ▸ Import M3U / M3U8…** to load an existing playlist.
- Use the **List** menu to add playlists from rekordbox or Mixxx (see [section 8](#8-rekordbox-and-mixxx)).

Double-click a row, or select it and press Enter, to play it.

## 3. A tour of the window

The window has four areas, from top to bottom.

### The deck

The black display at the top is the *LCD*.

| Element | What it shows |
|---|---|
| Large green digits | Elapsed time in the current track. Click them to switch between elapsed and remaining time. |
| Spectrum below the digits | A live visualiser. Click it to cycle between spectrum, oscilloscope and off. The picture is aligned to what you hear, not to what was just decoded. |
| Orange title | The track that is playing. For a mix, this is the track *inside* the mix. |
| Green line under it | For a mix: `MIX ▸ mix title, DJ`. For a single track: artist and album. |
| `BPM` frame | The tempo. It comes from the file's tags, or from rekordbox or Mixxx. Without either, Nullamp listens to the audio and shows its estimate with a `~` in front. For a mix it shows the tempo of the track inside the mix. `---` means the file's BPM tag is `-1`, which switches the tempo off for that file. |
| `FILE 1/2   TRACK 3/6` | Position in the playlist, and for a mix the position in the setlist. |
| `OUT` and a device name | The audio output. Click it to choose another device. |
| Bottom line | Format, bit rate, sample rate, channels and the file name. Hover the file name for the full path. |
| `SHUF  RPT  MUTE` | Lit when shuffle, repeat or mute is on. |

Right-click the display to open the settings (see [section 9](#9-settings)).

### The artwork

The square to the right of the LCD shows the artwork of the playing file. While a mix plays, it shows the cover of the track inside the mix (see [Covers of the tracks in a mix](#covers-of-the-tracks-in-a-mix)). A change of cover is animated.

Click the artwork to cycle between three looks: **dithered** (the default, in the style of the LCD), **original** and **off**. The choice is kept across tracks and restarts. A file without any artwork shows a generated picture instead, and clicking it opens the Inspector.

### The timeline

Under the LCD is the seek bar. Click or drag to seek.

Behind the bar is a waveform of the whole file: the dim outline is the peak level and the brighter body the average loudness, scaled so the loudest point fills the bar. It appears a moment after a track starts, drawing from left to right (a long mix takes a second or two the first time and is instant afterwards), so you can spot breaks, builds and drops before you get there. Tracks known to rekordbox or Mixxx use that program's coloured waveform instead (see [section 8](#8-rekordbox-and-mixxx)).

When the playing file is a mix, the timeline is divided into one segment per setlist entry. Thin vertical lines mark the track boundaries and the current track is lit in amber. Solid boundaries are exact. Dashed boundaries are estimates (see [Approximate times](#approximate-times)). Hover a segment to see the track name and its start time. The numbers at the ends are elapsed and total time.

### The transport

| Control | Action |
|---|---|
| ⏮ ⏵ ⏸ ⏹ ⏭ | Previous, play, pause, stop, next |
| Shuffle | Toggle shuffle |
| Repeat | Cycle repeat modes |
| Speaker and green bars | Mute, and volume. Click the bars or scroll over them. |
| **DIM** | Dims everything except the LCD and the artwork. Click again for darker steps, then off. The D key does the same. |
| **Playlist**, **Setlist**, **Inspector** | Show or hide the playlist and the two side panels |

The **Setlist** button carries the number of entries (`Setlist · 6`). It only appears for a mix.

### Playlist and side panel

The left area holds your playlists. The right area is the side panel, which shows either the **Setlist** or the **Inspector**. Drag the divider between them to resize. The `×` closes the side panel.

The status bar at the bottom shows track counts, total time and messages.

## 4. Playing music

- **Play, pause, stop, previous, next:** the transport buttons, or Space (play and pause), and Z X C V B. See [Keyboard shortcuts](#10-keyboard-shortcuts).
- **Seek:** click or drag the timeline (the seek happens when you let go), or press ← and → to move 5 seconds.
- **Volume:** the green bars, or + and −. M mutes.
- **Shuffle and repeat:** S and R, or the buttons.
- **Gapless:** the next track is prepared while the current one plays, so albums and consecutive mixes play without a gap.
- **Media keys:** the hardware play, pause and skip keys work even while Nullamp is in the background. The macOS Now Playing widget shows the current track. For a mix, it shows the track inside the mix as the title, "mix title, DJ" as the album, and the track's cover when one was found.
- **Output device:** click `OUT` on the LCD, or open **Settings ▸ Audio device**, to choose where sound goes. Your choice sticks: if the device is unplugged, Nullamp plays on the system default and returns to your device when it comes back.
- **Device changes:** if you plug in headphones, connect Bluetooth speakers, or wake the Mac from sleep, Nullamp recovers and continues from the same position.
- **Resume:** by default Nullamp starts silent. It does not resume the last track unless the `resume_playback` setting is on (see [Settings file](#settings-file)).

## 5. Playlists

### Tabs

Each playlist is a tab. The tab with a `▶` is the one that is playing.

- **+** creates a playlist.
- Double-click a tab to rename it.
- Right-click a tab for **Rename…**, **Export as M3U8…** and **Delete playlist…**.

Playlists are saved automatically and are restored the next time you start Nullamp. A playlist holds *references* to your files. Deleting a playlist never deletes audio.

### The table

| Action | How |
|---|---|
| Select | Click. ⇧-click or ⇧↑/⇧↓ extends, ⌘A selects all. |
| Play | Double-click, or Enter |
| Reorder | Drag rows. A line shows where they will land. Or use **▲ ▼**, or ⌥↑ and ⌥↓. |
| Copy to another playlist | Drag the rows onto that playlist's tab, or right-click ▸ **Copy to playlist**. They are added at the end and stay in this playlist too. |
| Remove | **Remove**, or Delete or Backspace |
| Right-click menu | **Play**, **Inspect metadata**, **Show in folder**, **Copy path**, **Copy to playlist**, **Move up**, **Move down**, **Remove** |
| Find the playing track | **◎ Now playing**, or ⌘J. This clears a filter that hides it. |
| Filter | Type in the **Filter** field at the right of the toolbar, or press ⌘F. Only tracks whose title, artist, album or file name contain every word you type stay in the table. Each tab shows how many of its tracks match, so you can see which playlist has what you are looking for; the filter stays when you switch tabs. Enter moves to the list, Esc in the field clears it. While a filter is on, rows cannot be reordered (dragging them onto another tab still copies them), and ⌘A, ⇧-click and the arrow keys only take the shown rows. Adding files clears the filter. |

Missing files stay in the list, greyed out with a warning, so you can find and fix them. **List ▸ Remove unavailable entries** removes them all.

### Columns

The available columns are number, artwork (a small thumbnail), title, artist, album, year, format, bitrate, BPM, key and time. Year and bitrate are hidden at first. Right-click the header to show or hide columns, drag a header to reorder them, and choose **Reset columns** to go back to the default.

The **BPM** column shows the tempo from the tags. A tempo Nullamp detected while playing is shown with a `~` and kept with the playlist. The **KEY** column is coloured by its position on the Camelot wheel, so keys that mix well look alike. Keys are shown as Camelot codes (`8A`) unless the `camelot` setting is off, which shows names (`Am`).

### Toolbar menus

- **Sort:** by title, artist, album, file path, bitrate, BPM, key or duration, and **Reverse**. By key follows the Camelot wheel.
- **List:**
  - **Open rekordbox USB**, **Open rekordbox collection…**, **Open Mixxx library…** and **Read metadata from DJ software** (see [section 8](#8-rekordbox-and-mixxx)).
  - **Import M3U / M3U8…** and **Export as M3U8…**.
  - **Select all**, **Remove unavailable entries** and **Rescan tags** (re-read tags for every entry).
  - **Clear playlist…**.

### Hiding the playlist

Click **Playlist** (or press ⌘P) to hide the table. With the side panel closed too, the window shrinks to just the player and the status bar. It restores its size when you bring the playlist back.

## 6. DJ mixes and setlists

A **setlist** is the list of tracks inside one long file, each with a start time. Nullamp keeps setlists per audio file, and they never alter the audio.

### How Nullamp finds a setlist

When a file is loaded, Nullamp looks for a track list in this order and uses the first one it finds:

1. A cue sheet next to the audio with exactly the same name (`mix.mp3` and `mix.cue`).
2. ID3 chapters embedded in the file (MP3, WAV and AIFF).
3. A cue sheet stored inside a tag, such as a CUESHEET item or a comment that contains a cue sheet. The screenshot at the top shows this case: the panel says "Cue sheet (embedded in tags) · exact timing".
4. Any other cue sheet in the same folder that refers to the file.
5. A plain numbered list in the comment or description tag. This has no times, so it is shown as a reference list only.

Cue sheets from rekordbox, where INDEX times are written as `hh:mm:ss`, are understood.

A file counts as a mix when it has a setlist of at least two entries, or when it is longer than 20 minutes, or when you create a setlist for it. For single tracks the Setlist button and the mix lines on the LCD stay hidden, and the Inspector is shown instead. Press ⌘L on a single track if you do want to add a setlist.

### Reading the setlist panel

Open **Setlist** in the side panel.

- The top card shows `MIX`, the mix title, DJ and year. The playing track is shown on the LCD and highlighted in the list. Edit the mix title, artist and date in the Inspector, like any other tag.
- A line below the buttons tells you how many tracks there are, where the list came from, and whether the timing is exact or approximate.
- The list shows each entry's number, start time, title and artist. The current entry is highlighted in amber.
- Any problem, such as entries out of order or beyond the end of the file, is listed above the entries.

**Double-click** an entry to jump to it. Press ⇧← and ⇧→ to go to the previous or next entry while listening.

### Covers of the tracks in a mix

When a track inside a mix starts playing for the first time, Nullamp looks up its cover online by artist and title (Deezer first, then iTunes). This is one of only three things in Nullamp that use the network; the others are the update check and [Identify with Shazam](#identifying-tracks-with-shazam). Each track is looked up once and the result is cached, so the same track in another mix gets the same cover. Entries without an artist or title, and "ID - ID" entries, are not looked up.

While the lookup runs, the deck shows the mix file's own cover. When nothing is found, it shows a generated retro landscape, which looks the same for that track every time. To switch the lookup off, set `online_artwork` to `false` (see [Settings file](#settings-file)).

### Correcting a setlist

Right-click an entry for **Edit entry**, **Set start to playhead**, **Identify with Shazam** and **Delete entry**. In the editor you can change the title and artist, type a time, or press **→ playhead** to use the current playback position. The **exact** checkbox says whether the time is trustworthy.

To fix a time by ear: play to the moment the new track starts, right-click that entry, and choose **Set start to playhead**. The boundary becomes exact.

To add a missing track, play to where it starts and click **+ At playhead**.

Your corrections are saved with the setlist. They survive renaming, moving or retagging the audio file, because Nullamp identifies the audio by a fingerprint of its content, not by its name. **Import ▸ Scan file again** re-reads the file's chapters and cue sheets while keeping your corrections.

### Finding where the tracks change

**Find tracks** in the setlist toolbar estimates from the audio where each track of the mix starts. It works offline and takes a few seconds; a spinner shows while it runs, and **Cancel** next to it stops it. It is offered while the setlist has no times: when there is no setlist yet, or when it is only a track list, such as one written in the file's comment tag.

- **With a track list** (say, the tracklist of the mix pasted into the file's comment tag), the number of tracks is known, and each listed track gets its start in list order. This is the accurate way: knowing how many tracks there are matters most.
- **Without a list**, Nullamp also guesses how many tracks there are and adds an unnamed entry for each. The guess can be off by several tracks in mixes whose tracks sound alike, so add or delete entries where it is wrong, and name them yourself or with [Identify with Shazam](#identifying-tracks-with-shazam).

The times are estimates, shown with **≈**. Correct one by playing to where the track takes over and choosing **Set start to playhead**. A change is placed where the next track has taken over, not where it first fades in. Times found for a list from the comment tag stay when you later edit the tag.

### Identifying tracks with Shazam

For a mix of your own that has no tracklist anywhere, mark where each track starts and let Shazam name the tracks one by one:

1. Play the mix, and at the start of each track click **+ At playhead**. Leave artist and title empty.
2. Right-click an entry and choose **Identify with Shazam**. Nullamp takes five 12-second clips from the middle of that track, away from the blends, and asks Shazam about each one, five seconds apart, so it takes about half a minute. The entry shows a spinner and how many clips have been asked about; **Cancel Shazam** in its right-click menu stops it, and so does playing another file. One track is asked about at a time.
3. The track is named only when at least two of its clips name the same song, at positions that fit how the mix moves through it. Single clips are often wrong, so this keeps chance answers out. The entry then shows artist and title as a suggestion, with Shazam as its source, and the status bar says what was found.
4. When the clips disagree, the entry keeps its text and gets an orange **?**. Edit the entry to see the songs Shazam named; **Use** puts one into the fields, **open** shows its page on shazam.com, and **Save** keeps it.

If you typed the entry's artist or title yourself, Shazam's answer does not replace them: it is offered in the editor, as for an unsure track. An entry from a cue sheet or chapters that you have not edited is filled in, so you can also use this to check a tracklist.

**Settings ▸ Logs** shows what happened: the answer for each clip (with where the clip sits in the song and how much faster or slower the mix plays it), and whether the clips agreed.

Only a signature of each clip is sent, a list of its loudest frequencies, not the audio. Nullamp uses the address the Shazam apps use, which Shazam does not offer for other programs: it can stop working at any time, and Shazam may refuse requests for a while if there are many. Shazam often knows only one version of a track (a vocal version for an instrumental, say), and it cannot know unreleased tracks or your own edits.

### Importing a setlist

Click **Import**:

- **Cue sheet or text file…** reads a `.cue` file or a text tracklist.
- **Paste tracklist…** parses a list you copied from a web page, a video description or a comment. Timestamps like `12:34` or `1:02:03` are recognised, as are numbered lines and `Artist - Title` pairs. You can keep the page address as the *source*.
- **Scan file again** as described above.

Websites such as 1001Tracklists and mixid.fm do not offer a public interface, and Nullamp does not scrape them. Open the page in your browser, copy the list, and use **Paste tracklist…**.

#### Approximate times

Lists copied from the web often have times that are only roughly right. Those entries are marked with **≈** and their boundaries are dashed on the timeline. Correct them as described above and they become exact.

### Exporting a setlist

Click **Export**:

- **Cue sheet…** writes a `.cue` file.
- **Text tracklist…** writes a plain text list with timestamps.
- **Copy as text** puts the list on the clipboard.
- **Write as MP3 chapters…** stores the setlist inside the MP3 itself as ID3 chapters, so other players can see it. This replaces any chapters already in the file. Other tags and the audio are kept, and a backup is made if backups are on.
- **Write cue sheet into the comment…** is for a setlist you have finished correcting. It replaces the file's comment with a cue sheet of the setlist (start times, artists, titles, and the mix's title and DJ), so the list travels with the file and Nullamp reads it back wherever the file is played. Every entry needs a start time; the item stays greyed out until they all have one. Before writing, Nullamp says how many entries are not confirmed and how many start times are still estimates (≈). Whatever the comment held before, such as the original track list, is replaced. Other tags and the audio are kept, and a backup is made if backups are on. Times are stored in cue sheet frames (1/75 s). Versions are written as part of the title, and labels are not stored. When a file also has two or more MP3 chapters, the chapters are read first.

**Clear…** removes all entries for the file, including your corrections. It asks first.

## 7. Editing tags and artwork

Open the **Inspector** (the button, or ⌘I). It shows the selected file in the playlist. Tick **Follow playing track** to make it show whatever is playing instead.

### Fields

Title, artist, album, date or year, genre, BPM, key, track number and total, and comment. Change what you like and press **Save**. **Cancel** discards your edits, and **Reload** reads the file again.

- **BPM:** when the field is empty and Nullamp has detected the tempo of the playing file, a **Detected N BPM: use it** button fills it in. MP3 and M4A store whole numbers only. Enter `-1` to switch tempo display and detection off for a file.
- **Key:** type it as a Camelot code (`8A`), an Open Key code (`1m`) or a name (`Am`, `F# minor`, `Bb`). It is written as you type it.

### Several files at once

Select up to 500 files and open the Inspector. A field that differs between the files shows `‹multiple values›`. Only the fields you touch are written, so you can set the album on a whole folder without changing anything else.

### Artwork

The artwork section lists every embedded image with its thumbnail on the left and its type, size and weight beside it.

- **Import…** adds an image and **Remove** deletes one.
- **Find online** looks up a cover by the artist and title in the form, using the same services as the mix track covers. The image is added as type "Other" and is only written when you press **Save**. It is not available for several files at once.
- **Set as front cover** tags an image as the front cover and moves it to the first position. The front cover is the image other players show as album art, so use it when an image is labelled "Other" and some apps do not display it. Nullamp itself falls back to the first image when none is tagged as the front cover. The button only appears on images that are not already the front cover.

With several files selected, changing artwork changes it in all of them.

### Read-only information

Below the fields you will find:

- **Technical**: format, bit rate, sample rate, channels, duration.
- **From DJ software**: what rekordbox or Mixxx knows about the file, such as key, BPM, rating and colour (see [section 8](#8-rekordbox-and-mixxx)).
- **Embedded chapters** and **Cue sheet in tags**, if the file has them.
- **Other tag items**: frames Nullamp does not edit. They are preserved when saving.

### Safe saving

Nullamp never edits your file in place. Each save:

1. Checks that the file did not change on disk since you opened it.
2. Writes the new tags into a hidden copy.
3. Reads the copy back and checks the tags, the duration and the sample rate.
4. Optionally keeps a backup of the original.
5. Swaps the copy in atomically.

If anything fails, your original is untouched. Chapters, artwork you did not change, and unknown frames survive. The audio itself is never rewritten. With backups on, you can click **Restore it** after a save to put the previous version back.

Backups are off by default; turn them on with `backup_before_tag_writes` (see [Settings file](#settings-file)). They are stored in the data folder (see [section 11](#11-where-nullamp-keeps-your-data)), with a size budget of 256 MB by default. A file larger than half the budget is not backed up. The oldest backups go first.

## 8. rekordbox and Mixxx

Nullamp can add playlists from rekordbox and Mixxx, and use what those programs know about your tracks. It only reads their libraries and never changes them.

### rekordbox USB sticks

Plug in a USB stick exported by rekordbox and choose **List ▸ Open rekordbox USB**. Each stick becomes a playlist named after the volume, with title, artist, album, duration, bitrate, BPM and key filled in from rekordbox's database. Opening it again refreshes that playlist.

### The rekordbox desktop library

The library of the rekordbox app on your Mac is encrypted, and Nullamp needs its key once:

1. Quit rekordbox.
2. Open **Settings ▸ rekordbox** and click **Copy key command**. The tab tells you whether a Python 3 is installed, which the command needs.
3. Paste the command into Terminal and run it. It installs a small tool in a private folder and prints a 64-character key.
4. Paste the key into the **master.db key** field.

Then choose **List ▸ Open rekordbox collection…**. A dialog lists your playlists and folders. Tick **Entire collection** for one list with every track, or tick the playlists you want; ticking a folder ticks everything in it. Click **Add**.

rekordbox's bundled samples, effect sounds and demo tracks are left out. Streaming tracks (from Spotify and the like) are added but show as unavailable, because there is no file to play.

### The Mixxx library

Choose **List ▸ Open Mixxx library…**. The same kind of dialog lists your crates, playlists and history sessions. Nullamp finds the library in Mixxx's own folder; you cannot point it elsewhere. Mixxx's library is only read after you open it this way the first time.

If macOS refuses access (the Mac App Store version of Mixxx keeps its library in a protected folder), allow Nullamp under **System Settings ▸ Privacy & Security ▸ Full Disk Access**.

### What Nullamp uses from them

- **Key, BPM, year and the like** fill in what a file's own tags lack. The tags always win where they have a value.
- **Waveforms:** the timeline shows rekordbox's or Mixxx's coloured waveform instead of Nullamp's own. Turn this off with **Use rekordbox's and Mixxx's waveform analysis** on the rekordbox tab.
- **Rating and colour** are shown in the Inspector under **From DJ software**.

This works for any file the programs know, wherever you add it from. Nullamp also caches what it learned by the content of the file, so a copy of the file on another disk keeps its key, BPM and waveform even when the stick is away or the program is closed.

To cache this for a whole playlist ahead of time, choose **List ▸ Read metadata from DJ software**. The import dialogs do the same when **Also read waveforms, keys and ratings** is ticked. Progress shows in the status bar.

## 9. Settings

Right-click the LCD and pick a page. The settings page has these tabs:

| Tab | What it holds |
|---|---|
| **General** | Appearance: dark, light, or follow macOS. The LCD stays dark in every theme. |
| **Audio device** | The output device, as on the `OUT` label. |
| **Shortcuts** | The keyboard shortcuts. F1 opens it. |
| **rekordbox** | The waveform setting and the desktop library key (see [section 8](#8-rekordbox-and-mixxx)). |
| **Updates** | Your version and **Check for updates**. Nullamp only contacts the update address when you press the button. |
| **Cache** | What the cache holds, by kind, with a **Clear** button for each, **Clear all** and **Show in Finder**. Everything is made again when needed, except data from a rekordbox stick that is not plugged in. |
| **Logs** | The last 300 lines of Nullamp's log, with **Copy**. Useful when reporting a problem. |
| **About** | Version and licence. The **Nullamp ▸ About** menu opens it too. |

### Settings file

Nullamp writes `settings.json` in the config folder (see [section 11](#11-where-nullamp-keeps-your-data)). **Nullamp ▸ Open Config File** opens it in your text editor. Quit the app before editing it by hand, or your changes may be overwritten. Useful keys:

| Key | Meaning |
|---|---|
| `resume_playback` | Resume the last track at start. Default off. |
| `backup_before_tag_writes` | Keep a backup on every tag write. Default off. |
| `backup_budget_mb` | Size budget for backups. Default 256. |
| `online_artwork` | Look up covers of the tracks inside mixes, and allow **Find online**. Default on. |
| `camelot` | Show keys as Camelot codes (`8A`) rather than names (`Am`). Default on. |
| `appearance` | `dark`, `light` or `system`. |
| `visualizer` | `spectrum`, `scope` or `off`. |

## 10. Keyboard shortcuts

Press F1, or right-click the display and choose **Keyboard shortcuts**, to see this list in the app.

| Key | Action |
|---|---|
| Space or C | Play or pause |
| X | Play |
| V | Stop |
| Z or B | Previous or next track |
| ← or → | Seek 5 seconds |
| ⇧← or ⇧→ | Previous or next setlist entry |
| + or − | Volume up or down |
| M | Mute |
| S | Shuffle |
| R | Cycle repeat |
| D | Dim everything but the display and artwork |
| ↑ ↓, Home, End | Move the selection (⇧ extends it) |
| Enter | Play the selected entry |
| Delete or Backspace | Remove the selected entries |
| ⌥↑ or ⌥↓ | Move the selected entries |
| ⌘A | Select all |
| ⌘O | Add files |
| ⇧⌘O | Add folder |
| ⌘I | Inspector |
| ⌘L | Setlist |
| ⌘P | Show or hide the playlist |
| ⌘J | Scroll to the playing track |
| ⌘F | Filter the playlist |
| F1 | Keyboard shortcuts |

The letter keys do not fire while you type in a text field.

## 11. Where Nullamp keeps your data

| What | Where |
|---|---|
| Settings, playlists, saved setlists | `~/Library/Application Support/Nullamp/` |
| Tag-write backups | `~/Library/Application Support/Nullamp/tag-backups/` |
| Artwork thumbnails and mix track covers | `~/Library/Caches/Nullamp/artwork/`. Covers found online are kept 30 days, tracks with nothing found 7 days. |
| Waveforms, tempos and DJ software data | `~/Library/Caches/Nullamp/waveforms/`. A few KB per file. Entries not used for 90 days are removed when Nullamp starts. |

Everything under `~/Library/Caches/Nullamp/` is safe to delete, and the **Cache** tab in the settings does it for you.

Set the environment variable `NULLAMP_HOME` to a folder before launching to keep everything in one place, for example on a USB stick or for testing: `NULLAMP_HOME=/some/dir open -a Nullamp`.

If a data file cannot be read, Nullamp renames it to `.broken`, tells you, and starts fresh with that file.

## 12. Troubleshooting

**The first launch says the app cannot be opened.** The app is unsigned. Right-click it, choose **Open**, and confirm once.

**No sound, or sound on the wrong device.** Click `OUT` on the LCD and choose the device. If you never picked one, Nullamp follows the macOS output device in System Settings ▸ Sound within a moment.

**A file is greyed out with a warning.** The file moved or its volume is not mounted. Reconnect the volume, or remove the entry.

**A mix shows no setlist.** The file has no chapters or cue sheet that Nullamp could find. Put a `.cue` file next to it with exactly the same name, or use **Import ▸ Paste tracklist…**, or let **Find tracks** estimate where the tracks change, or add entries yourself with **+ At playhead**, and right-click them for **Identify with Shazam**.

**Identify with Shazam is greyed out.** The entry has no start time, or Shazam is still being asked about another entry. Wait for it, or choose **Cancel Shazam** on that entry.

**Identify with Shazam stops with "Shazam refuses more requests".** Shazam limits how often it answers. Wait a few minutes and try again.

**Shazam is often unsure.** The clips may fall into long blends, or the track is unknown to Shazam. **Settings ▸ Logs** shows each clip's answer. Move the start of the entry, or of the next one, closer to where the tracks really change, or edit the entry and pick from Shazam's answers.

**Times in a mix are a little off.** Seek to the true start, then use **Set start to playhead** on that entry.

**"Changed on disk" when saving tags.** Another program modified the file after you opened it. Press **Reload**, then redo your edit.

**A save went wrong.** Press **Restore it** right after saving, or find the original in the tag-backups folder.

**Open rekordbox collection is greyed out.** It needs the key on **Settings ▸ rekordbox** first.

**The rekordbox key does not work.** Quit rekordbox before running the key command, and paste the whole 64-character result.

**Mixxx tracks all show as unavailable.** Mixxx stores the full path of each file. A library copied from another computer points to paths that do not exist on this Mac.

**Mix track covers do not appear.** The Mac is offline, or the services do not know the track. Nullamp tries again a minute later, or when the track becomes active again. Check that `online_artwork` is not off.

## 13. Known limitations

- Seeking far into a variable bit rate MP3 that has no seek table is approximate. Constant bit rate files, which covers most DJ mixes, seek exactly.
- The setlist panel follows the file that is playing. You cannot edit the setlist of a file that is not playing.
- Batch tag editing is limited to 500 files.
- Tracklists come only from chapters, cue sheets, paste import or Shazam. Nullamp does not look them up online.
- Identify with Shazam needs the start of each track; **Find tracks** estimates them, best when the number of tracks is known from a track list.
- Nullamp does not detect musical keys; it only shows keys from tags, rekordbox or Mixxx.
- Cues, hot cues and beat grids from rekordbox and Mixxx are not read.
- "Open with Nullamp" from Finder is not wired up yet. Drag files onto the window instead.
- macOS is the only supported platform for now.
