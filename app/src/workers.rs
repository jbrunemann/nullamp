//! Background work: everything that touches the filesystem or network.
//!
//! Three lanes keep slow work away from fast work:
//! - *interactive* (now-playing info, inspector reads, tag writes) is always served
//!   before *bulk* (playlist scans, folder imports) by the same local workers;
//! - *online* requests run on their own thread, so a slow network never delays
//!   local work.
//!
//! Every job carries a request id. The UI remembers the id it is waiting for and
//! ignores results for anything it has moved on from. Bulk scans also check a
//! generation counter so they stop early once superseded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, select_biased};
use nullamp_core::setlist::{MixInfo, Provenance};
use nullamp_core::{Setlist, TrackInfo, cue, m3u, tracklist_text};
use nullamp_meta::{Edit, FileMetadata, FileStamp, WriteOptions};
use nullamp_online::{ArtworkSource, CoverArtArchive, CoverImage, Http, MusicBrainz, OnlineError, RecordingMatch, RecordingSource};

use crate::paths::AppPaths;
use crate::store::{self, SavedSetlist};

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac", "aif", "aiff", "caf", "mka", "alac"];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn is_playlist_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "m3u" | "m3u8"))
}

/// Decoded RGBA image, ready to upload as a texture.
#[derive(Clone)]
pub struct Thumb {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SetlistOrigin {
    /// Previously saved (includes the user's corrections).
    Saved,
    /// Found in the file or next to it just now.
    Discovered,
}

pub struct NowPlayingInfo {
    pub token: u64,
    pub path: PathBuf,
    pub meta: Result<FileMetadata, String>,
    pub art: Option<Thumb>,
    /// Artwork written to the cache, for the OS media overlay.
    pub art_file: Option<PathBuf>,
    pub fingerprint: Option<String>,
    pub setlist: Option<(Setlist, SetlistOrigin)>,
    /// Tracklist pages referenced in the file's tags.
    pub tracklist_urls: Vec<String>,
}

pub enum Job {
    ScanInfo { generation: u64, paths: Vec<PathBuf> },
    /// Cheap existence check (one `stat` per file) for entries whose info is cached.
    CheckExists { generation: u64, paths: Vec<PathBuf> },
    ExpandPaths { request: u64, paths: Vec<PathBuf> },
    ImportPlaylist { request: u64, path: PathBuf },
    ExportPlaylist { path: PathBuf, items: Vec<(PathBuf, TrackInfo)> },
    NowPlaying { token: u64, path: PathBuf },
    Inspect { request: u64, path: PathBuf },
    DecodeImage { request: u64, bytes: Arc<[u8]> },
    WriteTags { request: u64, files: Vec<(PathBuf, FileStamp)>, edit: Edit, options: WriteOptions },
    SaveSetlist(SavedSetlist),
    DeleteSetlist(String),
    ReadText { request: u64, path: PathBuf },
    RestoreBackup { request: u64, backup: PathBuf, target: PathBuf },
    /// Look for chapters / cue sheets again (user-initiated rescan).
    Rediscover { request: u64, path: PathBuf },
    WriteText { path: PathBuf, text: String },
}

pub enum OnlineJob {
    RecordingSearch { request: u64, artist: String, title: String },
    ReleaseArtwork { request: u64, release_id: String },
    FetchImage { request: u64, url: String, hosts: &'static [&'static str] },
}

