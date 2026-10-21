//! rekordbox USB sticks: their tracks as playlist entries, and the waveform rekordbox analysed
//! for a track. Everything here runs on worker threads; the UI only receives the results.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use nullamp_core::{Key, TrackInfo};
use serde::{Deserialize, Serialize};
use nullamp_rekordbox::{Collection, Playlist, Stick, Track};

/// One mounted stick, ready to become a playlist.
pub struct Loaded {
    /// The volume's name.
    pub name: String,
    pub tracks: Result<Vec<(PathBuf, TrackInfo)>, String>,
}

fn info(t: &Track) -> TrackInfo {
    let text = |s: &str| Some(s.to_owned()).filter(|s| !s.is_empty());
    TrackInfo {
        title: text(&t.title),
        artist: text(&t.artist),
        album: text(&t.album),
        duration: (t.duration_secs > 0).then_some(f64::from(t.duration_secs)),
        bitrate: (t.bitrate > 0).then_some(t.bitrate),
        bpm: t.bpm,
        key: t.key.as_deref().and_then(Key::parse),
        ..TrackInfo::default()
    }
}

/// The tracks of every stick mounted under `volumes`.
#[cfg(test)]
fn load_sticks(volumes: &Path) -> Vec<Loaded> {
    load_stick_roots(nullamp_rekordbox::detect(volumes))
}

/// The tracks of every stick mounted anywhere the platform mounts them (`/Volumes`, drive letters, ...).
pub fn load_mounted_sticks() -> Vec<Loaded> {
    load_stick_roots(nullamp_rekordbox::detect_mounted())
}

fn load_stick_roots(roots: Vec<PathBuf>) -> Vec<Loaded> {
    roots
        .into_iter()
        .map(|root| {
            // A Windows drive root has no file name; the drive letter ("E:") names it instead.
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).or_else(|| Some(root.to_string_lossy().trim_end_matches(['\\', '/']).to_owned()).filter(|n| !n.is_empty())).unwrap_or_else(|| "rekordbox".into());
            let tracks = Stick::open(&root).map(|s| s.tracks.iter().map(|t| (t.file.clone(), info(t))).collect()).map_err(|e| e.to_string());
            Loaded { name, tracks }
        })
        .collect()
}

/// What rekordbox knows about a track that the file's own tags usually lack, kept in the cache by the
/// file's content fingerprint so it is still there when the same file is played from somewhere else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Data {
    /// As rekordbox names it ("Am", "Bbm").
    pub key: Option<String>,
    pub bpm: Option<f32>,
    /// Stars, 0 to 5. Shown in the inspector only.
    pub rating: u8,
    /// The colour label ("red", "aqua", ...). Shown in the inspector only.
    pub color: Option<String>,
}

impl Data {
    pub fn of(t: &Track) -> Data {
        Data { key: t.key.clone(), bpm: t.bpm, rating: t.rating, color: t.color.map(|c| c.name().to_owned()) }
    }

    /// The part of it that playlist rows show.
    fn info(&self) -> TrackInfo {
        TrackInfo { key: self.key.as_deref().and_then(Key::parse), bpm: self.bpm, ..TrackInfo::default() }
    }
}

impl Data {
    /// Every entry of the record that is not an object or an array, as the key and its value in
    /// text, for the inspector to show what rekordbox knows. A missing value reads "none".
    pub fn scalars(&self) -> Vec<(String, String)> {
        let Ok(serde_json::Value::Object(map)) = serde_json::to_value(self) else { return Vec::new() };
        map.into_iter()
            .filter_map(|(k, v)| match v {
                serde_json::Value::Null => Some((k, "none".to_owned())),
                serde_json::Value::String(s) => Some((k, s)),
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Some((k, v.to_string())),
                serde_json::Value::Object(_) | serde_json::Value::Array(_) => None,
            })
            .collect()
    }
}

/// Fills what `tags` leave out from `data`, the cached record of the same file on a stick.
pub fn fill_from_cache(tags: &mut TrackInfo, data: &Data) {
    fill_gaps(tags, &data.info());
}

/// A stick's tracks by file, as of the database's modification time.
struct Cached {
    modified: Option<SystemTime>,
    tracks: HashMap<String, Track>,
}

/// The rekordbox data of files that lie on a USB stick, found by looking for the stick above the
/// file. Works however a file got into a playlist and across restarts. Shared by the worker
/// threads; the parsed databases are kept and re-read when the stick is exported again.
#[derive(Default)]
pub struct Sticks {
    cache: Mutex<HashMap<PathBuf, Cached>>,
}

