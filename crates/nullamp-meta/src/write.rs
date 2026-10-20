//! Safe tag writing.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::WriteOptions as LoftyWriteOptions;
use lofty::picture::{MimeType, Picture};
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use nullamp_core::Setlist;

use crate::chapters::apply_chapters;
use crate::{Artwork, ArtworkKind, Field, FileStamp, MetaError, id3v2_major_version, open_tagged, read_chapters, stamp};

#[derive(Clone, Debug)]
pub struct NewArtwork {
    pub kind: ArtworkKind,
    pub mime: Option<String>,
    pub description: Option<String>,
    pub data: Arc<[u8]>,
}

impl From<&Artwork> for NewArtwork {
    fn from(a: &Artwork) -> Self {
        NewArtwork { kind: a.kind, mime: a.mime.clone(), description: a.description.clone(), data: Arc::clone(&a.data) }
    }
}

/// A set of changes. Only what is listed is touched; everything else in the file
/// (other fields, unknown frames, chapters, artwork) is preserved.
#[derive(Clone, Debug, Default)]
pub struct Edit {
    /// `Some(value)` sets a field, `None` or an empty value removes it.
    pub fields: BTreeMap<Field, Option<String>>,
    /// Replace all artwork with this list. `None` leaves artwork untouched.
    pub artwork: Option<Vec<NewArtwork>>,
    /// Replace embedded chapters (ID3v2 files only). `None` leaves them untouched.
    pub chapters: Option<Setlist>,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.artwork.is_none() && self.chapters.is_none()
    }
}

#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    /// Save the original here before replacing it: the tags of an MP3, the whole file otherwise.
    pub backup_dir: Option<PathBuf>,
    /// Total size the backup directory may use; the oldest backups are deleted first.
    pub backup_budget_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct WriteReport {
    pub stamp: FileStamp,
    pub backup: Option<PathBuf>,
    /// Why no backup was made, when one was requested.
    pub backup_skipped: Option<String>,
    /// Fields this file's tag format cannot store.
    pub unsupported: Vec<Field>,
}

/// Removes the temporary file unless disarmed.
struct TempGuard(Option<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.nullamp-tmp"))
}

/// Apply `edit` to the file at `path`.
///
/// The procedure never leaves a half-written file behind:
/// 1. validate all values;
/// 2. refuse if the file changed since `expected` was taken (external edits);
/// 3. copy the file to a hidden temporary file in the same directory;
/// 4. write the tags to the copy (keeping the original ID3v2 version);
/// 5. verify the copy: tags read back as written, audio properties unchanged;
/// 6. check again that the original was not modified meanwhile;
/// 7. optionally back up the original (its tags for an MP3, else a full copy);
/// 8. atomically rename the copy over the original.
pub fn write(path: &Path, expected: FileStamp, edit: &Edit, opts: &WriteOptions) -> Result<WriteReport, MetaError> {
    for (field, value) in &edit.fields {
        if let Some(v) = value {
            field.validate(v).map_err(|reason| MetaError::Invalid { field: field.label(), reason })?;
        }
    }
    if stamp(path)? != expected {
        return Err(MetaError::ModifiedExternally);
    }
    let before = open_tagged(path, false)?;
    let before_props = (before.properties().duration().as_secs_f64(), before.properties().sample_rate());
    let id3_version = id3v2_major_version(path);
    drop(before);

    let tmp = temp_path(path);
    let mut guard = TempGuard(Some(tmp.clone()));
    fs::copy(path, &tmp)?;

    let unsupported = apply_tags(&tmp, edit, id3_version)?;
    if let Some(setlist) = &edit.chapters {
        apply_chapter_edit(&tmp, setlist, before_props.0, id3_version)?;
    }
    verify(&tmp, edit, before_props, &unsupported)?;

    if stamp(path)? != expected {
        return Err(MetaError::ModifiedExternally);
    }
    let (backup, backup_skipped) = match &opts.backup_dir {
        Some(dir) => match make_backup(path, dir, opts.backup_budget_bytes) {
            Ok(p) => (Some(p), None),
            Err(reason) => (None, Some(reason)),
        },
        None => (None, None),
    };
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, path)?;
    guard.0 = None;
    Ok(WriteReport { stamp: stamp(path)?, backup, backup_skipped, unsupported })
}

