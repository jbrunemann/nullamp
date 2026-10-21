//! Reads the Mixxx library: `mixxxdb.sqlite` in Mixxx's settings folder (`~/Library/Application
//! Support/Mixxx` on macOS, `%LOCALAPPDATA%\Mixxx` on Windows, `~/.mixxx` on Linux). A plain SQLite
//! file, so unlike rekordbox's `master.db` there is no key.
//!
//! Read-only on purpose: Mixxx owns the database and may be running while we read it.

mod analysis;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};

pub use analysis::Waveform;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Mixxx database: {0}")]
    Sql(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("unreadable Mixxx data: {0}")]
    Format(&'static str),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sql(e.to_string())
    }
}

/// One track of the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// As Mixxx names it ("Dm", "B♭m"), for `nullamp_core::Key::parse`.
    pub key: Option<String>,
    pub bpm: Option<f32>,
    pub duration_secs: f64,
    pub year: Option<u16>,
    pub track_number: u32,
    pub bitrate: u32,
    pub sample_rate: u32,
    /// Stars, 0 to 5.
    pub rating: u8,
    pub play_count: u32,
    pub comment: String,
    pub date_added: String,
    /// The audio file. It is not checked to exist.
    pub file: PathBuf,
    /// The colour label as 0xRRGGBB, `None` when unset or when the database predates colours.
    pub color: Option<u32>,
    /// The file of the analysed waveform (the overview if Mixxx has one, else the detail), in the
    /// `analysis` folder beside the database. It is not checked to exist.
    pub analysis: Option<PathBuf>,
}

impl Track {
    /// The waveform Mixxx analysed for this track. `Ok(None)` when it has none.
    pub fn waveform(&self) -> Result<Option<Waveform>, Error> {
        match &self.analysis {
            Some(file) => analysis::read(file),
            None => Ok(None),
        }
    }
}

/// What a list of the library is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Crate,
    /// A playlist, Auto DJ's queue included.
    Playlist,
    /// A past session: Mixxx records what was played as a hidden playlist named after its date.
    History,
}