pub enum JobResult {
    Info { path: PathBuf, info: Result<TrackInfo, String> },
    Missing { paths: Vec<PathBuf> },
    Expanded { request: u64, paths: Vec<PathBuf>, skipped: usize },
    Imported { request: u64, result: Result<(Option<String>, Vec<(PathBuf, TrackInfo)>, usize), String> },
    Exported { path: PathBuf, result: Result<(), String> },
    NowPlaying(Box<NowPlayingInfo>),
    Inspected { request: u64, path: PathBuf, meta: Result<FileMetadata, String> },
    Image { request: u64, thumb: Option<Thumb> },
    Written { request: u64, results: Vec<(PathBuf, Result<nullamp_meta::WriteReport, String>)> },
    SetlistSaved(Result<(), String>),
    Text { request: u64, path: PathBuf, text: Result<String, String> },
    Rediscovered { request: u64, setlist: Option<Setlist> },
    Restored { request: u64, target: PathBuf, result: Result<(), String> },
    TextWritten { path: PathBuf, result: Result<(), String> },
    Recordings { request: u64, result: Result<Vec<RecordingMatch>, OnlineError> },
    ReleaseImages { request: u64, result: Result<Vec<CoverImage>, OnlineError> },
    OnlineImage { request: u64, url: String, result: Result<(Arc<[u8]>, Thumb), String> },
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct Workers {
    interactive: Sender<Job>,
    bulk: Sender<Job>,
    online: Sender<OnlineJob>,
    pub results: Receiver<JobResult>,
    scan_generation: Arc<AtomicU64>,
    next_request: AtomicU64,
}

impl Workers {
    pub fn start(paths: AppPaths, waker: Waker) -> Self {
        let (interactive, interactive_rx) = crossbeam_channel::unbounded::<Job>();
        let (bulk, bulk_rx) = crossbeam_channel::unbounded::<Job>();
        let (online, online_rx) = crossbeam_channel::unbounded::<OnlineJob>();
        let (results_tx, results) = crossbeam_channel::unbounded();
        let scan_generation = Arc::new(AtomicU64::new(1));

        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        for i in 0..threads {
            let (irx, brx, tx, w, gen_, p) =
                (interactive_rx.clone(), bulk_rx.clone(), results_tx.clone(), Arc::clone(&waker), Arc::clone(&scan_generation), paths.clone());
            std::thread::Builder::new()
                .name(format!("nullamp-worker-{i}"))
                .spawn(move || {
                    let ctx = Ctx { paths: p, scan_generation: gen_ };
                    loop {
                        let job = select_biased! {
                            recv(irx) -> j => j,
                            recv(brx) -> j => j,
                        };
                        let Ok(job) = job else { break };
                        for r in run_job(&ctx, job) {
                            if tx.send(r).is_err() {
                                return;
                            }
                        }
                        w();
                    }
                })
                .expect("spawn worker");
        }

        let tx = results_tx;
        let w = waker;
        let cache = paths.online_cache();
        std::thread::Builder::new()
            .name("nullamp-online".into())
            .spawn(move || {
                let http = Http::new(Some(cache));
                for job in online_rx {
                    if tx.send(run_online(&http, job)).is_err() {
                        return;
                    }
                    w();
                }
            })
            .expect("spawn online worker");

        Workers { interactive, bulk, online, results, scan_generation, next_request: AtomicU64::new(1) }
    }

    pub fn request_id(&self) -> u64 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    pub fn submit(&self, job: Job) {
        let lane = match job {
            Job::ScanInfo { .. } | Job::CheckExists { .. } | Job::ExpandPaths { .. } => &self.bulk,
            _ => &self.interactive,
        };
        let _ = lane.send(job);
    }

    pub fn submit_online(&self, job: OnlineJob) {
        let _ = self.online.send(job);
    }

    /// Queue tag reads for playlist rows, in chunks so other work interleaves.
    pub fn scan(&self, paths: Vec<PathBuf>) {
        let generation = self.scan_generation.load(Ordering::Relaxed);
        for chunk in paths.chunks(32) {
            self.submit(Job::ScanInfo { generation, paths: chunk.to_vec() });
        }
    }

    pub fn check_exists(&self, paths: Vec<PathBuf>) {
        let generation = self.scan_generation.load(Ordering::Relaxed);
        for chunk in paths.chunks(256) {
            self.submit(Job::CheckExists { generation, paths: chunk.to_vec() });
        }
    }

