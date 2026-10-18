//! Metadata reading and writing for Nullamp.
//!
//! - [`read`] returns everything the inspector shows: editable tag fields, read-only
//!   technical properties, artwork, embedded chapters and embedded cue sheets.
//! - [`read_track_info`] is the cheap variant used to fill playlist rows.
//! - [`write`] applies an [`Edit`] safely: it never modifies the original file in
//!   place. See [`write`] for the procedure.
//!
//! Tags are handled by `lofty` for all formats. ID3v2 chapter frames (CHAP/CTOC) are
//! read with the `id3` crate, which models them fully.

mod chapters;
mod write;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::{FileType, TaggedFile};
use lofty::picture::{PictureInformation, PictureType};
use lofty::prelude::*;
use lofty::tag::{ItemValue, TagType};
use nullamp_core::TrackInfo;

pub use chapters::{Chapter, chapters_to_setlist, read_chapters};
pub use write::{Edit, NewArtwork, WriteOptions, WriteReport, restore_backup, write};

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("file not found")]
    NotFound,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read tags: {0}")]
    Parse(String),
    #[error("the file changed on disk since it was opened; reload and try again")]
    ModifiedExternally,
    #[error("this file type does not support tags")]
    Unsupported,
    #[error("verification failed after writing: {0}")]
    Verification(String),
    #[error("invalid value for {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

macro_rules! from_lofty_error {
    ($($t:ty),*) => {$(
        impl From<$t> for MetaError {
            fn from(e: $t) -> Self {
                MetaError::Parse(e.to_string())
            }
        }
    )*};
}
from_lofty_error!(
    lofty::error::FileParseError,
    lofty::error::FileEncodingError,
    lofty::error::TagParseError,
    lofty::error::TagEncodingError
);

/// Size and modification time, used to detect changes made by other programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> std::io::Result<FileStamp> {
    let m = std::fs::metadata(path)?;
    Ok(FileStamp { len: m.len(), modified: m.modified().ok() })
}

/// The editable tag fields, independent of tag format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Date,
    Genre,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    Composer,
    Comment,
    Description,
    Copyright,
    ArtistUrl,
    FileUrl,
}

impl Field {
    pub const ALL: [Field; 16] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Date,
        Field::Genre,
        Field::TrackNumber,
        Field::TrackTotal,
        Field::DiscNumber,
        Field::DiscTotal,
        Field::Composer,
        Field::Comment,
        Field::Description,
        Field::Copyright,
        Field::ArtistUrl,
        Field::FileUrl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Artist => "Artist",
            Field::Album => "Album",
            Field::AlbumArtist => "Album artist",
            Field::Date => "Date / year",
            Field::Genre => "Genre",
            Field::TrackNumber => "Track",
            Field::TrackTotal => "of tracks",
            Field::DiscNumber => "Disc",
            Field::DiscTotal => "of discs",
            Field::Composer => "Composer",
            Field::Comment => "Comment",
            Field::Description => "Description",
            Field::Copyright => "Copyright",
            Field::ArtistUrl => "Artist URL",
            Field::FileUrl => "File URL",
        }
    }

    pub(crate) fn item_key(self) -> ItemKey {
        match self {
            Field::Title => ItemKey::TrackTitle,
            Field::Artist => ItemKey::TrackArtist,
            Field::Album => ItemKey::AlbumTitle,
            Field::AlbumArtist => ItemKey::AlbumArtist,
            Field::Date => ItemKey::RecordingDate,
            Field::Genre => ItemKey::Genre,
            Field::TrackNumber => ItemKey::TrackNumber,
            Field::TrackTotal => ItemKey::TrackTotal,
            Field::DiscNumber => ItemKey::DiscNumber,
            Field::DiscTotal => ItemKey::DiscTotal,
            Field::Composer => ItemKey::Composer,
            Field::Comment => ItemKey::Comment,
            Field::Description => ItemKey::Description,
            Field::Copyright => ItemKey::CopyrightMessage,
            Field::ArtistUrl => ItemKey::TrackArtistUrl,
            Field::FileUrl => ItemKey::AudioFileUrl,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Field::TrackNumber | Field::TrackTotal | Field::DiscNumber | Field::DiscTotal)
    }

    pub fn is_multiline(self) -> bool {
        matches!(self, Field::Comment | Field::Description)
    }

    /// Check a value before writing. Empty values mean "remove".
    pub fn validate(self, value: &str) -> Result<(), String> {
        let v = value.trim();
        if v.is_empty() {
            return Ok(());
        }
        if v.contains('\0') {
            return Err("contains a NUL character".into());
        }
        if self.is_numeric() {
            return match v.parse::<u32>() {
                Ok(n) if n > 0 && n < 10_000 => Ok(()),
                _ => Err("must be a whole number between 1 and 9999".into()),
            };
        }
        if self == Field::Date && !valid_date(v) {
            return Err("use YYYY, YYYY-MM or YYYY-MM-DD".into());
        }
        if matches!(self, Field::ArtistUrl | Field::FileUrl) && !(v.starts_with("http://") || v.starts_with("https://")) {
            return Err("must start with http:// or https://".into());
        }
        if !self.is_multiline() && v.contains(['\n', '\r']) {
            return Err("must be a single line".into());
        }
        Ok(())
    }
}