/// A crate, playlist or history session. `tracks` are indices into [`Library::tracks`], in order.
#[derive(Clone, Debug)]
pub struct List {
    pub id: i64,
    pub name: String,
    pub kind: Kind,
    pub tracks: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Library {
    pub tracks: Vec<Track>,
    /// Crates first, then playlists, then history, each in Mixxx's order.
    pub lists: Vec<List>,
}

/// Mixxx declares many columns as text that hold numbers (year, track number) and the other way
/// round, so a value is read as whatever it is stored as.
fn text(r: &Row, i: usize) -> rusqlite::Result<String> {
    Ok(match r.get_ref(i)? {
        ValueRef::Text(t) => String::from_utf8_lossy(t).trim().to_owned(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(x) => x.to_string(),
        ValueRef::Null | ValueRef::Blob(_) => String::new(),
    })
}

fn number(r: &Row, i: usize) -> rusqlite::Result<f64> {
    Ok(match r.get_ref(i)? {
        ValueRef::Integer(n) => n as f64,
        ValueRef::Real(x) => x,
        ValueRef::Text(t) => String::from_utf8_lossy(t).trim().parse().unwrap_or(0.0),
        ValueRef::Null | ValueRef::Blob(_) => 0.0,
    })
}

/// The leading number of a value like "3" or "3/12" (Mixxx keeps the track number as typed).
fn leading_number(s: &str) -> u32 {
    s.split(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// The year at the start of a date value ("2018", "2018-05-01").
fn year(s: &str) -> Option<u16> {
    let y = s.get(..4)?;
    y.bytes().all(|b| b.is_ascii_digit()).then(|| y.parse().ok()).flatten().filter(|&y| y > 0)
}

impl Library {
    /// Reads the library in `db`.
    pub fn open(db: &Path) -> Result<Library, Error> {
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut tracks = Vec::new();
        // The library's id of a track, to the row it became, for the lists below.
        let mut index_of: HashMap<i64, usize> = HashMap::new();
        {
            // `library.location` names a `track_locations` row by its id, whatever the declared reference says.
            let mut stmt = conn
                .prepare(
                    "SELECT l.id, t.location, l.title, l.artist, l.album, l.genre, l.year, l.tracknumber, l.comment, \
                            l.duration, l.bitrate, l.samplerate, l.bpm, l.key, l.rating, l.timesplayed, l.datetime_added \
                     FROM library l JOIN track_locations t ON t.id = l.location \
                     WHERE COALESCE(l.mixxx_deleted, 0) = 0 ORDER BY l.id",
                )
                .map_err(Error::from)?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let file = text(r, 1)?;
                if file.is_empty() {
                    continue;
                }
                let bpm = number(r, 12)?;
                let key = text(r, 13)?;
                let id: i64 = r.get(0)?;
                index_of.insert(id, tracks.len());
                tracks.push(Track {
                    id,
                    file: PathBuf::from(file),
                    title: text(r, 2)?,
                    artist: text(r, 3)?,
                    album: text(r, 4)?,
                    genre: text(r, 5)?,
                    year: year(&text(r, 6)?),
                    track_number: leading_number(&text(r, 7)?),
                    comment: text(r, 8)?,
                    duration_secs: number(r, 9)?.max(0.0),
                    bitrate: number(r, 10)?.max(0.0) as u32,
                    sample_rate: number(r, 11)?.max(0.0) as u32,
                    // 0 is "not analysed", not a tempo.
                    bpm: (bpm > 0.0).then_some(bpm as f32),
                    key: Some(key).filter(|k| !k.is_empty()),
                    rating: number(r, 14)?.clamp(0.0, 5.0) as u8,
                    play_count: number(r, 15)?.max(0.0) as u32,
                    date_added: text(r, 16)?,
                    color: None,
                    analysis: None,
                });
            }
        }
        // Both come from tables and columns that older Mixxx versions lack, so a failure is not fatal.
        let folder = db.parent().unwrap_or(Path::new(".")).join("analysis");
        read_colors(&conn, &index_of, &mut tracks);
        read_analysis(&conn, &index_of, &folder, &mut tracks);
        let mut lists = Vec::new();
        read_lists(&conn, &index_of, &mut lists)?;
        Ok(Library { tracks, lists })
    }
}

/// The colour labels of the tracks that have one.
fn read_colors(conn: &Connection, index_of: &HashMap<i64, usize>, tracks: &mut [Track]) {
    let Ok(mut stmt) = conn.prepare("SELECT id, color FROM library WHERE color IS NOT NULL") else { return };
    let Ok(mut rows) = stmt.query([]) else { return };
    while let Ok(Some(r)) = rows.next() {
        if let (Ok(id), Ok(color)) = (r.get::<_, i64>(0), number(r, 1))
            && let Some(&i) = index_of.get(&id)
        {
            tracks[i].color = u32::try_from(color as i64).ok().filter(|&c| c <= 0xff_ffff);
        }
    }
}

/// Where the analysed waveform of each track is: `track_analysis` lists them by type, 1 for the
/// detail and 2 for the overview, and the data lives in a file named by the row's id.
fn read_analysis(conn: &Connection, index_of: &HashMap<i64, usize>, folder: &Path, tracks: &mut [Track]) {
    let Ok(mut stmt) = conn.prepare("SELECT track_id, id, CAST(type AS INTEGER) FROM track_analysis WHERE CAST(type AS INTEGER) IN (1, 2) ORDER BY id") else { return };
    let Ok(mut rows) = stmt.query([]) else { return };
    // The newest row of each type wins; the overview is preferred, being small and as wide as the timeline.
    let mut best: HashMap<usize, [Option<i64>; 2]> = HashMap::new();
    while let Ok(Some(r)) = rows.next() {
        if let (Ok(track), Ok(id), Ok(kind)) = (r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2))
            && let Some(&i) = index_of.get(&track)
        {
            best.entry(i).or_default()[kind as usize - 1] = Some(id);
        }
    }
    for (i, [detail, overview]) in best {
        tracks[i].analysis = overview.or(detail).map(|id| folder.join(id.to_string()));
    }
}

/// The entries of every list in `table` as (list id, track id) in order, grouped by list.
fn members(conn: &Connection, sql: &str, index_of: &HashMap<i64, usize>) -> Result<HashMap<i64, Vec<usize>>, Error> {
    let mut out: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        // A track Mixxx has since removed from the library is left out.
        if let Some(&i) = index_of.get(&r.get::<_, i64>(1)?) {
            out.entry(r.get(0)?).or_default().push(i);
        }
    }
    Ok(out)
}

fn read_lists(conn: &Connection, index_of: &HashMap<i64, usize>, lists: &mut Vec<List>) -> Result<(), Error> {
    // A crate with `show` off is one Mixxx keeps out of its sidebar.
    let mut in_crate = members(conn, "SELECT crate_id, track_id FROM crate_tracks ORDER BY rowid", index_of)?;
    let mut stmt = conn.prepare("SELECT id, name FROM crates WHERE COALESCE(show, 1) <> 0 ORDER BY name COLLATE NOCASE, id")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, text(r, 1)?)))? {
        let (id, name) = row?;
        lists.push(List { id, name, kind: Kind::Crate, tracks: in_crate.remove(&id).unwrap_or_default() });
    }
    let mut in_playlist = members(conn, "SELECT playlist_id, track_id FROM PlaylistTracks ORDER BY position, id", index_of)?;
    // `hidden`: 0 an ordinary playlist, 1 the Auto DJ queue, 2 a session of the play history.
    let mut stmt = conn.prepare("SELECT id, name, hidden FROM Playlists ORDER BY position, id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, text(r, 1)?, r.get::<_, i64>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
    for kind in [Kind::Playlist, Kind::History] {
        for (id, name, hidden) in &rows {
            if (*hidden == 2) == (kind == Kind::History) {
                lists.push(List { id: *id, name: name.clone(), kind, tracks: in_playlist.remove(id).unwrap_or_default() });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
