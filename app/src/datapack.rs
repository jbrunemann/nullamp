//! The pack that carries what Nullamp cannot make again to another copy of it, say on a phone
//! without rekordbox or Mixxx: the saved setlists and the rekordbox and Mixxx records and
//! waveforms in the cache. Everything else (decoded waveforms, tempo, covers) is made again there.
//!
//! One zip file. A pack is untrusted input: names are matched against fixed patterns and never
//! used as paths, sizes are capped, and every entry must parse before anything is written.

use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::paths::AppPaths;
use crate::rekordbox::Data;
use crate::store::{self, SavedSetlist};
use crate::waveform_cache;

const SETLISTS: &str = "setlists/";
const DJ_DATA: &str = "djdata/";

/// More entries than any library has files; a pack beyond this is not one of ours.
const MAX_ENTRIES: usize = 200_000;
const MAX_SETLIST: u64 = 2 * 1024 * 1024;
const MAX_RECORD: u64 = 16 * 1024;
/// A waveform entry: 8 bytes of header, then peaks, RMS and three colour bytes per bucket.
const MAX_WAVEFORM: u64 = 8 + nullamp_audio::WAVEFORM_BUCKETS as u64 * 5;

/// What a pack held, or what an import did with it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Setlists written (export) or new here (import).
    pub setlists: usize,
    /// Import only: setlists that were already here and took the pack's entries in, as a refresh does.
    pub merged: usize,
    pub waveforms: usize,
    pub records: usize,
    /// Import only: waveforms and records this copy already had, which are left alone.
    pub present: usize,
    /// Import only: entries that were not ours or did not parse.
    pub rejected: usize,
}

enum Kind {
    Setlist(String),
    Waveform(String),
    Record(String),
}

fn classify(name: &str) -> Option<Kind> {
    if let Some(rest) = name.strip_prefix(SETLISTS) {
        let fingerprint = rest.strip_suffix(".json")?;
        return store::valid_fingerprint(fingerprint).then(|| Kind::Setlist(fingerprint.to_owned()));
    }
    let (stem, extension) = name.strip_prefix(DJ_DATA)?.split_once('.')?;
    if stem.len() != 16 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let stem = stem.to_ascii_lowercase();
    match extension {
        "rwf" => Some(Kind::Waveform(stem)),
        "rbi" => Some(Kind::Record(stem)),
        _ => None,
    }
}