fn valid_date(v: &str) -> bool {
    let parts: Vec<&str> = v.split('-').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    match parts.as_slice() {
        [y] => digits(y, 4),
        [y, m] => digits(y, 4) && digits(m, 2) && (1..=12).contains(&m.parse::<u8>().unwrap_or(0)),
        [y, m, d] => {
            digits(y, 4)
                && digits(m, 2)
                && digits(d, 2)
                && (1..=12).contains(&m.parse::<u8>().unwrap_or(0))
                && (1..=31).contains(&d.parse::<u8>().unwrap_or(0))
        }
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct Artwork {
    pub kind: ArtworkKind,
    pub mime: Option<String>,
    pub description: Option<String>,
    pub data: Arc<[u8]>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// The subset of ID3/FLAC picture types offered in the UI. Others are preserved
/// and shown as `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtworkKind {
    FrontCover,
    BackCover,
    Artist,
    Other,
}

impl ArtworkKind {
    pub fn label(self) -> &'static str {
        match self {
            ArtworkKind::FrontCover => "Front cover",
            ArtworkKind::BackCover => "Back cover",
            ArtworkKind::Artist => "Artist",
            ArtworkKind::Other => "Other",
        }
    }

    fn from_lofty(t: PictureType) -> Self {
        match t {
            PictureType::CoverFront => ArtworkKind::FrontCover,
            PictureType::CoverBack => ArtworkKind::BackCover,
            PictureType::Artist | PictureType::LeadArtist => ArtworkKind::Artist,
            _ => ArtworkKind::Other,
        }
    }

    pub(crate) fn to_lofty(self) -> PictureType {
        match self {
            ArtworkKind::FrontCover => PictureType::CoverFront,
            ArtworkKind::BackCover => PictureType::CoverBack,
            ArtworkKind::Artist => PictureType::Artist,
            ArtworkKind::Other => PictureType::Other,
        }
    }
}

/// Read-only properties derived from the audio stream and container.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TechnicalInfo {
    pub format: String,
    pub duration: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub audio_bitrate_kbps: Option<u32>,
    /// MP3 only: whether a Xing/VBRI header marks the file as variable bitrate.
    pub vbr: Option<bool>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub bit_depth: Option<u8>,
    pub file_size: u64,
    /// Tags present in the file, e.g. "ID3v2.3", "ID3v1", "APE".
    pub tag_types: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub stamp: FileStamp,
    pub fields: BTreeMap<Field, String>,
    /// Tag items not covered by [`Field`], for display only (they are preserved).
    pub other_items: Vec<(String, String)>,
    pub artwork: Vec<Artwork>,
    pub technical: TechnicalInfo,
    pub chapters: Vec<Chapter>,
    /// A cue sheet stored inside a tag (CUESHEET item, or a comment that is one).
    pub embedded_cue: Option<String>,
    /// Whether the format supports writing tags at all.
    pub writable: bool,
}