    /// Abandon queued scans (e.g. after clearing a large playlist).
    pub fn cancel_scans(&self) {
        self.scan_generation.fetch_add(1, Ordering::Relaxed);
    }
}

struct Ctx {
    paths: AppPaths,
    scan_generation: Arc<AtomicU64>,
}

fn run_job(ctx: &Ctx, job: Job) -> Vec<JobResult> {
    match job {
        Job::ScanInfo { generation, paths } => {
            let mut out = Vec::with_capacity(paths.len());
            for path in paths {
                if ctx.scan_generation.load(Ordering::Relaxed) != generation {
                    break;
                }
                let info = nullamp_meta::read_track_info(&path).map_err(|e| e.to_string());
                out.push(JobResult::Info { path, info });
            }
            out
        }
        Job::CheckExists { generation, paths } => {
            if ctx.scan_generation.load(Ordering::Relaxed) != generation {
                return Vec::new();
            }
            let missing: Vec<PathBuf> = paths.into_iter().filter(|p| !p.exists()).collect();
            if missing.is_empty() { Vec::new() } else { vec![JobResult::Missing { paths: missing }] }
        }
        Job::ExpandPaths { request, paths } => {
            let (paths, skipped) = expand_paths(&paths);
            vec![JobResult::Expanded { request, paths, skipped }]
        }
        Job::ImportPlaylist { request, path } => {
            let result = std::fs::read(&path).map_err(|e| e.to_string()).map(|bytes| {
                let base = path.parent().unwrap_or(Path::new("/"));
                let parsed = m3u::parse(&bytes, base);
                let items = parsed.items.into_iter().map(|i| (i.path, i.info)).collect();
                (parsed.title, items, parsed.skipped.len())
            });
            vec![JobResult::Imported { request, result }]
        }
        Job::ExportPlaylist { path, items } => {
            let base = path.parent().map(Path::to_path_buf);
            let text = m3u::write(items.iter().map(|(p, i)| (p.as_path(), i)), &m3u::WriteOptions { relative_to: base });
            let result = store_text(&path, &text);
            vec![JobResult::Exported { path, result }]
        }
        Job::NowPlaying { token, path } => vec![JobResult::NowPlaying(Box::new(now_playing(ctx, token, path)))],
        Job::Inspect { request, path } => {
            let meta = nullamp_meta::read(&path).map_err(|e| e.to_string());
            vec![JobResult::Inspected { request, path, meta }]
        }
        Job::DecodeImage { request, bytes } => vec![JobResult::Image { request, thumb: decode_thumb(&bytes, 512) }],
        Job::WriteTags { request, files, edit, options } => {
            let results = files
                .into_iter()
                .map(|(path, stamp)| {
                    let r = nullamp_meta::write(&path, stamp, &edit, &options).map_err(|e| e.to_string());
                    (path, r)
                })
                .collect();
            vec![JobResult::Written { request, results }]
        }
        Job::SaveSetlist(saved) => {
            let result = store::save_setlist(&ctx.paths, &saved.fingerprint, Some(&saved));
            vec![JobResult::SetlistSaved(result.map_err(|e| e.to_string()))]
        }
        Job::DeleteSetlist(fp) => {
            let result = store::save_setlist(&ctx.paths, &fp, None);
            vec![JobResult::SetlistSaved(result.map_err(|e| e.to_string()))]
        }
        Job::ReadText { request, path } => {
            let text = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| if b.len() > 2 * 1024 * 1024 { Err("file too large".into()) } else { Ok(b) })
                .map(|b| nullamp_core::text::decode_lossy(&b));
            vec![JobResult::Text { request, path, text }]
        }
        Job::RestoreBackup { request, backup, target } => {
            let result = nullamp_meta::restore_backup(&backup, &target).map(|_| ()).map_err(|e| e.to_string());
            vec![JobResult::Restored { request, target, result }]
        }
        Job::Rediscover { request, path } => {
            let meta = nullamp_meta::read(&path).ok();
            vec![JobResult::Rediscovered { request, setlist: discover_local_setlist(&path, meta.as_ref()) }]
        }
        Job::WriteText { path, text } => {
            let result = store_text(&path, &text);
            vec![JobResult::TextWritten { path, result }]
        }
    }
}

