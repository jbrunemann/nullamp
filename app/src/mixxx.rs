//! The Mixxx library: its crates, playlists and play history as playlists, and the key, BPM and
//! rating, colour and waveform it stores for any file the user plays. Parallels the desktop half of
//! `rekordbox.rs` (`fill`, `data` and `waveform`, so the same cache and the same bulk read serve
//! both), without a key: Mixxx's database is a plain file. Everything here runs on worker threads.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use nullamp_core::{Key, TrackInfo};
use nullamp_mixxx::{Kind, Library, Track};

use crate::rekordbox::{Analysed, Data, Item, Loaded, Overview, fill_gaps};

fn info(t: &Track) -> TrackInfo {
    let text = |s: &str| Some(s.to_owned()).filter(|s| !s.is_empty());
    TrackInfo {
        title: text(&t.title),
        artist: text(&t.artist),
        album: text(&t.album),
        year: t.year,
        duration: (t.duration_secs > 0.0).then_some(t.duration_secs),
        bitrate: (t.bitrate > 0).then_some(t.bitrate),
        bpm: t.bpm,
        key: t.key.as_deref().and_then(Key::parse),
        ..TrackInfo::default()
    }
}

/// The files Mixxx keeps its library in on this platform, in the order they are tried. The
/// sandboxed macOS build keeps it in its container, which macOS only lets another app read once
/// the user has allowed it.
fn default_locations() -> Vec<PathBuf> {
    let Some(dirs) = directories::BaseDirs::new() else { return Vec::new() };
    let folders = if cfg!(windows) {
        vec![dirs.data_local_dir().join("Mixxx")]
    } else if cfg!(target_os = "macos") {
        vec![dirs.data_dir().join("Mixxx"), dirs.home_dir().join("Library/Containers/org.mixxx.mixxx/Data/Library/Application Support/Mixxx")]
    } else {
        vec![dirs.home_dir().join(".mixxx")]
    };
    folders.into_iter().map(|f| f.join("mixxxdb.sqlite")).collect()
}

/// The parsed library with its tracks indexed by file path, as of the database's modification time.
struct Index {
    library: Library,
    by_path: HashMap<String, usize>,
}

impl Index {
    fn new(library: Library) -> Index {
        let by_path = library.tracks.iter().enumerate().map(|(i, t)| (nullamp_rekordbox::path_key(&t.file), i)).collect();
        Index { library, by_path }
    }
}

const FOLDERS: [(Kind, &str, &str); 3] = [(Kind::Crate, "crates", "Crates"), (Kind::Playlist, "playlists", "Playlists"), (Kind::History, "history", "History")];

/// The id the chooser knows a list by.
fn item_id(kind: Kind, id: i64) -> String {
    let (_, prefix, _) = FOLDERS.iter().find(|f| f.0 == kind).unwrap();
    format!("{prefix}/{id}")
}

/// The chooser's tree: a folder for each kind of list that exists, holding its lists.
fn overview_of(index: &Index) -> Overview {
    let lib = &index.library;
    let mut items = Vec::new();
    for (kind, id, name) in FOLDERS {
        let lists: Vec<&nullamp_mixxx::List> = lib.lists.iter().filter(|l| l.kind == kind).collect();
        if lists.is_empty() {
            continue;
        }
        items.push(Item { id: id.into(), name: name.into(), depth: 0, is_folder: true, tracks: 0 });
        items.extend(lists.into_iter().map(|l| Item { id: item_id(kind, l.id), name: l.name.clone(), depth: 1, is_folder: false, tracks: l.tracks.len() }));
    }
    Overview { tracks: lib.tracks.len(), items }
}

/// The name a list gets as a playlist. A name a list is found by, and one of that name is replaced,
/// so a session of the history is named "History / 2019-06-24" (its own name is only a date), and a
/// crate and a playlist with one name get their kind in front.
fn list_names(lists: &[&nullamp_mixxx::List]) -> Vec<String> {
    let mut taken: HashMap<&str, usize> = HashMap::new();
    for l in lists {
        *taken.entry(l.name.as_str()).or_default() += 1;
    }
    lists
        .iter()
        .map(|l| {
            let (_, _, folder) = FOLDERS.iter().find(|f| f.0 == l.kind).unwrap();
            if l.kind == Kind::History || taken[l.name.as_str()] > 1 { format!("{folder} / {}", l.name) } else { l.name.clone() }
        })
        .collect()
}