fn apply_tags(path: &Path, edit: &Edit, id3_version: Option<u8>) -> Result<Vec<Field>, MetaError> {
    if edit.fields.is_empty() && edit.artwork.is_none() {
        return Ok(Vec::new());
    }
    let mut tagged = open_tagged(path, true)?;
    let primary = tagged.primary_tag_type();
    if !tagged.tag_support(primary).is_writable() {
        return Err(MetaError::Unsupported);
    }
    if tagged.tag(primary).is_none() {
        tagged.insert_tag(Tag::new(primary));
    }
    let mut options = LoftyWriteOptions::default();
    let options = options.use_id3v23(id3_version == Some(3));

    let mut unsupported = Vec::new();
    let tag = tagged.tag_mut(primary).expect("inserted above");
    for (field, value) in &edit.fields {
        match value.as_deref().map(str::trim) {
            Some(v) if !v.is_empty() => {
                if !field.set(tag, v) {
                    unsupported.push(*field);
                }
            }
            _ => field.clear(tag),
        }
    }
    if let Some(art) = &edit.artwork {
        while tag.picture_count() > 0 {
            tag.remove_picture(0);
        }
        for a in art {
            let mut builder = Picture::unchecked(a.data.to_vec()).pic_type(a.kind.to_lofty());
            if let Some(m) = &a.mime {
                builder = builder.mime_type(MimeType::from_str(m));
            }
            if let Some(d) = &a.description {
                builder = builder.description(d.clone());
            }
            tag.push_picture(builder.build());
        }
    }
    tag.save_to_path(path, options)?;

    // Keep secondary tags (ID3v1, APE) consistent with the primary one, so other
    // players do not show stale values. Fields they cannot hold are ignored.
    for secondary in [TagType::Id3v1, TagType::Ape] {
        if secondary == primary {
            continue;
        }
        if let Some(tag) = tagged.tag_mut(secondary) {
            for (field, value) in &edit.fields {
                match value.as_deref().map(str::trim) {
                    Some(v) if !v.is_empty() => {
                        field.set(tag, v);
                    }
                    _ => field.clear(tag),
                }
            }
            tag.save_to_path(path, options)?;
        }
    }
    Ok(unsupported)
}

fn apply_chapter_edit(path: &Path, setlist: &Setlist, duration: f64, id3_version: Option<u8>) -> Result<(), MetaError> {
    let is_mp3 = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".nullamp-tmp"))
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".mp3"));
    if !is_mp3 {
        return Err(MetaError::Unsupported);
    }
    let mut tag = match id3::Tag::read_from_path(path) {
        Ok(t) => t,
        Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => id3::Tag::new(),
        Err(e) => return Err(MetaError::Parse(e.to_string())),
    };
    apply_chapters(&mut tag, setlist, (duration > 0.0).then_some(duration));
    let version = match id3_version {
        Some(3) => id3::Version::Id3v23,
        _ => id3::Version::Id3v24,
    };
    tag.write_to_path(path, version).map_err(|e| MetaError::Parse(e.to_string()))
}

fn verify(path: &Path, edit: &Edit, before: (f64, Option<u32>), unsupported: &[Field]) -> Result<(), MetaError> {
    let after = open_tagged(path, true).map_err(|e| MetaError::Verification(e.to_string()))?;
    let props = after.properties();
    let duration = props.duration().as_secs_f64();
    if (duration - before.0).abs() > 0.5 || props.sample_rate() != before.1 {
        return Err(MetaError::Verification(format!(
            "audio properties changed ({:.2}s/{:?} -> {:.2}s/{:?})",
            before.0,
            before.1,
            duration,
            props.sample_rate()
        )));
    }
    let tag = after.tag(after.primary_tag_type());
    for (field, value) in &edit.fields {
        if unsupported.contains(field) {
            continue;
        }
        let wanted = value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let got = tag.and_then(|t| field.get(t)).map(str::trim).filter(|v| !v.is_empty());
        let same = match (wanted, got) {
            (Some(w), Some(g)) => field.reads_back_as(w, g),
            (w, g) => w == g,
        };
        if !same {
            return Err(MetaError::Verification(format!("{} reads back as {got:?}", field.label())));
        }
    }
    if let Some(art) = &edit.artwork {
        let count = tag.map(|t| t.pictures().len()).unwrap_or(0);
        if count != art.len() {
            return Err(MetaError::Verification(format!("expected {} images, found {count}", art.len())));
        }
    }
    if let Some(setlist) = &edit.chapters {
        let wanted = setlist.entries.iter().filter(|e| e.start.is_some()).count();
        let got = read_chapters(path).map_err(|e| MetaError::Verification(e.to_string()))?.len();
        if wanted != got {
            return Err(MetaError::Verification(format!("expected {wanted} chapters, found {got}")));
        }
    }
    Ok(())
}