impl FileMetadata {
    pub fn track_info(&self) -> TrackInfo {
        TrackInfo {
            title: self.fields.get(&Field::Title).cloned(),
            artist: self.fields.get(&Field::Artist).cloned(),
            album: self.fields.get(&Field::Album).cloned(),
            duration: self.technical.duration,
        }
    }

    pub fn front_cover(&self) -> Option<&Artwork> {
        self.artwork.iter().find(|a| a.kind == ArtworkKind::FrontCover).or(self.artwork.first())
    }
}

fn parse_options(cover_art: bool) -> ParseOptions {
    ParseOptions::new().parsing_mode(ParsingMode::Relaxed).read_cover_art(cover_art)
}

fn open_tagged(path: &Path, cover_art: bool) -> Result<TaggedFile, MetaError> {
    if !path.exists() {
        return Err(MetaError::NotFound);
    }
    let probe = lofty::probe::Probe::open(path)?.options(parse_options(cover_art)).guess_file_type()?;
    if probe.file_type().is_none() {
        return Err(MetaError::Unsupported);
    }
    Ok(probe.read()?)
}

/// Read everything the metadata inspector shows.
pub fn read(path: &Path) -> Result<FileMetadata, MetaError> {
    let stamp = stamp(path)?;
    let tagged = open_tagged(path, true)?;
    let props = tagged.properties();

    let mut fields = BTreeMap::new();
    let mut other_items = Vec::new();
    let mut artwork = Vec::new();
    let mut embedded_cue = None;
    let known: Vec<ItemKey> = Field::ALL.iter().map(|f| f.item_key()).collect();

    // Fields come from the primary tag first, then any other tag (e.g. ID3v1).
    let mut tags: Vec<&lofty::tag::Tag> = tagged.tags().iter().collect();
    let primary = tagged.primary_tag_type();
    tags.sort_by_key(|t| t.tag_type() != primary);
    for tag in &tags {
        for field in Field::ALL {
            if let Some(v) = tag.get_string(field.item_key())
                && !v.trim().is_empty()
            {
                fields.entry(field).or_insert_with(|| v.to_owned());
            }
        }
        for item in tag.items() {
            let text = match item.value() {
                ItemValue::Text(t) | ItemValue::Locator(t) => t.clone(),
                ItemValue::Binary(b) => format!("<{} bytes>", b.len()),
            };
            if looks_like_cue(&text) && embedded_cue.is_none() {
                embedded_cue = Some(text.clone());
            }
            if known.contains(&item.key()) {
                continue;
            }
            let key = format!("{:?}", item.key());
            if !other_items.iter().any(|(k, _)| *k == key) {
                other_items.push((key, truncate(&text, 500)));
            }
        }
        if tag.tag_type() == primary || artwork.is_empty() {
            for pic in tag.pictures() {
                let info = PictureInformation::from_picture(pic).ok();
                artwork.push(Artwork {
                    kind: ArtworkKind::from_lofty(pic.pic_type()),
                    mime: pic.mime_type().map(|m| m.as_str().to_owned()),
                    description: pic.description().map(str::to_owned).filter(|s| !s.is_empty()),
                    data: Arc::from(pic.data()),
                    width: info.as_ref().map(|i| i.width).filter(|w| *w > 0),
                    height: info.as_ref().map(|i| i.height).filter(|h| *h > 0),
                });
            }
        }
    }

    let is_mp3 = tagged.file_type() == FileType::Mpeg;
    let chapters = if is_mp3 || tagged.file_type() == FileType::Aiff || tagged.file_type() == FileType::Wav {
        read_chapters(path).unwrap_or_default()
    } else {
        Vec::new()
    };
    if id3v2_major_version(path).is_some()
        && let Ok(id3_items) = chapters::other_id3_frames(path)
    {
        // lofty's generic view omits frames it has no mapping for (custom TXXX, PRIV,
        // ...). They are preserved on write either way; list them all for display.
        other_items = id3_items;
    }
    let duration = props.duration().as_secs_f64();
    let technical = TechnicalInfo {
        format: format_name(tagged.file_type()),
        duration: (duration > 0.0).then_some(duration),
        bitrate_kbps: props.overall_bitrate(),
        audio_bitrate_kbps: props.audio_bitrate(),
        vbr: if is_mp3 { detect_mp3_vbr(path) } else { None },
        sample_rate: props.sample_rate(),
        channels: props.channels(),
        bit_depth: props.bit_depth(),
        file_size: stamp.len,
        tag_types: tag_type_names(path, &tagged),
    };
    let writable = tagged.tag_support(tagged.primary_tag_type()).is_writable();

    Ok(FileMetadata {
        path: path.to_path_buf(),
        stamp,
        fields,
        other_items,
        artwork,
        technical,
        chapters,
        embedded_cue,
        writable,
    })
}

