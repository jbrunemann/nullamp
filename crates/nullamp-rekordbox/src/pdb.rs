//! Reader for `export.pdb`, the DeviceSQL database rekordbox writes to a USB stick.
//!
//! Layout after Deep Symmetry's "rekordbox export analysis" and the `rekordcrate` sources:
//! fixed-size pages, each table a linked list of pages, rows found through a footer of
//! 16-row groups. Every read is bounds checked; a damaged file is an error, never a panic.

use std::collections::HashMap;

use crate::Error;

const HEADER_LEN: usize = 0x28;
const GROUP_LEN: usize = 36;

const TRACKS: u32 = 0;
const GENRES: u32 = 1;
const ARTISTS: u32 = 2;
const ALBUMS: u32 = 3;
const LABELS: u32 = 4;
const KEYS: u32 = 5;
const ARTWORK: u32 = 13;

/// One row of the tracks table, ids still unresolved.
#[derive(Debug, Default)]
pub(crate) struct RawTrack {
    pub id: u32,
    pub sample_rate: u32,
    pub file_size: u32,
    pub artwork_id: u32,
    pub key_id: u32,
    pub label_id: u32,
    pub bitrate: u32,
    pub track_number: u32,
    pub tempo: u32,
    pub genre_id: u32,
    pub album_id: u32,
    pub artist_id: u32,
    pub disc_number: u16,
    pub play_count: u16,
    pub year: u16,
    pub duration: u16,
    pub rating: u8,
    pub color: u8,
    pub date_added: String,
    pub analyze_path: String,
    pub comment: String,
    pub title: String,
    pub file_path: String,
}

#[derive(Debug, Default)]
pub(crate) struct Tables {
    pub tracks: Vec<RawTrack>,
    pub artists: HashMap<u32, String>,
    pub albums: HashMap<u32, String>,
    pub genres: HashMap<u32, String>,
    pub labels: HashMap<u32, String>,
    pub keys: HashMap<u32, String>,
    pub artwork: HashMap<u32, String>,
}