/// First bytes of a tag-only backup; any other backup is a full copy of the file.
const TAG_BACKUP_MAGIC: &[u8; 8] = b"NLTAGBK1";

fn make_backup(path: &Path, dir: &Path, budget: u64) -> Result<PathBuf, String> {
    let tags = if is_mp3(path) { tag_backup_bytes(path).map_err(|e| e.to_string())? } else { None };
    let size = match &tags {
        Some(bytes) => bytes.len() as u64,
        None => fs::metadata(path).map_err(|e| e.to_string())?.len(),
    };
    if budget > 0 && size > budget / 2 {
        return Err(format!("backup is larger than the backup budget allows ({} MB)", size / 1_000_000));
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let (dst, saved) = match tags {
        Some(bytes) => {
            let dst = dir.join(format!("{secs}-{name}.id3"));
            let saved = fs::write(&dst, bytes);
            (dst, saved)
        }
        None => {
            let dst = dir.join(format!("{secs}-{name}"));
            let saved = fs::copy(path, &dst).map(|_| ());
            (dst, saved)
        }
    };
    saved.map_err(|e| e.to_string())?;
    if budget > 0 {
        prune_backups(dir, budget, &dst);
    }
    Ok(dst)
}

fn is_mp3(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
}

/// Length of the ID3v2 tag at the start of the file (header, padding and footer), 0 when there is none.
fn id3v2_len(f: &mut fs::File, file_len: u64) -> std::io::Result<u64> {
    let mut head = [0u8; 10];
    f.seek(SeekFrom::Start(0))?;
    if file_len < 10 {
        return Ok(0);
    }
    f.read_exact(&mut head)?;
    if &head[..3] != b"ID3" {
        return Ok(0);
    }
    let size = head[6..10].iter().fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7F));
    let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
    Ok((10 + size + footer).min(file_len))
}

/// The byte range holding the audio: after a leading ID3v2 tag and before a trailing ID3v1 tag.
/// `None` when the file also has an APE tag, which these two ends would not cover.
fn audio_range(f: &mut fs::File) -> std::io::Result<Option<(u64, u64)>> {
    let len = f.metadata()?.len();
    let start = id3v2_len(f, len)?;
    let mut end = len;
    if end - start >= 128 {
        let mut tag = [0u8; 3];
        f.seek(SeekFrom::Start(len - 128))?;
        f.read_exact(&mut tag)?;
        if &tag == b"TAG" {
            end -= 128;
        }
    }
    if end - start >= 32 {
        let mut footer = [0u8; 8];
        f.seek(SeekFrom::Start(end - 32))?;
        f.read_exact(&mut footer)?;
        if &footer == b"APETAGEX" {
            return Ok(None);
        }
    }
    Ok(Some((start, end)))
}

/// The ID3v2 and ID3v1 tags of an MP3 as a backup: the magic, the two lengths, then both byte ranges.
/// `None` when the file has an APE tag, so the whole file is copied instead.
fn tag_backup_bytes(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut f = fs::File::open(path)?;
    let Some((start, end)) = audio_range(&mut f)? else { return Ok(None) };
    let len = f.metadata()?.len();
    let mut out = Vec::with_capacity((start + len - end) as usize + 24);
    out.extend_from_slice(TAG_BACKUP_MAGIC);
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&(len - end).to_le_bytes());
    f.seek(SeekFrom::Start(0))?;
    (&mut f).take(start).read_to_end(&mut out)?;
    f.seek(SeekFrom::Start(end))?;
    f.read_to_end(&mut out)?;
    Ok(Some(out))
}