/// Cheap read for playlist rows: title, artist, album, duration. No artwork.
pub fn read_track_info(path: &Path) -> Result<TrackInfo, MetaError> {
    let tagged = open_tagged(path, false)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let get = |k: ItemKey| tag.and_then(|t| t.get_string(k)).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let duration = tagged.properties().duration().as_secs_f64();
    Ok(TrackInfo {
        title: get(ItemKey::TrackTitle),
        artist: get(ItemKey::TrackArtist),
        album: get(ItemKey::AlbumTitle),
        duration: (duration > 0.0).then_some(duration),
    })
}

/// Only the embedded artwork, for the now-playing display.
pub fn read_front_cover(path: &Path) -> Result<Option<Artwork>, MetaError> {
    let tagged = open_tagged(path, true)?;
    let mut best: Option<&lofty::picture::Picture> = None;
    for tag in tagged.tags() {
        for pic in tag.pictures() {
            if best.is_none() || pic.pic_type() == PictureType::CoverFront {
                best = Some(pic);
            }
        }
    }
    Ok(best.map(|pic| Artwork {
        kind: ArtworkKind::from_lofty(pic.pic_type()),
        mime: pic.mime_type().map(|m| m.as_str().to_owned()),
        description: None,
        data: Arc::from(pic.data()),
        width: None,
        height: None,
    }))
}