/// A waveform in the form the timeline draws, with rekordbox's colours when it has them.
pub struct Analysed {
    pub wave: nullamp_audio::Waveform,
    pub colors: Option<Vec<[u8; 3]>>,
}

impl Sticks {
    /// rekordbox's record of `file`, `None` when it is not on a stick or not in its library.
    pub fn track(&self, file: &Path) -> Option<Track> {
        let root = nullamp_rekordbox::stick_root(file)?;
        let modified = std::fs::metadata(root.join("PIONEER/rekordbox/export.pdb")).and_then(|m| m.modified()).ok();
        let mut cache = self.cache.lock().ok()?;
        if cache.get(&root).is_none_or(|c| c.modified != modified) {
            let tracks = match Stick::open(&root) {
                Ok(stick) => stick.tracks.into_iter().map(|t| (nullamp_rekordbox::path_key(&t.file), t)).collect(),
                Err(e) => {
                    log::debug!("rekordbox library {}: {e}", root.display());
                    HashMap::new()
                }
            };
            cache.insert(root.clone(), Cached { modified, tracks });
        }
        cache.get(&root)?.tracks.get(&nullamp_rekordbox::path_key(file)).cloned()
    }

    /// Fills what the file's own tags leave out (key and BPM mostly: rekordbox keeps them in its
    /// database, not in the file) from the stick's library. `false` when the file is not on a stick.
    pub fn fill(&self, file: &Path, info: &mut TrackInfo) -> bool {
        let Some(track) = self.track(file) else { return false };
        fill_gaps(info, &self::info(&track));
        true
    }

    /// The cacheable record of a file on a stick, `None` for any other file.
    pub fn data(&self, file: &Path) -> Option<Data> {
        self.track(file).map(|t| Data::of(&t))
    }

    pub fn waveform(&self, file: &Path) -> Option<Analysed> {
        analysed(file, &self.track(file)?)
    }
}

/// The waveform rekordbox analysed for `track`, in the timeline's form. Shared by the stick and the
/// desktop library, which differ only in how they find the track.
pub fn analysed(file: &Path, track: &Track) -> Option<Analysed> {
    match track.waveform() {
        Ok(Some(wave)) => {
            let (peak, rms) = wave.buckets(nullamp_audio::WAVEFORM_BUCKETS);
            log::debug!("waveform of {} from the rekordbox analysis ({} columns)", file.display(), wave.heights.len());
            Some(Analysed { wave: nullamp_audio::Waveform { peak, rms }, colors: wave.bucket_colors(nullamp_audio::WAVEFORM_BUCKETS) })
        }
        Ok(None) => None,
        Err(e) => {
            log::debug!("rekordbox waveform of {}: {e}", file.display());
            None
        }
    }
}

/// Where the desktop library lives and the key to open it. The share folder holds the analysis
/// files, normally the `share` directory beside `master.db`.
#[derive(Clone, PartialEq)]
pub struct DesktopConfig {
    pub db: PathBuf,
    pub key: String,
    pub share: PathBuf,
}

impl DesktopConfig {
    /// The standard rekordbox 7 location with the given key (`~/Library/Pioneer/rekordbox` on macOS,
    /// `%APPDATA%\Pioneer\rekordbox` on Windows), or `None` when the home directory cannot be found.
    pub fn standard(key: String) -> Option<DesktopConfig> {
        let dirs = directories::BaseDirs::new()?;
        let base = if cfg!(windows) { dirs.config_dir().join("Pioneer/rekordbox") } else { dirs.home_dir().join("Library/Pioneer/rekordbox") };
        Some(DesktopConfig { db: base.join("master.db"), share: base.join("share"), key })
    }
}

/// The parsed desktop library with its tracks indexed by file path, as of the database's modification time.
pub struct Library {
    collection: Collection,
    by_path: HashMap<String, usize>,
}

impl Library {
    fn new(collection: Collection) -> Library {
        let by_path = collection.tracks.iter().enumerate().map(|(i, t)| (nullamp_rekordbox::path_key(&t.file), i)).collect();
        Library { collection, by_path }
    }
}

/// One playlist or folder of the desktop library, as the chooser lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// How many folders it lies in.
    pub depth: usize,
    pub is_folder: bool,
    /// Tracks of its own; a folder has none.
    pub tracks: usize,
}

/// What the chooser needs of the library: how many tracks it holds and its playlist tree, folders
/// before what they contain, in rekordbox's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Overview {
    pub tracks: usize,
    pub items: Vec<Item>,
}