/// Write `target`'s audio between the tags of a tag-only backup to `tmp`.
fn splice_tags(tag_backup: &[u8], target: &Path, tmp: &Path) -> Result<(), MetaError> {
    let lengths = tag_backup.get(8..24).ok_or_else(|| MetaError::Parse("tag backup is truncated".into()))?;
    let head_len = u64::from_le_bytes(lengths[..8].try_into().expect("8 bytes")) as usize;
    let tail_len = u64::from_le_bytes(lengths[8..].try_into().expect("8 bytes")) as usize;
    let tags = &tag_backup[24..];
    if tags.len() != head_len + tail_len {
        return Err(MetaError::Parse("tag backup is truncated".into()));
    }
    let mut src = fs::File::open(target)?;
    let Some((start, end)) = audio_range(&mut src)? else {
        return Err(MetaError::Parse("the file has an APE tag; restore is not possible".into()));
    };
    src.seek(SeekFrom::Start(start))?;
    let mut out = fs::File::create(tmp)?;
    out.write_all(&tags[..head_len])?;
    std::io::copy(&mut (&mut src).take(end - start), &mut out)?;
    out.write_all(&tags[head_len..])?;
    out.sync_all()?;
    Ok(())
}

/// Delete the oldest backups until the directory fits the budget. `keep` is never deleted.
fn prune_backups(dir: &Path, budget: u64, keep: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, p) in files {
        if total <= budget {
            break;
        }
        if p == keep {
            continue;
        }
        if fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

/// Put a backup back in place of `target`, using the same copy-then-rename procedure as
/// writing. A tag-only backup replaces the tags and keeps the audio now in `target`.
pub fn restore_backup(backup: &Path, target: &Path) -> Result<FileStamp, MetaError> {
    let tmp = temp_path(target);
    let mut guard = TempGuard(Some(tmp.clone()));
    let mut magic = [0u8; 8];
    let is_tag_backup = fs::File::open(backup)?.read_exact(&mut magic).is_ok() && &magic == TAG_BACKUP_MAGIC;
    if is_tag_backup {
        splice_tags(&fs::read(backup)?, target, &tmp)?;
    } else {
        fs::copy(backup, &tmp)?;
    }
    fs::rename(&tmp, target)?;
    guard.0 = None;
    Ok(stamp(target)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_copy;
    use crate::{read, read_chapters};
    use id3::TagLike;
    use nullamp_core::setlist::{Provenance, SetlistEntry};

    /// Audio bytes: everything after a leading ID3v2 tag and before a trailing ID3v1 tag.
    fn audio_payload(path: &Path) -> Vec<u8> {
        let bytes = fs::read(path).unwrap();
        let mut start = 0;
        if bytes.starts_with(b"ID3") {
            let size = bytes[6..10].iter().fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7F));
            start = 10 + size;
            // Skip padding up to the first frame sync.
            while start + 1 < bytes.len() && !(bytes[start] == 0xFF && bytes[start + 1] & 0xE0 == 0xE0) {
                start += 1;
            }
        }
        let mut end = bytes.len();
        if end >= 128 && &bytes[end - 128..end - 125] == b"TAG" {
            end -= 128;
        }
        bytes[start..end].to_vec()
    }

    fn set(fields: &[(Field, Option<&str>)]) -> Edit {
        Edit { fields: fields.iter().map(|(f, v)| (*f, v.map(str::to_owned))).collect(), ..Default::default() }
    }

    #[test]
    fn edits_preserve_everything_else() {
        let (_dir, path) = scratch_copy("tagged_v23.mp3");
        // Add frames lofty has no generic mapping for.
        let mut t = id3::Tag::read_from_path(&path).unwrap();
        t.add_frame(id3::frame::Private { owner_identifier: "com.example".into(), private_data: vec![1, 2, 3] });
        t.add_frame(id3::frame::ExtendedText { description: "DJ_KEY".into(), value: "8A".into() });
        t.write_to_path(&path, id3::Version::Id3v23).unwrap();
        let audio_before = audio_payload(&path);

        let stamp0 = stamp(&path).unwrap();
        let report =
            write(&path, stamp0, &set(&[(Field::Title, Some("Renamed — ünïcödé")), (Field::Comment, None)]), &WriteOptions::default())
                .unwrap();
        assert!(report.unsupported.is_empty());

        let m = read(&path).unwrap();
        assert_eq!(m.fields.get(&Field::Title).map(String::as_str), Some("Renamed — ünïcödé"));
        assert_eq!(m.fields.get(&Field::Comment), None, "removed");
        assert_eq!(m.fields.get(&Field::Album).map(String::as_str), Some("Fixtures"), "untouched field kept");
        assert_eq!(m.artwork.len(), 1, "artwork kept");
        assert_eq!(m.technical.tag_types, ["ID3v2.3"], "ID3 version kept");
        let t = id3::Tag::read_from_path(&path).unwrap();
        assert!(t.frames().any(|f| f.id() == "PRIV"), "unknown frame kept");
        assert!(t.extended_texts().any(|x| x.description == "DJ_KEY" && x.value == "8A"));
        assert!(t.extended_texts().any(|x| x.value == "keep me"));
        assert_eq!(audio_payload(&path), audio_before, "audio data unchanged");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn chapters_survive_tag_edits_and_can_be_replaced() {
        let (_dir, path) = scratch_copy("mix_chapters.mp3");
        write(&path, stamp(&path).unwrap(), &set(&[(Field::Artist, Some("New DJ"))]), &WriteOptions::default()).unwrap();
        assert_eq!(read_chapters(&path).unwrap().len(), 3);

        let mut setlist = Setlist::default();
        for (i, (a, t)) in [("X", "First"), ("Y", "Second")].iter().enumerate() {
            setlist.entries.push(SetlistEntry::new(*a, *t, Some(i as f64 * 3.0), Provenance::Manual));
        }
        setlist.entries.push(SetlistEntry::new("No", "Time", None, Provenance::Manual));
        let edit = Edit { chapters: Some(setlist), ..Default::default() };
        write(&path, stamp(&path).unwrap(), &edit, &WriteOptions::default()).unwrap();
        let ch = read_chapters(&path).unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[1].title.as_deref(), Some("Second"));
        assert_eq!(ch[1].artist.as_deref(), Some("Y"));
        assert_eq!(ch[1].start, 3.0);
        assert!((ch[1].end.unwrap() - 6.0).abs() < 0.1, "last chapter ends at file end");
        assert_eq!(read(&path).unwrap().fields.get(&Field::Artist).map(String::as_str), Some("New DJ"));
    }

    #[test]
    fn refuses_when_modified_externally() {
        let (_dir, path) = scratch_copy("tagged_v24.mp3");
        let old = stamp(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0u8; 16]);
        fs::write(&path, &bytes).unwrap();
        let err = write(&path, old, &set(&[(Field::Title, Some("x"))]), &WriteOptions::default()).unwrap_err();
        assert!(matches!(err, MetaError::ModifiedExternally));
        assert_eq!(fs::read(&path).unwrap(), bytes, "file untouched");
    }

    #[test]
    fn invalid_values_are_rejected_before_touching_the_file() {
        let (_dir, path) = scratch_copy("tagged_v24.mp3");
        let before = fs::read(&path).unwrap();
        let err = write(&path, stamp(&path).unwrap(), &set(&[(Field::TrackNumber, Some("abc"))]), &WriteOptions::default());
        assert!(matches!(err, Err(MetaError::Invalid { .. })));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn artwork_replace_and_remove() {
        let (_dir, path) = scratch_copy("tagged_v24.mp3");
        let jpeg: Arc<[u8]> = Arc::from(fs::read(crate::test_support::fixture("cover.jpg")).unwrap());
        let art = NewArtwork { kind: ArtworkKind::FrontCover, mime: Some("image/jpeg".into()), description: None, data: jpeg };
        let edit = Edit { artwork: Some(vec![art]), ..Default::default() };
        write(&path, stamp(&path).unwrap(), &edit, &WriteOptions::default()).unwrap();
        let m = read(&path).unwrap();
        assert_eq!(m.artwork.len(), 1);
        assert_eq!(m.artwork[0].width, Some(64));

        let edit = Edit { artwork: Some(vec![]), ..Default::default() };
        write(&path, stamp(&path).unwrap(), &edit, &WriteOptions::default()).unwrap();
        assert!(read(&path).unwrap().artwork.is_empty());
    }

    #[test]
    fn writes_other_formats() {
        for name in ["tone.flac", "tone.ogg", "tone.m4a"] {
            let (_dir, path) = scratch_copy(name);
            let edit = set(&[(Field::Title, Some("Edited")), (Field::TrackNumber, Some("7")), (Field::Date, Some("2021-05"))]);
            let r = write(&path, stamp(&path).unwrap(), &edit, &WriteOptions::default());
            let r = r.unwrap_or_else(|e| panic!("{name}: {e}"));
            let m = read(&path).unwrap();
            assert_eq!(m.fields.get(&Field::Title).map(String::as_str), Some("Edited"), "{name}");
            if !r.unsupported.contains(&Field::TrackNumber) {
                assert_eq!(m.fields.get(&Field::TrackNumber).map(String::as_str), Some("7"), "{name}");
            }
        }
    }

    #[test]
    fn backup_restore_and_budget() {
        let (dir, path) = scratch_copy("tagged_v24.mp3");
        let original = fs::read(&path).unwrap();
        let backups = dir.path().join("backups");
        let opts = WriteOptions { backup_dir: Some(backups.clone()), backup_budget_bytes: 10 * original.len() as u64 };
        let r = write(&path, stamp(&path).unwrap(), &set(&[(Field::Title, Some("one"))]), &opts).unwrap();
        let backup = r.backup.unwrap();
        // Only the tags are kept, not the audio.
        let tags = fs::read(&backup).unwrap();
        assert!(tags.starts_with(TAG_BACKUP_MAGIC));
        assert!(tags.len() < original.len(), "{} of {} bytes", tags.len(), original.len());
        assert_ne!(fs::read(&path).unwrap(), original);
        restore_backup(&backup, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);

        // Many writes: the directory stays within budget and the newest backup survives.
        let mut last = None;
        for i in 0..12 {
            let r = write(&path, stamp(&path).unwrap(), &set(&[(Field::Title, Some(&format!("t{i}")))]), &opts).unwrap();
            last = r.backup;
        }
        let total: u64 = fs::read_dir(&backups).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert!(total <= opts.backup_budget_bytes);
        assert!(last.unwrap().exists());

        let small = WriteOptions { backup_dir: Some(backups), backup_budget_bytes: 10 };
        let r = write(&path, stamp(&path).unwrap(), &set(&[(Field::Title, Some("big"))]), &small).unwrap();
        assert!(r.backup.is_none() && r.backup_skipped.is_some());
    }

    #[test]
    fn tag_backup_covers_id3v1_and_other_formats_are_copied() {
        let (dir, path) = scratch_copy("tagged_v23.mp3");
        let backups = dir.path().join("backups");
        let opts = WriteOptions { backup_dir: Some(backups.clone()), backup_budget_bytes: 0 };
        // Append an ID3v1 tag so both ends of the file hold tags.
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..9].copy_from_slice(b"before");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&v1);
        fs::write(&path, &bytes).unwrap();
        let audio = audio_payload(&path);

        let r = write(&path, stamp(&path).unwrap(), &set(&[(Field::Title, Some("after"))]), &opts).unwrap();
        let backup = r.backup.unwrap();
        assert!(fs::read(&backup).unwrap().starts_with(TAG_BACKUP_MAGIC));
        restore_backup(&backup, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(audio_payload(&path), audio);

        let (dir, flac) = scratch_copy("tone.flac");
        let flac_original = fs::read(&flac).unwrap();
        let opts = WriteOptions { backup_dir: Some(dir.path().join("backups")), backup_budget_bytes: 0 };
        let r = write(&flac, stamp(&flac).unwrap(), &set(&[(Field::Title, Some("after"))]), &opts).unwrap();
        let backup = r.backup.unwrap();
        assert_eq!(fs::read(&backup).unwrap(), flac_original);
        restore_backup(&backup, &flac).unwrap();
        assert_eq!(fs::read(&flac).unwrap(), flac_original);
    }
}