fn store_text(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("nullamp-tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Expand folders recursively into audio files (sorted per folder), keep files,
/// and count what was skipped.
pub fn expand_paths(inputs: &[PathBuf]) -> (Vec<PathBuf>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for p in inputs {
        if p.is_dir() {
            walk(p, &mut out, &mut skipped, 0);
        } else if is_audio_file(p) {
            out.push(p.clone());
        } else {
            skipped += 1;
        }
    }
    (out, skipped)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, skipped: &mut usize, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        *skipped += 1;
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    for p in entries {
        let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        // Do not follow symlinked directories (avoids cycles).
        let is_dir = std::fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            walk(&p, out, skipped, depth + 1);
        } else if is_audio_file(&p) {
            out.push(p);
        }
    }
}

pub fn decode_thumb(bytes: &[u8], max_side: u32) -> Option<Thumb> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max_side || img.height() > max_side { img.thumbnail(max_side, max_side) } else { img };
    let rgba = img.to_rgba8();
    Some(Thumb { size: [rgba.width() as usize, rgba.height() as usize], rgba: rgba.into_raw() })
}

fn now_playing(ctx: &Ctx, token: u64, path: PathBuf) -> NowPlayingInfo {
    let meta = nullamp_meta::read(&path).map_err(|e| e.to_string());
    let mut art = None;
    let mut art_file = None;
    let fingerprint = store::fingerprint(&path).ok();
    if let Ok(m) = &meta
        && let Some(cover) = m.front_cover()
    {
        art = decode_thumb(&cover.data, 512);
        if art.is_some() {
            let dir = ctx.paths.artwork_cache();
            let ext = if cover.mime.as_deref() == Some("image/png") { "png" } else { "jpg" };
            let name = format!("now-{}.{ext}", fingerprint.as_deref().unwrap_or("current"));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join(&name), &cover.data).is_ok() {
                art_file = Some(dir.join(name));
            }
        }
    }

    let saved = fingerprint.as_deref().and_then(|fp| store::find_setlist(&ctx.paths, fp, &path));
    let setlist = match saved {
        Some(s) => Some((s.setlist, SetlistOrigin::Saved)),
        None => discover_local_setlist(&path, meta.as_ref().ok()).map(|s| (s, SetlistOrigin::Discovered)),
    };
    let tracklist_urls = meta.as_ref().map(tracklist_urls).unwrap_or_default();
    NowPlayingInfo { token, path, meta, art, art_file, fingerprint, setlist, tracklist_urls }
}

/// Local setlist sources, in order: embedded chapters, a cue sheet stored in a tag,
/// a cue sheet next to the file.
pub fn discover_local_setlist(path: &Path, meta: Option<&FileMetadata>) -> Option<Setlist> {
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    let mix = meta
        .map(|m| MixInfo {
            title: m.fields.get(&nullamp_meta::Field::Title).cloned(),
            dj: m.fields.get(&nullamp_meta::Field::Artist).cloned(),
            date: m.fields.get(&nullamp_meta::Field::Date).cloned(),
            ..Default::default()
        })
        .unwrap_or_default();

    if let Some(m) = meta {
        if m.chapters.len() >= 2 {
            return Some(nullamp_meta::chapters_to_setlist(&m.chapters, mix));
        }
        if let Some(text) = &m.embedded_cue {
            let sheet = cue::parse(text.as_bytes());
            if let Some(mut s) = sheet.to_setlist(&file_name, "embedded")
                && s.entries.len() >= 2
            {
                for e in &mut s.entries {
                    e.provenance = Provenance::CueSheet { path: "embedded in tags".into() };
                }
                fill_mix(&mut s.mix, &mix);
                return Some(s);
            }
        }
    }
    for cue_path in adjacent_cue_sheets(path) {
        let Ok(bytes) = std::fs::read(&cue_path) else { continue };
        let sheet = cue::parse(&bytes);
        if let Some(mut s) = sheet.to_setlist(&file_name, &cue_path.to_string_lossy())
            && !s.entries.is_empty()
        {
            fill_mix(&mut s.mix, &mix);
            return Some(s);
        }
    }
    // A plain track list in the description, often without times: reference only.
    let described = meta.and_then(|m| {
        [nullamp_meta::Field::Comment, nullamp_meta::Field::Description].iter().find_map(|f| tracklist_text::parse_description(m.fields.get(f)?))
    });
    described.map(|mut s| {
        fill_mix(&mut s.mix, &mix);
        s
    })
}