fn lists_of(index: &Index, all: bool, ids: &HashSet<String>) -> Vec<Loaded> {
    let lib = &index.library;
    let entry = |&i: &usize| (lib.tracks[i].file.clone(), info(&lib.tracks[i]));
    let mut out = Vec::new();
    if all {
        out.push(Loaded { name: "Mixxx".into(), tracks: Ok((0..lib.tracks.len()).map(|i| entry(&i)).collect()) });
    }
    let picked: Vec<&nullamp_mixxx::List> = lib.lists.iter().filter(|l| ids.contains(&item_id(l.kind, l.id)) && !l.tracks.is_empty()).collect();
    for (l, name) in picked.iter().zip(list_names(&picked)) {
        out.push(Loaded { name, tracks: Ok(l.tracks.iter().map(entry).collect()) });
    }
    out
}

/// The Mixxx library, indexed by file path so what Mixxx knows fills any file the user plays,
/// wherever it came from. Only Mixxx's own folder is read, and only once the user has opened the
/// library; until then it holds nothing.
pub struct Mixxx {
    /// Where the library may be, in the order tried.
    locations: Vec<PathBuf>,
    enabled: AtomicBool,
    /// The library as read when its file had this modification time, or why it could not be.
    loaded: Mutex<Option<(Option<SystemTime>, Result<Arc<Index>, String>)>>,
    /// The last "cannot find or reach the library" that was logged, so a scan that asks for every
    /// file does not repeat it.
    reported: Mutex<Option<String>>,
}

impl Default for Mixxx {
    fn default() -> Mixxx {
        Mixxx::at(default_locations())
    }
}

impl Mixxx {
    fn at(locations: Vec<PathBuf>) -> Mixxx {
        Mixxx { locations, enabled: AtomicBool::new(false), loaded: Mutex::new(None), reported: Mutex::new(None) }
    }

