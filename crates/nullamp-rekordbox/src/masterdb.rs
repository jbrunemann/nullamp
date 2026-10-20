//! Reads the rekordbox **desktop** library: `~/Library/Pioneer/rekordbox/master.db`, a
//! SQLCipher-encrypted SQLite database. Unlike a USB stick (`export.pdb`), the desktop app keeps
//! absolute local paths to the originals, so its tracks play straight from disk, and the analysis
//! files it points at live under the `share` folder next to the database.
//!
//! Read-only on purpose, with a key the user supplies (rekordbox 7 derives it per install; we never
//! extract or store it ourselves). Opening while rekordbox runs is fine: a read-only connection sees
//! a consistent snapshot through the WAL and never writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::{Color, Error, Track};

/// A playlist (or folder) of the desktop library. `tracks` are indices into [`Collection::tracks`];
/// a folder has none of its own. `parent` is `None` for a top-level entry (`ParentID` "root").
#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub is_folder: bool,
    pub tracks: Vec<usize>,
}

/// The desktop library: every track and the playlist tree.
#[derive(Clone, Debug)]
pub struct Collection {
    pub tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sql(e.to_string())
    }
}

/// The analysis files a desktop track points at are named relative to the `share` folder
/// (`/PIONEER/USBANLZ/...`); on a stick the same path is relative to the stick's root.
fn under_share(share: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    (!relative.is_empty()).then(|| share.join(relative))
}

impl Collection {
    /// Reads the library in `master_db`, decrypting with `key` (64 hex characters). `share` is the
    /// folder that holds the analysis files, normally the `share` directory beside the database.
    pub fn open(master_db: &Path, key: &str, share: &Path) -> Result<Collection, Error> {
        // Read-only so the live database is never written, URI so `immutable` stays available if we
        // ever need it. The key must be set before any read; rekordbox uses SQLCipher 4 defaults.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(master_db, flags)?;
        // rekordbox supplies the key as a passphrase (SQLCipher 4 defaults: PBKDF2-HMAC-SHA512,
        // 256000 iterations), not as a raw key, so it is passed straight through.
        conn.pragma_update(None, "key", key.trim())?;
        // A first read proves the key: a wrong one fails here with "file is not a database".
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|_| Error::Format("wrong key or not a rekordbox database"))?;

        let mut tracks = Vec::new();
        // ContentID is text in the database; map it to the row index for the playlists below.
        let mut index_of: HashMap<String, usize> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT c.ID, c.FolderPath, c.Title, c.BPM, c.Length, c.TrackNo, c.DiscNo, \
                        c.BitRate, c.SampleRate, c.FileSize, c.Rating, c.DJPlayCount, c.ReleaseYear, \
                        c.Commnt, c.StockDate, c.AnalysisDataPath, c.ColorID, \
                        a.Name, al.Name, g.Name, l.Name, k.ScaleName \
                 FROM djmdContent c \
                 LEFT JOIN djmdArtist a  ON a.ID  = c.ArtistID \
                 LEFT JOIN djmdAlbum  al ON al.ID = c.AlbumID \
                 LEFT JOIN djmdGenre  g  ON g.ID  = c.GenreID \
                 LEFT JOIN djmdLabel  l  ON l.ID  = c.LabelID \
                 LEFT JOIN djmdKey    k  ON k.ID  = c.KeyID \
                 WHERE c.rb_local_deleted = 0 \
                 ORDER BY c.ID",
            )?;
            let text = |s: Option<String>| s.unwrap_or_default();
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let raw_id: String = r.get(0)?;
                // BPM and ColorID are hundredths of a BPM and a 1-8 colour id, both 0 when unset.
                let tempo: i64 = r.get(3).unwrap_or(0);
                let color_id: i64 = r.get::<_, Option<String>>(16)?.and_then(|s| s.parse().ok()).unwrap_or(0);
                let analysis = r.get::<_, Option<String>>(15)?.and_then(|p| under_share(share, &p));
                let track = Track {
                    id: raw_id.parse().unwrap_or(0),
                    title: text(r.get(2)?),
                    artist: text(r.get(17)?),
                    album: text(r.get(18)?),
                    genre: text(r.get(19)?),
                    label: text(r.get(20)?),
                    key: r.get::<_, Option<String>>(21)?.filter(|s| !s.is_empty()),
                    bpm: (tempo > 0).then(|| tempo as f32 / 100.0),
                    duration_secs: r.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u32,
                    year: r.get::<_, Option<i64>>(12)?.filter(|&y| y > 0).map(|y| y as u16),
                    track_number: r.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u32,
                    disc_number: r.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u16,
                    bitrate: r.get::<_, Option<i64>>(7)?.unwrap_or(0).max(0) as u32,
                    sample_rate: r.get::<_, Option<i64>>(8)?.unwrap_or(0).max(0) as u32,
                    file_size: r.get::<_, Option<i64>>(9)?.unwrap_or(0).max(0) as u32,
                    rating: (r.get::<_, Option<i64>>(10)?.unwrap_or(0).clamp(0, 5)) as u8,
                    color: u8::try_from(color_id).ok().and_then(Color::from_id),
                    play_count: r.get::<_, Option<i64>>(11)?.unwrap_or(0).clamp(0, i64::from(u16::MAX)) as u16,
                    comment: text(r.get(13)?),
                    date_added: text(r.get(14)?),
                    file: PathBuf::from(text(r.get(1)?)),
                    analysis,
                    artwork: None,
                };
                index_of.insert(raw_id, tracks.len());
                tracks.push(track);
            }
        }

        let playlists = Self::read_playlists(&conn, &index_of)?;
        Ok(Collection { tracks, playlists })
    }

    fn read_playlists(conn: &Connection, index_of: &HashMap<String, usize>) -> Result<Vec<Playlist>, Error> {
        // The entries of each playlist, in track order, as indices into `tracks`.
        let mut members: HashMap<String, Vec<usize>> = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT PlaylistID, ContentID FROM djmdSongPlaylist WHERE rb_local_deleted = 0 ORDER BY TrackNo")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let playlist: String = r.get(0)?;
                let content: String = r.get(1)?;
                if let Some(&i) = index_of.get(&content) {
                    members.entry(playlist).or_default().push(i);
                }
            }
        }
        let mut stmt = conn.prepare("SELECT ID, Name, ParentID, Attribute FROM djmdPlaylist WHERE rb_local_deleted = 0 ORDER BY Seq")?;
        let playlists = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                let parent: Option<String> = r.get(2)?;
                // Attribute 1 marks a folder; 0 an ordinary playlist, 4 a smart one.
                let is_folder = r.get::<_, i64>(3)? == 1;
                Ok(Playlist {
                    tracks: members.remove(&id).unwrap_or_default(),
                    id,
                    name: r.get(1)?,
                    parent: parent.filter(|p| p != "root"),
                    is_folder,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(playlists)
    }
}