fn fill_mix(target: &mut MixInfo, from: &MixInfo) {
    if target.title.is_none() {
        target.title = from.title.clone();
    }
    if target.dj.is_none() {
        target.dj = from.dj.clone();
    }
    if target.date.is_none() {
        target.date = from.date.clone();
    }
}

/// `mix.cue` for `mix.mp3`, then `mix.mp3.cue`, then any other cue sheet in the
/// folder that names this file.
fn adjacent_cue_sheets(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let with_cue = path.with_extension("cue");
    if with_cue.is_file() {
        out.push(with_cue);
    }
    let appended = PathBuf::from(format!("{}.cue", path.display()));
    if appended.is_file() {
        out.push(appended);
    }
    let Some(dir) = path.parent() else { return out };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return out };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()).take(500) {
            let p = e.path();
            let is_cue = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("cue"));
            if !is_cue || out.contains(&p) {
                continue;
            }
            let small = e.metadata().map(|m| m.len() < cue::MAX_CUE_BYTES as u64).unwrap_or(false);
            if small && std::fs::read(&p).map(|b| nullamp_core::text::decode_lossy(&b).to_lowercase().contains(&name)).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

/// URLs of known tracklist sites found in the file's tags.
fn tracklist_urls(m: &FileMetadata) -> Vec<String> {
    let mut texts: Vec<&str> = m.fields.values().map(String::as_str).collect();
    texts.extend(m.other_items.iter().map(|(_, v)| v.as_str()));
    let mut urls = Vec::new();
    for t in texts {
        for word in t.split_whitespace() {
            let w = word.trim_matches(|c: char| "<>()[]\"',".contains(c));
            let known = ["1001tracklists.com", "mixcloud.com", "mixesdb.com", "soundcloud.com"];
            if (w.starts_with("https://") || w.starts_with("http://")) && known.iter().any(|k| w.contains(k)) && !urls.iter().any(|u| u == w) {
                urls.push(w.to_owned());
            }
        }
    }
    urls
}

fn run_online(http: &Http, job: OnlineJob) -> JobResult {
    match job {
        OnlineJob::RecordingSearch { request, artist, title } => {
            JobResult::Recordings { request, result: MusicBrainz.search(http, &artist, &title) }
        }
        OnlineJob::ReleaseArtwork { request, release_id } => {
            JobResult::ReleaseImages { request, result: CoverArtArchive.images_for_release(http, &release_id) }
        }
        OnlineJob::FetchImage { request, url, hosts } => {
            let result = http
                .get_image(&url, hosts)
                .map_err(|e| e.to_string())
                .and_then(|b| decode_thumb(&b, 512).map(|t| (Arc::from(b), t)).ok_or_else(|| "cannot decode image".to_owned()));
            JobResult::OnlineImage { request, url, result }
        }
    }
}