fn looks_like_cue(text: &str) -> bool {
    let upper = text.len() > 20 && text.contains("TRACK ") && text.contains("INDEX 01");
    upper && text.lines().count() >= 3
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn format_name(t: FileType) -> String {
    match t {
        FileType::Mpeg => "MP3 (MPEG audio)".into(),
        FileType::Flac => "FLAC".into(),
        FileType::Vorbis => "Ogg Vorbis".into(),
        FileType::Opus => "Ogg Opus".into(),
        FileType::Mp4 => "MP4 / M4A".into(),
        FileType::Wav => "WAV".into(),
        FileType::Aiff => "AIFF".into(),
        FileType::Aac => "AAC (ADTS)".into(),
        other => format!("{other:?}"),
    }
}

fn tag_type_names(path: &Path, tagged: &TaggedFile) -> Vec<String> {
    tagged
        .tags()
        .iter()
        .map(|t| match t.tag_type() {
            TagType::Id3v2 => match id3v2_major_version(path) {
                Some(v) => format!("ID3v2.{v}"),
                None => "ID3v2".into(),
            },
            TagType::Id3v1 => "ID3v1".into(),
            TagType::Ape => "APE".into(),
            TagType::VorbisComments => "Vorbis comments".into(),
            TagType::Mp4Ilst => "MP4 ilst".into(),
            TagType::RiffInfo => "RIFF INFO".into(),
            TagType::AiffText => "AIFF text".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Major version of a leading ID3v2 tag (2, 3 or 4).
pub(crate) fn id3v2_major_version(path: &Path) -> Option<u8> {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    (&head[..3] == b"ID3").then_some(head[3])
}

/// Look for a Xing (VBR) or Info (CBR) header in the first MPEG frame.
fn detect_mp3_vbr(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 10];
    f.read_exact(&mut header).ok()?;
    let mut offset = 0u64;
    if &header[..3] == b"ID3" {
        let size = header[6..10].iter().fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7F));
        let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
        offset = 10 + size + footer;
    }
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    let buf = &buf[..n];
    // First frame sync.
    let sync = buf.windows(2).position(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0)?;
    let frame = &buf[sync..buf.len().min(sync + 2048)];
    if frame.windows(4).any(|w| w == b"Xing" || w == b"VBRI") {
        Some(true)
    } else if frame.windows(4).any(|w| w == b"Info") {
        Some(false)
    } else {
        // No header at all: almost always a constant-bitrate encode.
        Some(false)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    pub fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
    }

    /// Copy a fixture into a temporary directory so tests can modify it.
    pub fn scratch_copy(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join(name);
        std::fs::copy(fixture(name), &dst).unwrap();
        (dir, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::fixture;
    use super::*;

    #[test]
    fn reads_tagged_mp3() {
        let m = read(&fixture("tagged_v23.mp3")).unwrap();
        assert_eq!(m.fields.get(&Field::Title).map(String::as_str), Some("Fixture Tone"));
        assert_eq!(m.fields.get(&Field::Artist).map(String::as_str), Some("Nullamp Tests"));
        assert_eq!(m.fields.get(&Field::AlbumArtist).map(String::as_str), Some("Various"));
        assert_eq!(m.fields.get(&Field::TrackNumber).map(String::as_str), Some("3"));
        assert_eq!(m.fields.get(&Field::TrackTotal).map(String::as_str), Some("12"));
        assert_eq!(m.fields.get(&Field::Genre).map(String::as_str), Some("Techno"));
        assert!(m.other_items.iter().any(|(_, v)| v == "keep me"), "{:?}", m.other_items);
        assert_eq!(m.technical.tag_types, ["ID3v2.3"]);
        assert_eq!(m.technical.sample_rate, Some(44100));
        assert!((m.technical.duration.unwrap() - 3.0).abs() < 0.1);
        assert_eq!(m.technical.vbr, Some(false));
        assert_eq!(m.artwork.len(), 1);
        let art = &m.artwork[0];
        assert_eq!(art.kind, ArtworkKind::FrontCover);
        assert_eq!(art.mime.as_deref(), Some("image/jpeg"));
        assert_eq!((art.width, art.height), (Some(64), Some(64)));
        assert!(m.writable);
    }

    #[test]
    fn reads_other_formats() {
        for (name, title, format) in [
            ("tone.flac", "Flac Tone", "FLAC"),
            ("tone.ogg", "Ogg Tone", "Ogg Vorbis"),
            ("tone.m4a", "M4A Tone", "MP4 / M4A"),
        ] {
            let m = read(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(m.fields.get(&Field::Title).map(String::as_str), Some(title), "{name}");
            assert_eq!(m.technical.format, format);
            assert!((m.technical.duration.unwrap() - 2.0).abs() < 0.1, "{name}");
        }
        let wav = read(&fixture("tone_mono_22k.wav")).unwrap();
        assert_eq!(wav.technical.channels, Some(1));
        assert_eq!(wav.technical.sample_rate, Some(22050));
    }

    #[test]
    fn track_info_and_errors() {
        let info = read_track_info(&fixture("tagged_v24.mp3")).unwrap();
        assert_eq!(info.title.as_deref(), Some("V24 Tone"));
        assert!(matches!(read_track_info(&fixture("nope.mp3")), Err(MetaError::NotFound)));
        assert!(read(&fixture("corrupt.mp3")).is_err());
    }

    #[test]
    fn embedded_chapters_are_reported() {
        let m = read(&fixture("mix_chapters.mp3")).unwrap();
        assert_eq!(m.chapters.len(), 3);
        assert_eq!(m.chapters[1].title.as_deref(), Some("Middle Track"));
    }

    #[test]
    fn field_validation() {
        assert!(Field::TrackNumber.validate("12").is_ok());
        assert!(Field::TrackNumber.validate("x").is_err());
        assert!(Field::TrackNumber.validate("0").is_err());
        assert!(Field::Date.validate("2024-02-30").is_ok(), "day range only, not calendar");
        assert!(Field::Date.validate("2024-13").is_err());
        assert!(Field::Date.validate("24").is_err());
        assert!(Field::ArtistUrl.validate("ftp://x").is_err());
        assert!(Field::Title.validate("a\nb").is_err());
        assert!(Field::Comment.validate("a\nb").is_ok());
        assert!(Field::Title.validate("").is_ok());
    }
}