impl Overview {
    /// The rows inside the folder at `index`: those that follow it and lie deeper.
    pub fn below(&self, index: usize) -> std::ops::Range<usize> {
        let depth = self.items[index].depth;
        let len = self.items[index + 1..].iter().take_while(|i| i.depth > depth).count();
        index + 1..index + 1 + len
    }
}

/// The playlists as (index, depth) in tree order: each folder, then what it holds, siblings in the
/// order given. An entry whose folder no longer exists counts as top-level.
fn tree_order(playlists: &[Playlist]) -> Vec<(usize, usize)> {
    let ids: HashSet<&str> = playlists.iter().map(|p| p.id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for (i, p) in playlists.iter().enumerate() {
        let parent = p.parent.as_deref().filter(|id| ids.contains(id));
        children.entry(parent).or_default().push(i);
    }
    let mut out = Vec::with_capacity(playlists.len());
    let mut stack: Vec<(usize, usize)> = children.get(&None).into_iter().flatten().rev().map(|&i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        out.push((i, depth));
        stack.extend(children.get(&Some(playlists[i].id.as_str())).into_iter().flatten().rev().map(|&c| (c, depth + 1)));
    }
    out
}

/// What each of the playlists at `picked` is called as a list: its own name, or with the folders
/// above it when two of them would share one (a name becomes a list, and a list of that name is replaced).
fn list_names(playlists: &[Playlist], picked: &[usize]) -> Vec<String> {
    let by_id: HashMap<&str, &Playlist> = playlists.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut taken: HashMap<&str, usize> = HashMap::new();
    for &i in picked {
        *taken.entry(playlists[i].name.as_str()).or_default() += 1;
    }
    picked
        .iter()
        .map(|&i| {
            let p = &playlists[i];
            if taken[p.name.as_str()] < 2 {
                return p.name.clone();
            }
            let mut path = vec![p.name.as_str()];
            let mut up = p.parent.as_deref();
            while let Some(folder) = up.and_then(|id| by_id.get(id)) {
                path.push(folder.name.as_str());
                up = folder.parent.as_deref();
            }
            path.reverse();
            path.join(" / ")
        })
        .collect()
}

/// The **desktop** library: the tracks of `master.db`, indexed by file path so what rekordbox
/// knows fills any file the user plays, wherever it came from. Parallels [`Sticks`] but for the
/// local collection, which needs a key the user supplies once. Empty until a key is set.
#[derive(Default)]
pub struct Desktop {
    config: Mutex<Option<DesktopConfig>>,
    /// The library as read when `master.db` had this modification time, or why it could not be.
    loaded: Mutex<Option<(Option<SystemTime>, Result<Arc<Library>, String>)>>,
}

impl Desktop {
    /// Points the library at a new location and key (or clears it). Drops the parsed library so the
    /// next lookup reads the new one.
    pub fn set_config(&self, config: Option<DesktopConfig>) {
        let mut slot = self.config.lock().unwrap();
        if *slot != config {
            *slot = config;
            *self.loaded.lock().unwrap() = None;
        }
    }

    /// The parsed library, read again when `master.db` has changed. `Err` when no key is set or the
    /// database will not open.
    fn library(&self) -> Result<Arc<Library>, String> {
        let config = self.config.lock().unwrap().clone().ok_or("No rekordbox key set (Settings > rekordbox)")?;
        let modified = std::fs::metadata(&config.db).and_then(|m| m.modified()).ok();
        let mut loaded = self.loaded.lock().unwrap();
        if let Some((at, result)) = loaded.as_ref()
            && *at == modified
        {
            return result.clone();
        }
        let result = Collection::open(&config.db, &config.key, &config.share).map(|col| Arc::new(Library::new(col))).map_err(|e| e.to_string());
        if let Err(e) = &result {
            log::debug!("rekordbox desktop library {}: {e}", config.db.display());
        }
        *loaded = Some((modified, result.clone()));
        result
    }

    /// The playlist tree and the size of the collection, for choosing what to add.
    pub fn overview(&self) -> Result<Overview, String> {
        let lib = self.library()?;
        let playlists = &lib.collection.playlists;
        let items = tree_order(playlists)
            .into_iter()
            .map(|(i, depth)| {
                let p = &playlists[i];
                Item { id: p.id.clone(), name: p.name.clone(), depth, is_folder: p.is_folder, tracks: p.tracks.len() }
            })
            .collect();
        Ok(Overview { tracks: lib.collection.tracks.len(), items })
    }

    /// The chosen part of the library as lists: the whole collection as "rekordbox" when `all`, then
    /// each non-empty playlist whose id is in `ids`, in tree order.
    pub fn open_playlists(&self, all: bool, ids: &HashSet<String>) -> Result<Vec<Loaded>, String> {
        let lib = self.library()?;
        let col = &lib.collection;
        let entry = |t: &Track| (t.file.clone(), info(t));
        let mut lists = Vec::new();
        if all {
            lists.push(Loaded { name: "rekordbox".into(), tracks: Ok(col.tracks.iter().map(entry).collect()) });
        }
        let picked: Vec<usize> = tree_order(&col.playlists).into_iter().map(|(i, _)| i).filter(|&i| ids.contains(&col.playlists[i].id) && !col.playlists[i].tracks.is_empty()).collect();
        let names = list_names(&col.playlists, &picked);
        for (&i, name) in picked.iter().zip(names) {
            lists.push(Loaded { name, tracks: Ok(col.playlists[i].tracks.iter().map(|&t| entry(&col.tracks[t])).collect()) });
        }
        Ok(lists)
    }

    /// rekordbox's record of `file`. `None` when no key is set, the library will not open, or the
    /// file is not in it.
    pub fn track(&self, file: &Path) -> Option<Track> {
        let lib = self.library().ok()?;
        lib.by_path.get(&nullamp_rekordbox::path_key(file)).map(|&i| lib.collection.tracks[i].clone())
    }

    /// Fills what the file's own tags leave out from the library. `false` when the file is not in it.
    pub fn fill(&self, file: &Path, info: &mut TrackInfo) -> bool {
        let Some(track) = self.track(file) else { return false };
        fill_gaps(info, &self::info(&track));
        true
    }

    /// The cacheable record of a file in the library, `None` for any other file.
    pub fn data(&self, file: &Path) -> Option<Data> {
        self.track(file).map(|t| Data::of(&t))
    }

    pub fn waveform(&self, file: &Path) -> Option<Analysed> {
        analysed(file, &self.track(file)?)
    }
}

/// What the tags say wins; `stick` supplies only what they leave out.
fn fill_gaps(info: &mut TrackInfo, stick: &TrackInfo) {
    info.title = info.title.take().or_else(|| stick.title.clone());
    info.artist = info.artist.take().or_else(|| stick.artist.clone());
    info.album = info.album.take().or_else(|| stick.album.clone());
    info.duration = info.duration.or(stick.duration);
    info.bitrate = info.bitrate.or(stick.bitrate);
    info.key = info.key.or(stick.key);
    if info.bpm.is_none() && !info.bpm_off {
        info.bpm = stick.bpm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures")
    }

    fn playlist(id: &str, name: &str, parent: Option<&str>, is_folder: bool, tracks: usize) -> Playlist {
        Playlist { id: id.into(), name: name.into(), parent: parent.map(Into::into), is_folder, tracks: (0..tracks).collect() }
    }

    /// Two folders (one nested), listed out of order, and a playlist whose folder is gone.
    fn tree() -> Vec<Playlist> {
        vec![
            playlist("in-b", "Warm up", Some("b"), false, 3),
            playlist("a", "Clubs", None, true, 0),
            playlist("top", "Favourites", None, false, 5),
            playlist("b", "Sets", Some("a"), true, 0),
            playlist("in-a", "Peak", Some("a"), false, 2),
            playlist("orphan", "Lost", Some("deleted"), false, 1),
        ]
    }

    #[test]
    fn the_playlist_tree_lists_folders_before_what_they_hold() {
        let playlists = tree();
        let order: Vec<(&str, usize)> = tree_order(&playlists).into_iter().map(|(i, depth)| (playlists[i].id.as_str(), depth)).collect();
        assert_eq!(order, vec![("a", 0), ("b", 1), ("in-b", 2), ("in-a", 1), ("top", 0), ("orphan", 0)]);
    }

    #[test]
    fn a_folder_spans_the_rows_deeper_than_itself() {
        let items = |rows: &[(usize, bool)]| Overview { tracks: 0, items: rows.iter().map(|&(depth, is_folder)| Item { id: String::new(), name: String::new(), depth, is_folder, tracks: 0 }).collect() };
        let overview = items(&[(0, true), (1, true), (2, false), (1, false), (0, false)]);
        assert_eq!(overview.below(0), 1..4);
        assert_eq!(overview.below(1), 2..3);
        assert_eq!(overview.below(3), 4..4);
        assert_eq!(overview.below(4), 5..5);
    }

    #[test]
    fn lists_keep_their_names_unless_two_would_share_one() {
        let mut playlists = tree();
        let all: Vec<usize> = (0..playlists.len()).collect();
        assert_eq!(list_names(&playlists, &all), ["Warm up", "Clubs", "Favourites", "Sets", "Peak", "Lost"]);
        // The same name in two folders.
        playlists[2].name = "Peak".into();
        assert_eq!(list_names(&playlists, &[2, 4]), ["Peak", "Clubs / Peak"]);
        assert_eq!(list_names(&playlists, &[4]), ["Peak"], "alone it keeps its own name");
    }

    #[test]
    fn scalars_list_every_plain_value_of_the_record() {
        let data = Data { key: Some("Am".into()), bpm: Some(128.5), rating: 3, color: None };
        let rows = data.scalars();
        let get = |k: &str| rows.iter().find(|(name, _)| name == k).map(|(_, v)| v.as_str());
        assert_eq!(get("key"), Some("Am"));
        assert_eq!(get("bpm"), Some("128.5"));
        assert_eq!(get("rating"), Some("3"));
        assert_eq!(get("color"), Some("none"));
    }

    #[test]
    fn a_stick_becomes_entries_with_their_tags_filled_in() {
        let sticks = load_sticks(&fixtures());
        assert_eq!(sticks.len(), 1);
        assert_eq!(sticks[0].name, "rekordbox");
        let tracks = sticks[0].tracks.as_ref().unwrap();
        assert_eq!(tracks.len(), 3);
        let (path, info) = tracks.iter().find(|(p, _)| p.ends_with("Veracocha - Carte Blanche (Original Mix).mp3")).unwrap();
        assert!(path.starts_with(fixtures().join("rekordbox/Contents")));
        assert_eq!((info.artist.as_deref(), info.title.as_deref()), (Some("Veracocha"), Some("Carte Blanche (Original Mix)")));
        assert_eq!((info.duration, info.bitrate, info.bpm), (Some(425.0), Some(320), Some(140.02)));
        assert_eq!(info.key, Key::parse("Bbm"));
        assert!(!info.bpm_detected, "the BPM is rekordbox's, not a guess");
    }

    #[test]
    fn a_file_on_a_stick_gets_the_rekordbox_waveform_and_others_do_not() {
        let sticks = Sticks::default();
        let file = fixtures().join("rekordbox/Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
        let found = sticks.waveform(&file).expect("analysed track");
        assert_eq!(found.wave.peak.len(), nullamp_audio::WAVEFORM_BUCKETS);
        assert_eq!(found.wave.peak.len(), found.wave.rms.len());
        assert!(found.wave.peak.iter().any(|&p| p > 128));
        assert_eq!(found.colors.map(|c| c.len()), Some(found.wave.peak.len()));
        assert!(sticks.waveform(&fixtures().join("beat_120.mp3")).is_none(), "not on a stick");
        assert!(sticks.waveform(&fixtures().join("rekordbox/Contents/Signum/other.mp3")).is_none(), "not in the library");
    }

    #[test]
    fn data_of_a_stick_file_and_none_for_others() {
        let sticks = Sticks::default();
        let file = fixtures().join("rekordbox/Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
        let data = sticks.data(&file).expect("on the stick");
        assert_eq!((data.key.as_deref(), data.bpm, data.rating, data.color.as_deref()), (Some("Bbm"), Some(140.02), 0, None));
        assert!(sticks.data(&fixtures().join("beat_120.mp3")).is_none());
        let mut tags = TrackInfo { bpm: Some(128.0), ..TrackInfo::default() };
        fill_from_cache(&mut tags, &data);
        assert_eq!((tags.bpm, tags.key), (Some(128.0), Key::parse("Bbm")));
    }

    #[test]
    fn tags_win_and_the_stick_fills_the_gaps() {
        let sticks = Sticks::default();
        let file = fixtures().join("rekordbox/Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
        // A file whose tags hold a title and nothing else, like the ones rekordbox exports.
        let mut info = TrackInfo { title: Some("Own title".into()), ..TrackInfo::default() };
        sticks.fill(&file, &mut info);
        assert_eq!(info.title.as_deref(), Some("Own title"));
        assert_eq!((info.artist.as_deref(), info.bpm, info.key), (Some("Veracocha"), Some(140.02), Key::parse("Bbm")));
        let mut tags = TrackInfo { bpm: Some(128.0), key: Key::parse("Am"), ..TrackInfo::default() };
        sticks.fill(&file, &mut tags);
        assert_eq!((tags.bpm, tags.key), (Some(128.0), Key::parse("Am")));
        let mut off = TrackInfo { bpm_off: true, ..TrackInfo::default() };
        sticks.fill(&file, &mut off);
        assert_eq!(off.bpm, None, "a BPM switched off in the tags stays off");
    }
}