/// The cache entry's stem (the hash of the fingerprint) and extension, when the file is one we pack.
fn cache_entry(path: &Path) -> Option<(String, &'static str)> {
    let name = path.file_name()?.to_str()?;
    match classify(&format!("{DJ_DATA}{name}"))? {
        Kind::Waveform(stem) => Some((stem, "rwf")),
        Kind::Record(stem) => Some((stem, "rbi")),
        Kind::Setlist(_) => None,
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Write every saved setlist and every rekordbox or Mixxx entry of the cache to `out`. The setlists go
/// without the path of the file on this machine.
pub fn export(paths: &AppPaths, out: &Path) -> Result<Summary, String> {
    let tmp = out.with_extension("nullamp-tmp");
    let result = write_pack(paths, &tmp).and_then(|summary| fs::rename(&tmp, out).map(|_| summary).map_err(err));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_pack(paths: &AppPaths, file: &Path) -> Result<Summary, String> {
    let mut zip = ZipWriter::new(fs::File::create(file).map_err(err)?);
    let options = SimpleFileOptions::default();
    let mut summary = Summary::default();
    for entry in fs::read_dir(paths.setlists()).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some(fingerprint) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")).filter(|f| store::valid_fingerprint(f)) else { continue };
        let Some(mut saved) = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SavedSetlist>(&b).ok()) else { continue };
        saved.last_path = PathBuf::new();
        zip.start_file(format!("{SETLISTS}{fingerprint}.json"), options).map_err(err)?;
        zip.write_all(&serde_json::to_vec(&saved).map_err(err)?).map_err(err)?;
        summary.setlists += 1;
    }
    for entry in fs::read_dir(paths.waveform_cache()).into_iter().flatten().flatten() {
        let path = entry.path();
        let Some((stem, extension)) = cache_entry(&path) else { continue };
        let Ok(bytes) = fs::read(&path) else { continue };
        let ok = if extension == "rwf" { waveform_cache::parse_rekordbox(&bytes).is_some() } else { serde_json::from_slice::<Data>(&bytes).is_ok() };
        if !ok {
            continue;
        }
        zip.start_file(format!("{DJ_DATA}{stem}.{extension}"), options).map_err(err)?;
        zip.write_all(&bytes).map_err(err)?;
        if extension == "rwf" {
            summary.waveforms += 1;
        } else {
            summary.records += 1;
        }
    }
    zip.finish().map_err(err)?;
    Ok(summary)
}

/// Bring a pack in. A setlist this copy has already takes the pack's entries the way a refresh from a
/// source does, so what was edited here is kept; waveforms and records this copy has are left alone.
pub fn import(paths: &AppPaths, file: &Path) -> Result<Summary, String> {
    let mut zip = ZipArchive::new(BufReader::new(fs::File::open(file).map_err(err)?)).map_err(|_| "That is not a Nullamp data file".to_owned())?;
    if zip.len() > MAX_ENTRIES {
        return Err("That data file has too many entries".into());
    }
    let cache = paths.waveform_cache();
    let mut summary = Summary::default();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            summary.rejected += 1;
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let Some(kind) = entry.name().ok().and_then(|name| classify(&name)) else {
            summary.rejected += 1;
            continue;
        };
        let cap = match kind {
            Kind::Setlist(_) => MAX_SETLIST,
            Kind::Waveform(_) => MAX_WAVEFORM,
            Kind::Record(_) => MAX_RECORD,
        };
        let mut bytes = Vec::new();
        if Read::take(&mut entry, cap + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > cap {
            summary.rejected += 1;
            continue;
        }
        let parsed = match kind {
            Kind::Setlist(fingerprint) => import_setlist(paths, &fingerprint, &bytes, &mut summary),
            Kind::Waveform(stem) => waveform_cache::parse_rekordbox(&bytes).map(|_| put(&cache, &stem, "rwf", &bytes, &mut summary.present, &mut summary.waveforms)),
            Kind::Record(stem) => serde_json::from_slice::<Data>(&bytes).ok().map(|_| put(&cache, &stem, "rbi", &bytes, &mut summary.present, &mut summary.records)),
        };
        if parsed.is_none() {
            summary.rejected += 1;
        }
    }
    Ok(summary)
}

fn put(cache: &Path, stem: &str, extension: &str, bytes: &[u8], present: &mut usize, added: &mut usize) {
    let path = cache.join(format!("{stem}.{extension}"));
    if path.exists() {
        *present += 1;
    } else {
        waveform_cache::write_entry(&path, bytes);
        *added += 1;
    }
}

/// `None` when the entry does not hold a setlist for this fingerprint.
fn import_setlist(paths: &AppPaths, fingerprint: &str, bytes: &[u8], summary: &mut Summary) -> Option<()> {
    let mut incoming: SavedSetlist = serde_json::from_slice(bytes).ok()?;
    if incoming.fingerprint != fingerprint {
        return None;
    }
    incoming.last_path = PathBuf::new();
    match store::load_setlist(paths, fingerprint) {
        Some(mut have) => {
            have.setlist.merge_refresh(incoming.setlist);
            store::save_setlist(paths, fingerprint, Some(&have)).ok()?;
            summary.merged += 1;
        }
        None => {
            store::save_setlist(paths, fingerprint, Some(&incoming)).ok()?;
            summary.setlists += 1;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rekordbox::Analysed;
    use nullamp_audio::Waveform;
    use nullamp_core::Setlist;
    use nullamp_core::setlist::{Provenance, SetlistEntry};

    fn paths(dir: &Path) -> AppPaths {
        AppPaths { config: dir.join("config"), data: dir.join("data"), cache: dir.join("cache") }
    }

    fn analysed() -> Analysed {
        Analysed { wave: Waveform { peak: vec![1, 2, 3], rms: vec![1, 1, 2] }, colors: Some(vec![[1, 2, 3]; 3]) }
    }

    fn saved(fingerprint: &str, entries: &[(&str, bool)]) -> SavedSetlist {
        let mut setlist = Setlist::default();
        for (title, edited) in entries {
            let mut e = SetlistEntry::new("A", *title, Some(1.0), Provenance::Manual);
            e.user_edited = *edited;
            setlist.entries.push(e);
        }
        SavedSetlist { fingerprint: fingerprint.into(), last_path: "/Users/me/mix.mp3".into(), setlist }
    }

    fn data() -> Data {
        Data { key: Some("Am".into()), bpm: Some(126.0), rating: 4, color: Some("red".into()) }
    }

    fn zip_of(file: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(fs::File::create(file).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_pack_carries_setlists_waveforms_and_records_without_the_local_path() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (paths(&dir.path().join("a")), paths(&dir.path().join("b")));
        store::save_setlist(&from, "ab-12", Some(&saved("ab-12", &[("One", false), ("Two", true)]))).unwrap();
        waveform_cache::store_rekordbox(&from.waveform_cache(), "ab-12", &analysed());
        waveform_cache::store_rekordbox_data(&from.waveform_cache(), "ab-12", &data());
        // Made again on the other side, so not packed.
        waveform_cache::store_tempo(&from.waveform_cache(), "ab-12", None);

        let pack = dir.path().join("pack.zip");
        let out = export(&from, &pack).unwrap();
        assert_eq!(out, Summary { setlists: 1, waveforms: 1, records: 1, ..Default::default() });
        let got = import(&to, &pack).unwrap();
        assert_eq!(got, out);

        let setlist = store::load_setlist(&to, "ab-12").unwrap();
        assert_eq!(setlist.setlist.entries.len(), 2);
        assert_eq!(setlist.last_path, PathBuf::new());
        assert!(waveform_cache::load_rekordbox(&to.waveform_cache(), "ab-12").is_some());
        assert_eq!(waveform_cache::load_rekordbox_data(&to.waveform_cache(), "ab-12"), Some(data()));
        assert!(waveform_cache::load_tempo(&to.waveform_cache(), "ab-12").is_none());
    }

    #[test]
    fn importing_keeps_what_is_here() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (paths(&dir.path().join("a")), paths(&dir.path().join("b")));
        store::save_setlist(&from, "ab-12", Some(&saved("ab-12", &[("From the pack", false)]))).unwrap();
        waveform_cache::store_rekordbox_data(&from.waveform_cache(), "ab-12", &data());
        let mut mine = saved("ab-12", &[("Mine", true), ("Old suggestion", false)]);
        mine.last_path = "/local/mix.mp3".into();
        store::save_setlist(&to, "ab-12", Some(&mine)).unwrap();
        let other = Data { rating: 1, ..data() };
        waveform_cache::store_rekordbox_data(&to.waveform_cache(), "ab-12", &other);

        let pack = dir.path().join("pack.zip");
        export(&from, &pack).unwrap();
        let got = import(&to, &pack).unwrap();
        assert_eq!((got.setlists, got.merged, got.records, got.present), (0, 1, 0, 1));

        let setlist = store::load_setlist(&to, "ab-12").unwrap();
        let titles: Vec<&str> = setlist.setlist.entries.iter().map(|e| e.title.as_str()).collect();
        assert!(titles.contains(&"Mine") && titles.contains(&"From the pack") && !titles.contains(&"Old suggestion"), "{titles:?}");
        assert_eq!(setlist.last_path, PathBuf::from("/local/mix.mp3"));
        assert_eq!(waveform_cache::load_rekordbox_data(&to.waveform_cache(), "ab-12"), Some(other));
    }

    #[test]
    fn a_hostile_pack_writes_nothing_outside_the_fixed_names() {
        let dir = tempfile::tempdir().unwrap();
        let to = paths(&dir.path().join("b"));
        let good = serde_json::to_vec(&saved("ab-12", &[("One", false)])).unwrap();
        let mut wave = b"NRW1".to_vec();
        wave.extend_from_slice(&3u32.to_le_bytes());
        wave.extend_from_slice(&[1, 2, 3, 1, 1, 2]);
        let big = vec![b' '; MAX_RECORD as usize + 1];
        let pack = dir.path().join("pack.zip");
        zip_of(
            &pack,
            &[
                ("../evil.json", &good),
                ("setlists/../x.json", &good),
                ("setlists/..%2Fx.json", &good),
                ("setlists/ab-12.json.exe", &good),
                ("setlists/ff-99.json", &good),
                ("setlists/ab-12.json", b"not json"),
                ("djdata/not-hex-at-all-1.rwf", &wave),
                ("djdata/00000000000000ff.rwf", b"NRW1junk"),
                ("djdata/00000000000000ff.rbi", &big),
                ("djdata/00000000000000ff.exe", b"x"),
                ("djdata/00000000000000AA.rwf", &wave),
            ],
        );
        let got = import(&to, &pack).unwrap();
        // Only the well formed waveform with a hash name, which lands under the lower case name.
        assert_eq!((got.setlists, got.merged, got.waveforms, got.records), (0, 0, 1, 0));
        assert_eq!(got.rejected, 10);
        assert!(!dir.path().join("evil.json").exists());
        assert!(to.waveform_cache().join("00000000000000aa.rwf").exists());
        assert_eq!(fs::read_dir(to.waveform_cache()).unwrap().count(), 1);
        assert!(!to.setlists().exists() || fs::read_dir(to.setlists()).unwrap().count() == 0);
        assert!(import(&to, &dir.path().join("nothing.zip")).is_err());
        fs::write(dir.path().join("plain.zip"), b"hello").unwrap();
        assert_eq!(import(&to, &dir.path().join("plain.zip")), Err("That is not a Nullamp data file".into()));
    }
}
