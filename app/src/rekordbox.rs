//! rekordbox USB sticks: their tracks as playlist entries, and the waveform rekordbox analysed
//! for a track. Everything here runs on worker threads; the UI only receives the results.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use nullamp_core::{Key, TrackInfo};
use nullamp_rekordbox::{Stick, Track};

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
pub fn load_sticks(volumes: &Path) -> Vec<Loaded> {
    nullamp_rekordbox::detect(volumes)
        .into_iter()
        .map(|root| {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "rekordbox".into());
            let tracks = Stick::open(&root).map(|s| s.tracks.iter().map(|t| (t.file.clone(), info(t))).collect()).map_err(|e| e.to_string());
            Loaded { name, tracks }
        })
        .collect()
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
    fn track(&self, file: &Path) -> Option<Track> {
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
    /// database, not in the file) from the stick's library.
    pub fn fill(&self, file: &Path, info: &mut TrackInfo) {
        if let Some(track) = self.track(file) {
            fill_gaps(info, &self::info(&track));
        }
    }

    pub fn waveform(&self, file: &Path) -> Option<Analysed> {
        let track = self.track(file)?;
        match track.waveform() {
            Ok(Some(wave)) => {
                let (peak, rms) = wave.buckets(nullamp_audio::WAVEFORM_BUCKETS);
                log::info!("waveform of {} from the rekordbox analysis ({} columns)", file.display(), wave.heights.len());
                Some(Analysed { wave: nullamp_audio::Waveform { peak, rms }, colors: wave.bucket_colors(nullamp_audio::WAVEFORM_BUCKETS) })
            }
            Ok(None) => None,
            Err(e) => {
                log::debug!("rekordbox waveform of {}: {e}", file.display());
                None
            }
        }
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
