//! Disk cache of what the background analysis finds in a file (waveform overview, tempo), so
//! a file is only decoded for it once.
//!
//! Entries are keyed by the file's content fingerprint (see `store::fingerprint`), which
//! survives retagging and renaming. A hit refreshes the entry's modified time, so the
//! age-based cleanup removes what has gone unused, not what is merely old.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use nullamp_audio::{Tempo, Waveform};

/// Entries not used for this long are removed at startup. A few KB each; this only keeps
/// the folder from collecting every file ever played.
pub const MAX_AGE: Duration = Duration::from_secs(90 * 24 * 3600);
/// A hit only rewrites the modified time when it is older than this, so replaying a track does not touch the disk.
const TOUCH_AFTER: Duration = Duration::from_secs(24 * 3600);

fn entry(dir: &Path, fingerprint: &str) -> std::path::PathBuf {
    entry_as(dir, fingerprint, "wf")
}

fn entry_as(dir: &Path, fingerprint: &str, extension: &str) -> std::path::PathBuf {
    dir.join(format!("{:016x}.{extension}", crate::store::fnv1a(fingerprint.as_bytes())))
}

/// The entry's bytes, refreshing its modified time when it is getting old.
fn read_entry(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let stale = fs::metadata(path).ok()?.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > TOUCH_AFTER);
    if stale && let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(bytes)
}