/// Hosts allowed for images (Cover Art Archive is the only image source in use).
pub fn image_hosts_for(_url: &str) -> &'static [&'static str] {
    CoverArtArchive.allowed_hosts()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures").join(name)
    }

    #[test]
    fn expands_folders_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("B sub");
        std::fs::create_dir_all(&sub).unwrap();
        for p in [dir.path().join("b.mp3"), dir.path().join("A.flac"), sub.join("c.ogg"), dir.path().join("notes.txt"), dir.path().join(".hidden.mp3")] {
            std::fs::write(p, b"x").unwrap();
        }
        let (paths, skipped) = expand_paths(&[dir.path().to_path_buf(), dir.path().join("notes.txt")]);
        let names: Vec<_> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["A.flac", "c.ogg", "b.mp3"], "case-insensitive name order; \"B sub\" sorts before \"b.mp3\"");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn discovers_chapters_then_cue() {
        let path = fixture("mix_chapters.mp3");
        let meta = nullamp_meta::read(&path).unwrap();
        let s = discover_local_setlist(&path, Some(&meta)).unwrap();
        assert_eq!(s.entries.len(), 3, "chapters win over the adjacent cue sheet");
        assert_eq!(s.entries[0].provenance, Provenance::EmbeddedChapters);

        // Without chapters, the adjacent cue sheet is used.
        let mut no_chapters = meta.clone();
        no_chapters.chapters.clear();
        let s = discover_local_setlist(&path, Some(&no_chapters)).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[1].title, "Cue Middle");
        assert!(matches!(&s.entries[0].provenance, Provenance::CueSheet { path } if path.ends_with("mix.cue")));

        // A file with no sources yields nothing.
        let plain = fixture("tagged_v24.mp3");
        assert!(discover_local_setlist(&plain, nullamp_meta::read(&plain).ok().as_ref()).is_none());
    }

    #[test]
    fn discovers_untimed_list_in_description() {
        let path = fixture("tagged_v24.mp3");
        let mut meta = nullamp_meta::read(&path).unwrap();
        meta.fields.insert(nullamp_meta::Field::Comment, "1. A - One\n2. B - Two\n3. C - Three\n".into());
        let s = discover_local_setlist(&path, Some(&meta)).unwrap();
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[1].title, "Two");
        assert!(s.entries.iter().all(|e| e.start.is_none()));
        assert!(s.validate(None).is_empty(), "missing times are expected for a reference list");
    }

    #[test]
    fn embedded_cue_in_comment() {
        let path = fixture("tagged_v24.mp3");
        let mut meta = nullamp_meta::read(&path).unwrap();
        meta.embedded_cue = Some("FILE \"x.wav\" WAVE\n TRACK 01 AUDIO\n TITLE \"A\"\n PERFORMER \"P\"\n INDEX 01 00:00:00\n TRACK 02 AUDIO\n TITLE \"B\"\n INDEX 01 01:00:00\n".into());
        let s = discover_local_setlist(&path, Some(&meta)).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[1].start, Some(60.0));
    }

    #[test]
    fn finds_tracklist_urls() {
        let path = fixture("tagged_v24.mp3");
        let mut meta = nullamp_meta::read(&path).unwrap();
        meta.fields.insert(nullamp_meta::Field::Comment, "Tracklist: https://www.1001tracklists.com/tracklist/abc/x.html, enjoy".into());
        meta.other_items.push(("WXXX:".into(), "https://www.mixcloud.com/u/m/".into()));
        let urls = tracklist_urls(&meta);
        assert_eq!(urls, ["https://www.1001tracklists.com/tracklist/abc/x.html", "https://www.mixcloud.com/u/m/"]);
    }

    #[test]
    fn decodes_thumbnails() {
        let bytes = std::fs::read(fixture("cover.jpg")).unwrap();
        let t = decode_thumb(&bytes, 32).unwrap();
        assert_eq!(t.size, [32, 32]);
        assert_eq!(t.rgba.len(), 32 * 32 * 4);
        assert!(decode_thumb(b"not an image", 32).is_none());
    }
}