    /// Turns the reader on (the user has opened the Mixxx library) or off.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// The library file and its modification time. macOS answers "not permitted" for a container
    /// the user has not allowed us into, which is told apart from there being no library.
    fn locate(&self) -> Result<(&Path, Option<SystemTime>), String> {
        let mut denied = None;
        for path in &self.locations {
            match std::fs::metadata(path) {
                Ok(m) if m.is_file() => return Ok((path, m.modified().ok())),
                Err(e) if e.kind() == ErrorKind::PermissionDenied => denied = denied.or(Some(path)),
                _ => {}
            }
        }
        Err(match denied {
            Some(path) => format!("macOS does not let Nullamp read {} (allow it under System Settings > Privacy & Security > Full Disk Access)", path.display()),
            None => format!("No Mixxx library found, looked for {}", self.locations.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" and ")),
        })
    }

    /// The parsed library, read again when the file has changed. `Err` when it is not turned on,
    /// not there or will not open.
    fn index(&self) -> Result<Arc<Index>, String> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Err("The Mixxx library has not been opened".into());
        }
        let (db, modified) = match self.locate() {
            Ok(found) => found,
            Err(e) => {
                let mut reported = self.reported.lock().unwrap();
                if reported.as_ref() != Some(&e) {
                    log::warn!("Mixxx: {e}");
                    *reported = Some(e.clone());
                }
                return Err(e);
            }
        };
        self.reported.lock().unwrap().take();
        let mut loaded = self.loaded.lock().unwrap();
        if let Some((at, result)) = loaded.as_ref()
            && *at == modified
        {
            return result.clone();
        }
        let started = std::time::Instant::now();
        let result = Library::open(db).map(|lib| Arc::new(Index::new(lib))).map_err(|e| format!("{}: {e}", db.display()));
        match &result {
            Ok(index) => {
                let tracks = &index.library.tracks;
                let (waveforms, colors) = (tracks.iter().filter(|t| t.analysis.is_some()).count(), tracks.iter().filter(|t| t.color.is_some()).count());
                log::info!("Mixxx: read {} from {} in {:?}: {} tracks ({waveforms} with a waveform, {colors} with a colour), {} lists", db.file_name().unwrap_or_default().to_string_lossy(), db.parent().unwrap_or(db).display(), started.elapsed(), tracks.len(), index.library.lists.len());
            }
            Err(e) => log::warn!("Mixxx: cannot read the library {e}"),
        }
        *loaded = Some((modified, result.clone()));
        result
    }

    /// The crates, playlists and history sessions and the size of the library, for choosing what to add.
    pub fn overview(&self) -> Result<Overview, String> {
        let index = self.index()?;
        Ok(overview_of(&index))
    }

    /// The chosen part of the library as lists: all of it as "Mixxx" when `all`, then each non-empty
    /// list whose id is in `ids`.
    pub fn open_lists(&self, all: bool, ids: &HashSet<String>) -> Result<Vec<Loaded>, String> {
        let index = self.index()?;
        Ok(lists_of(&index, all, ids))
    }

    /// The library's track for a file, `None` when the file is not in it.
    fn track(&self, file: &Path) -> Option<Track> {
        let index = self.index().ok()?;
        index.by_path.get(&nullamp_rekordbox::path_key(file)).map(|&i| index.library.tracks[i].clone())
    }

    /// Fills what the file's own tags leave out from the library. `false` when the file is not in it.
    pub fn fill(&self, file: &Path, tags: &mut TrackInfo) -> bool {
        let Some(track) = self.track(file) else { return false };
        fill_gaps(tags, &info(&track));
        true
    }

    /// The cacheable record of a file in the library, `None` for any other file. Mixxx's colour
    /// labels are any colour, so it is kept as "#rrggbb" rather than as one of rekordbox's names.
    pub fn data(&self, file: &Path) -> Option<Data> {
        let t = self.track(file)?;
        Some(Data { key: t.key, bpm: t.bpm, rating: t.rating.min(5), color: t.color.map(|c| format!("#{c:06x}")) })
    }

    /// The waveform Mixxx analysed for a file in the library, in the timeline's form, with a colour
    /// per bucket from its low, mid and high bands. `None` for any other file or one Mixxx never analysed.
    pub fn waveform(&self, file: &Path) -> Option<Analysed> {
        match self.track(file)?.waveform() {
            Ok(Some(wave)) => {
                let (peak, rms) = wave.buckets(nullamp_audio::WAVEFORM_BUCKETS);
                log::info!("waveform of {} from the Mixxx analysis ({} columns{})", file.display(), wave.heights.len(), if wave.bands.is_some() { ", coloured" } else { "" });
                Some(Analysed { wave: nullamp_audio::Waveform { peak, rms }, colors: wave.bucket_colors(nullamp_audio::WAVEFORM_BUCKETS) })
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!("Mixxx waveform of {}: {e}", file.display());
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nullamp_mixxx::List;

    pub(crate) fn track(id: i64, file: &str) -> Track {
        Track {
            id,
            title: format!("Title {id}"),
            artist: String::new(),
            album: String::new(),
            genre: String::new(),
            key: Some("B♭m".into()),
            bpm: Some(126.5),
            duration_secs: 300.4,
            year: Some(2018),
            track_number: 0,
            bitrate: 320,
            sample_rate: 44100,
            rating: 4,
            color: Some(0x00ff80),
            analysis: None,
            play_count: 0,
            comment: String::new(),
            date_added: String::new(),
            file: file.into(),
        }
    }

    fn list(id: i64, name: &str, kind: Kind, tracks: &[usize]) -> List {
        List { id, name: name.into(), kind, tracks: tracks.to_vec() }
    }

    fn index() -> Index {
        Index::new(Library {
            tracks: vec![track(1, "/m/a.mp3"), track(2, "/m/b.mp3")],
            lists: vec![list(1, "Trance", Kind::Crate, &[1, 0]), list(2, "Warm up", Kind::Playlist, &[0]), list(3, "Trance", Kind::Playlist, &[1]), list(4, "2019-06-24", Kind::History, &[0, 1]), list(5, "Empty", Kind::Crate, &[])],
        })
    }

    #[test]
    fn the_chooser_has_a_folder_for_each_kind_of_list_that_exists() {
        let o = overview_of(&index());
        assert_eq!(o.tracks, 2);
        let rows: Vec<(&str, usize, bool, usize)> = o.items.iter().map(|i| (i.name.as_str(), i.depth, i.is_folder, i.tracks)).collect();
        assert_eq!(rows, [("Crates", 0, true, 0), ("Trance", 1, false, 2), ("Empty", 1, false, 0), ("Playlists", 0, true, 0), ("Warm up", 1, false, 1), ("Trance", 1, false, 1), ("History", 0, true, 0), ("2019-06-24", 1, false, 2)]);
        assert_eq!(o.below(0), 1..3, "a folder spans its lists");
        let mut lib = index().library;
        lib.lists.retain(|l| l.kind == Kind::Crate);
        let only_crates = overview_of(&Index::new(lib));
        let rows: Vec<&str> = only_crates.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(rows, ["Crates", "Trance", "Empty"], "no empty folders");
    }

    #[test]
    fn chosen_lists_become_playlists_with_their_tracks_in_order() {
        let ids: HashSet<String> = ["crates/1", "playlists/2", "history/4", "crates/5"].map(String::from).into();
        let lists = lists_of(&index(), false, &ids);
        let names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Trance", "Warm up", "History / 2019-06-24"], "an empty list makes no playlist");
        let files: Vec<&Path> = lists[0].tracks.as_ref().unwrap().iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(files, [Path::new("/m/b.mp3"), Path::new("/m/a.mp3")]);
        let (_, first) = &lists[0].tracks.as_ref().unwrap()[0];
        assert_eq!((first.title.as_deref(), first.artist.as_deref(), first.bpm, first.key, first.year), (Some("Title 2"), None, Some(126.5), Key::parse("Bbm"), Some(2018)));
        assert_eq!(first.duration, Some(300.4));
    }

    #[test]
    fn the_whole_library_is_one_list_and_a_shared_name_gets_its_kind() {
        let ids: HashSet<String> = ["crates/1", "playlists/3"].map(String::from).into();
        let lists = lists_of(&index(), true, &ids);
        let names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Mixxx", "Crates / Trance", "Playlists / Trance"]);
        assert_eq!(lists[0].tracks.as_ref().unwrap().len(), 2);
    }

    /// A reader, turned on, whose "library file" is a dummy and whose parse is `library`, as if read from it.
    pub(crate) fn mixxx_with_library(dir: &Path, library: Library) -> Mixxx {
        let db = dir.join("mixxxdb.sqlite");
        std::fs::write(&db, b"not read, the index below stands in for it").unwrap();
        let mixxx = Mixxx::at(vec![dir.join("elsewhere/mixxxdb.sqlite"), db.clone()]);
        *mixxx.loaded.lock().unwrap() = Some((std::fs::metadata(&db).unwrap().modified().ok(), Ok(Arc::new(Index::new(library)))));
        mixxx
    }

    /// The same, with the test library, not yet turned on.
    fn mixxx_with_index(dir: &Path) -> Mixxx {
        mixxx_with_library(dir, index().library)
    }

    #[test]
    fn tags_win_and_mixxx_fills_the_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let mixxx = mixxx_with_index(dir.path());
        mixxx.set_enabled(true);
        let mut tags = TrackInfo { title: Some("Own".into()), ..TrackInfo::default() };
        assert!(mixxx.fill(Path::new("/m/a.mp3"), &mut tags));
        assert_eq!((tags.title.as_deref(), tags.bpm, tags.key), (Some("Own"), Some(126.5), Key::parse("Bbm")));
        assert!(!mixxx.fill(Path::new("/m/other.mp3"), &mut TrackInfo::default()));
    }

    #[test]
    fn the_record_of_a_file_in_the_library_is_what_the_cache_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let mixxx = mixxx_with_index(dir.path());
        assert!(mixxx.data(Path::new("/m/a.mp3")).is_none(), "not opened yet");
        mixxx.set_enabled(true);
        let data = mixxx.data(Path::new("/m/a.mp3")).unwrap();
        assert_eq!((data.key.as_deref(), data.bpm, data.rating, data.color), (Some("B♭m"), Some(126.5), 4, Some("#00ff80".to_owned())));
        assert!(mixxx.data(Path::new("/m/other.mp3")).is_none());
    }

    #[test]
    fn nothing_is_read_until_the_library_is_opened_and_only_where_mixxx_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let mixxx = mixxx_with_index(dir.path());
        assert!(mixxx.overview().is_err(), "not opened yet");
        assert!(!mixxx.fill(Path::new("/m/a.mp3"), &mut TrackInfo::default()));
        mixxx.set_enabled(true);
        assert!(mixxx.overview().is_ok());
        let nowhere = Mixxx::at(vec![dir.path().join("none/mixxxdb.sqlite")]);
        nowhere.set_enabled(true);
        let e = nowhere.overview().unwrap_err();
        assert!(e.starts_with("No Mixxx library found") && e.contains("none"), "{e}");
        assert_eq!(nowhere.reported.lock().unwrap().as_deref(), Some(e.as_str()), "remembered, so it is logged once");
        assert!(mixxx.overview().is_ok() && mixxx.reported.lock().unwrap().is_none(), "found again, so a later problem is logged again");
    }
}