pub(crate) fn write_entry(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // Named after the whole entry: the waveform and the tempo of one file are written at the same time.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

pub fn load(dir: &Path, fingerprint: &str) -> Option<Waveform> {
    Waveform::from_bytes(&read_entry(&entry(dir, fingerprint))?)
}

pub fn store(dir: &Path, fingerprint: &str, wave: &Waveform) {
    write_entry(&entry(dir, fingerprint), &wave.to_bytes());
}

const RECORDBOX_MAGIC: &[u8; 4] = b"NRW1";

/// A waveform taken from rekordbox's analysis, in `entry_as(.., "rwf")` beside the decoded one so
/// that turning rekordbox off brings the decoded waveform back: magic, bucket count (u32 LE), peaks,
/// RMS values, then optionally the three colour bytes of every bucket.
pub fn load_rekordbox(dir: &Path, fingerprint: &str) -> Option<crate::rekordbox::Analysed> {
    parse_rekordbox(&read_entry(&entry_as(dir, fingerprint, "rwf"))?)
}

/// The bytes of a `.rwf` entry, `None` unless they are well formed (also what a data pack is checked with).
pub fn parse_rekordbox(bytes: &[u8]) -> Option<crate::rekordbox::Analysed> {
    let (head, body) = bytes.split_at_checked(8)?;
    if &head[..4] != RECORDBOX_MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    if n == 0 || n > nullamp_audio::WAVEFORM_BUCKETS || !(body.len() == n * 2 || body.len() == n * 5) {
        return None;
    }
    let colors = (body.len() == n * 5).then(|| body[n * 2..].chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect());
    Some(crate::rekordbox::Analysed { wave: Waveform { peak: body[..n].to_vec(), rms: body[n..n * 2].to_vec(), colors: None, heights: None }, colors })
}

pub fn store_rekordbox(dir: &Path, fingerprint: &str, found: &crate::rekordbox::Analysed) {
    let n = found.wave.peak.len().min(found.wave.rms.len());
    let mut bytes = RECORDBOX_MAGIC.to_vec();
    bytes.extend_from_slice(&(n as u32).to_le_bytes());
    bytes.extend_from_slice(&found.wave.peak[..n]);
    bytes.extend_from_slice(&found.wave.rms[..n]);
    if let Some(colors) = found.colors.as_ref().filter(|c| c.len() == n) {
        bytes.extend(colors.iter().flatten());
    }
    write_entry(&entry_as(dir, fingerprint, "rwf"), &bytes);
}

/// rekordbox's record of the file (key, BPM, rating, colour label) as JSON in a `.rbi` entry.
pub fn load_rekordbox_data(dir: &Path, fingerprint: &str) -> Option<crate::rekordbox::Data> {
    serde_json::from_slice(&read_entry(&entry_as(dir, fingerprint, "rbi"))?).ok()
}

pub fn store_rekordbox_data(dir: &Path, fingerprint: &str, data: &crate::rekordbox::Data) {
    if let Ok(bytes) = serde_json::to_vec(data) {
        write_entry(&entry_as(dir, fingerprint, "rbi"), &bytes);
    }
}

const TEMPO_MAGIC: &[u8; 4] = b"NBP1";

/// `Some(None)` is a cached "no clear beat", so such a file is not analysed again; `None` is a miss.
pub fn load_tempo(dir: &Path, fingerprint: &str) -> Option<Option<Tempo>> {
    let bytes = read_entry(&entry_as(dir, fingerprint, "bpm"))?;
    let [m0, m1, m2, m3, b0, b1, b2, b3, c0, c1, c2, c3] = bytes[..] else { return None };
    if &[m0, m1, m2, m3] != TEMPO_MAGIC {
        return None;
    }
    let (bpm, confidence) = (f32::from_le_bytes([b0, b1, b2, b3]), f32::from_le_bytes([c0, c1, c2, c3]));
    Some((bpm > 0.0).then_some(Tempo { bpm, confidence }))
}

pub fn store_tempo(dir: &Path, fingerprint: &str, tempo: Option<Tempo>) {
    let (bpm, confidence) = tempo.map_or((0.0, 0.0), |t| (t.bpm, t.confidence));
    let mut bytes = TEMPO_MAGIC.to_vec();
    bytes.extend_from_slice(&bpm.to_le_bytes());
    bytes.extend_from_slice(&confidence.to_le_bytes());
    write_entry(&entry_as(dir, fingerprint, "bpm"), &bytes);
}

/// Remove entries unused for `max_age`. Returns how many went.
pub fn prune(dir: &Path, max_age: Duration) -> usize {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    read.filter_map(|e| e.ok())
        .filter(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|e| fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave() -> Waveform {
        Waveform { peak: vec![9, 8, 7], rms: vec![3, 2, 1], colors: None, heights: None }
    }

    #[test]
    fn stores_and_loads_by_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "abc"), None);
        store(dir.path(), "abc", &wave());
        assert_eq!(load(dir.path(), "abc"), Some(wave()));
        assert_eq!(load(dir.path(), "abd"), None);
    }

    #[test]
    fn rekordbox_waveform_is_kept_apart_from_the_decoded_one() {
        use crate::rekordbox::Analysed;
        let dir = tempfile::tempdir().unwrap();
        assert!(load_rekordbox(dir.path(), "abc").is_none());
        let coloured = Analysed { wave: wave(), colors: Some(vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]]) };
        store_rekordbox(dir.path(), "abc", &coloured);
        let back = load_rekordbox(dir.path(), "abc").unwrap();
        assert_eq!((back.wave, back.colors), (wave(), coloured.colors));
        assert_eq!(load(dir.path(), "abc"), None, "the decoded entry is separate");
        store_rekordbox(dir.path(), "plain", &Analysed { wave: wave(), colors: None });
        assert_eq!(load_rekordbox(dir.path(), "plain").unwrap().colors, None);
        fs::write(entry_as(dir.path(), "abc", "rwf"), b"NRW1\x03\0\0\0xx").unwrap();
        assert!(load_rekordbox(dir.path(), "abc").is_none(), "damage is a miss");
    }

    #[test]
    fn rekordbox_data_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_rekordbox_data(dir.path(), "abc"), None);
        let data = crate::rekordbox::Data { key: Some("Am".into()), bpm: Some(128.5), rating: 3, color: Some("aqua".into()) };
        store_rekordbox_data(dir.path(), "abc", &data);
        assert_eq!(load_rekordbox_data(dir.path(), "abc"), Some(data));
        fs::write(entry_as(dir.path(), "abc", "rbi"), b"{ not json").unwrap();
        assert_eq!(load_rekordbox_data(dir.path(), "abc"), None, "damage is a miss");
    }

    #[test]
    fn tempo_round_trips_including_no_tempo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_tempo(dir.path(), "abc"), None);
        let tempo = Tempo { bpm: 127.5, confidence: 0.8 };
        store_tempo(dir.path(), "abc", Some(tempo));
        store_tempo(dir.path(), "none", None);
        assert_eq!(load_tempo(dir.path(), "abc"), Some(Some(tempo)));
        assert_eq!(load_tempo(dir.path(), "none"), Some(None));
        fs::write(entry_as(dir.path(), "abc", "bpm"), b"NBP1xx").unwrap();
        assert_eq!(load_tempo(dir.path(), "abc"), None, "damage is a miss");
    }

    #[test]
    fn damaged_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "abc", &wave());
        fs::write(entry(dir.path(), "abc"), b"garbage").unwrap();
        assert_eq!(load(dir.path(), "abc"), None);
    }

    #[test]
    fn prune_removes_unused_entries_and_a_hit_keeps_one_alive() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), "old", &wave());
        store(dir.path(), "used", &wave());
        let long_ago = SystemTime::now() - Duration::from_secs(100 * 24 * 3600);
        for name in ["old", "used"] {
            fs::File::options().write(true).open(entry(dir.path(), name)).unwrap().set_modified(long_ago).unwrap();
        }
        assert!(load(dir.path(), "used").is_some(), "a hit refreshes the entry");
        assert_eq!(prune(dir.path(), MAX_AGE), 1);
        assert_eq!(load(dir.path(), "old"), None);
        assert!(load(dir.path(), "used").is_some());
    }
}
