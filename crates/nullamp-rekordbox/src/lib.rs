//! Reads what rekordbox writes to a USB stick: the track library (`PIONEER/rekordbox/export.pdb`)
//! and the waveforms in the analysis files. Read-only on purpose: the stick's database and the
//! files it lists must stay in step, and only rekordbox may change them.

mod anlz;
mod pdb;

use std::path::{Path, PathBuf};

pub use anlz::Waveform;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("unreadable rekordbox data: {0}")]
    Format(&'static str),
}

/// One track of the stick's library.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub label: String,
    /// As rekordbox names it ("Am", "Bbm"), for `nullamp_core::Key::parse`.
    pub key: Option<String>,
    pub bpm: Option<f32>,
    pub duration_secs: u32,
    pub year: Option<u16>,
    pub track_number: u32,
    pub disc_number: u16,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub file_size: u32,
    pub rating: u8,
    pub play_count: u16,
    pub comment: String,
    pub date_added: String,
    /// The audio file on the stick. It is not checked to exist.
    pub file: PathBuf,
    /// The analysis file (`ANLZ0000.DAT`), absent for tracks rekordbox never analysed.
    pub analysis: Option<PathBuf>,
    pub artwork: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Stick {
    pub root: PathBuf,
    pub tracks: Vec<Track>,
}

fn database(root: &Path) -> PathBuf {
    root.join("PIONEER").join("rekordbox").join("export.pdb")
}

/// Paths in the database are absolute on the stick (`/Contents/...`).
fn on_stick(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    (!relative.is_empty()).then(|| root.join(relative))
}

/// The stick that holds `file`: the nearest folder above it with an export.
pub fn stick_root(file: &Path) -> Option<PathBuf> {
    file.ancestors().skip(1).find(|dir| database(dir).is_file()).map(Path::to_path_buf)
}

/// A path as a string that compares equal however its accents are encoded. The database holds
/// "ë" as one character while a FAT or exFAT volume lists it as "e" and a combining mark.
pub fn path_key(path: &Path) -> String {
    use unicode_normalization::UnicodeNormalization;
    path.to_string_lossy().nfc().collect()
}

/// Mounted volumes under `volumes` (`/Volumes` on macOS) that hold a rekordbox export.
pub fn detect(volumes: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(volumes) else { return Vec::new() };
    let mut found: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| database(p).is_file()).collect();
    found.sort();
    found
}

impl Stick {
    /// Reads the library of the stick mounted at `root`.
    pub fn open(root: &Path) -> Result<Stick, Error> {
        let tables = pdb::read(&std::fs::read(database(root))?)?;
        let name = |map: &std::collections::HashMap<u32, String>, id: u32| map.get(&id).cloned().unwrap_or_default();
        let tracks = tables
            .tracks
            .iter()
            .map(|t| Track {
                id: t.id,
                title: t.title.clone(),
                artist: name(&tables.artists, t.artist_id),
                album: name(&tables.albums, t.album_id),
                genre: name(&tables.genres, t.genre_id),
                label: name(&tables.labels, t.label_id),
                key: tables.keys.get(&t.key_id).cloned(),
                // Stored as hundredths of a BPM; 0 means not analysed.
                bpm: (t.tempo > 0).then(|| t.tempo as f32 / 100.0),
                duration_secs: u32::from(t.duration),
                year: (t.year > 0).then_some(t.year),
                track_number: t.track_number,
                disc_number: t.disc_number,
                bitrate: t.bitrate,
                sample_rate: t.sample_rate,
                file_size: t.file_size,
                rating: t.rating,
                play_count: t.play_count,
                comment: t.comment.clone(),
                date_added: t.date_added.clone(),
                file: on_stick(root, &t.file_path).unwrap_or_default(),
                analysis: on_stick(root, &t.analyze_path),
                artwork: tables.artwork.get(&t.artwork_id).and_then(|p| on_stick(root, p)),
            })
            .collect();
        Ok(Stick { root: root.to_path_buf(), tracks })
    }
}

impl Track {
    /// The waveform rekordbox analysed for this track: the detail waveform from the `.EXT`
    /// file, else the preview from the `.DAT`. `Ok(None)` when the track has no analysis.
    pub fn waveform(&self) -> Result<Option<Waveform>, Error> {
        let Some(dat) = &self.analysis else { return Ok(None) };
        match std::fs::read(dat.with_extension("EXT")) {
            Ok(ext) => {
                if let Some(wave) = anlz::detail(&ext)? {
                    return Ok(Some(wave));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        match std::fs::read(dat) {
            Ok(bytes) => anlz::preview(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests;