fn u8_at(d: &[u8], o: usize) -> Option<u8> {
    d.get(o).copied()
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// A DeviceSQL string at `o` in `row`: a short ASCII form (length in the first byte's upper
/// bits, lowest bit set) or a long form with a 4-byte header (flags `0x40` UTF-8, `0x90` UTF-16LE).
fn string_at(row: &[u8], o: usize) -> Option<String> {
    let first = u8_at(row, o)?;
    if first & 1 == 1 {
        let len = usize::from(first >> 1).checked_sub(1)?;
        let bytes = row.get(o + 1..o + 1 + len)?;
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    let total = usize::from(u16_at(row, o + 1)?);
    let body = row.get(o + 4..o + total)?;
    match first {
        0x40 => Some(String::from_utf8_lossy(body).into_owned()),
        0x90 => {
            let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// Rows of one page, each as the slice from its start to the end of the page.
pub(crate) fn page_rows(page: &[u8]) -> Vec<&[u8]> {
    let flags = page.get(27).copied().unwrap_or(0x40);
    if flags & 0x40 != 0 {
        return Vec::new();
    }
    let small = usize::from(page.get(24).copied().unwrap_or(0));
    let large = u16_at(page, 34).map_or(0, usize::from);
    let count = if large > small && large != 0x1fff { large } else { small };
    let mut rows = Vec::with_capacity(count);
    for group in 0..count.div_ceil(16) {
        let Some(end) = page.len().checked_sub(group * GROUP_LEN) else { break };
        let Some(present) = end.checked_sub(4).and_then(|o| u16_at(page, o)) else { break };
        for i in 0..16usize.min(count - group * 16) {
            if present & (1 << i) == 0 {
                continue;
            }
            let offset = end.checked_sub(4 + 2 * (i + 1)).and_then(|o| u16_at(page, o));
            if let Some(row) = offset.and_then(|off| page.get(HEADER_LEN + usize::from(off)..)) {
                rows.push(row);
            }
        }
    }
    rows
}

fn track(row: &[u8]) -> Option<RawTrack> {
    // The 21 string offsets start after the fixed fields; title, filename and file path are the last ones.
    let string = |n: usize| string_at(row, usize::from(u16_at(row, 94 + 2 * n)?));
    Some(RawTrack {
        sample_rate: u32_at(row, 8)?,
        file_size: u32_at(row, 16)?,
        artwork_id: u32_at(row, 28)?,
        key_id: u32_at(row, 32)?,
        label_id: u32_at(row, 40)?,
        bitrate: u32_at(row, 48)?,
        track_number: u32_at(row, 52)?,
        tempo: u32_at(row, 56)?,
        genre_id: u32_at(row, 60)?,
        album_id: u32_at(row, 64)?,
        artist_id: u32_at(row, 68)?,
        id: u32_at(row, 72)?,
        disc_number: u16_at(row, 76)?,
        play_count: u16_at(row, 78)?,
        year: u16_at(row, 80)?,
        duration: u16_at(row, 84)?,
        color: u8_at(row, 88)?,
        rating: u8_at(row, 89)?,
        date_added: string(10)?,
        analyze_path: string(14)?,
        comment: string(16)?,
        title: string(17)?,
        file_path: string(20)?,
    })
}

/// Id and name of the simple lookup tables; `name_at` is where the string starts in the row.
fn named(row: &[u8], id_at: usize, name_at: usize) -> Option<(u32, String)> {
    Some((u32_at(row, id_at)?, string_at(row, name_at)?))
}

fn artist(row: &[u8]) -> Option<(u32, String)> {
    let far = u16_at(row, 0)? == 0x64;
    let name = if far { usize::from(u16_at(row, 10)?) } else { usize::from(u8_at(row, 9)?) };
    Some((u32_at(row, 4)?, string_at(row, name)?))
}

fn album(row: &[u8]) -> Option<(u32, String)> {
    Some((u32_at(row, 12)?, string_at(row, usize::from(u8_at(row, 21)?))?))
}

pub(crate) fn read(data: &[u8]) -> Result<Tables, Error> {
    let bad = |what: &'static str| Error::Format(what);
    let page_size = usize::try_from(u32_at(data, 4).ok_or(bad("truncated header"))?).map_err(|_| bad("page size"))?;
    let tables = u32_at(data, 8).ok_or(bad("truncated header"))?;
    if !(512..=1 << 20).contains(&page_size) || tables > 64 {
        return Err(bad("not an export.pdb"));
    }
    let pages = data.len() / page_size;
    let mut out = Tables::default();
    for t in 0..usize::try_from(tables).map_err(|_| bad("table count"))? {
        let at = 28 + 16 * t;
        let (Some(kind), Some(first), Some(last)) = (u32_at(data, at), u32_at(data, at + 8), u32_at(data, at + 12)) else {
            return Err(bad("truncated table list"));
        };
        let mut index = first;
        // A table that never ends would otherwise loop forever on a damaged file.
        for _ in 0..pages {
            let start = usize::try_from(index).ok().and_then(|i| i.checked_mul(page_size));
            let Some(page) = start.and_then(|s| data.get(s..s + page_size)) else { break };
            for row in page_rows(page) {
                match kind {
                    TRACKS => out.tracks.extend(track(row)),
                    GENRES => out.genres.extend(named(row, 0, 4)),
                    ARTISTS => out.artists.extend(artist(row)),
                    ALBUMS => out.albums.extend(album(row)),
                    LABELS => out.labels.extend(named(row, 0, 4)),
                    KEYS => out.keys.extend(named(row, 0, 8)),
                    ARTWORK => out.artwork.extend(named(row, 0, 4)),
                    _ => {}
                }
            }
            if index == last {
                break;
            }
            index = u32_at(page, 12).unwrap_or(last);
        }
    }
    Ok(out)
}
