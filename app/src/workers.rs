//! Background work: everything that touches the filesystem.
//!
//! Three lanes keep slow work away from fast work:
//! - *interactive* (now-playing info, inspector reads, tag writes) is always served
//!   before *bulk* (playlist scans, folder imports) by the same local workers;
//! - *waveform* overviews run on their own thread too: decoding a long mix takes
//!   seconds and must not hold up the local workers;
//! - *tempo* analysis has a thread of its own as well, so it neither waits for a waveform nor
//!   cancels one. Both start after the track has been handed to the audio engine and never
//!   delay playback;
//! - *online* artwork lookups (the tracks of a mix, and the inspector's "Find online") wait on
//!   the network for seconds, so they have a thread of their own too and answer in the order
//!   they were asked;
//! - *identify* (naming the tracks of a mix with Shazam) runs for minutes, mostly waiting
//!   between requests, so it has a thread of its own and must not hold up the artwork lookups.
//!
//! Every job carries a request id. The UI remembers the id it is waiting for and
//! ignores results for anything it has moved on from. Bulk scans also check a
//! generation counter so they stop early once superseded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, select_biased};
use nullamp_audio::{Tempo, TempoError, Waveform, WaveformError};
use nullamp_core::setlist::{MixInfo, Provenance};
use nullamp_core::{EntryId, Setlist, TrackInfo, cue, m3u, tracklist_text};
use nullamp_meta::{Edit, FileMetadata, FileStamp, WriteOptions};

use crate::paths::AppPaths;
use crate::store::{self, SavedSetlist};
use crate::waveform_cache;

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
    /// `art` as dithered pixel art; made here so switching the setting needs no new work.
    pub retro_art: Option<Thumb>,
    /// Artwork written to the cache, for the OS media overlay.
    pub art_file: Option<PathBuf>,
    pub fingerprint: Option<String>,
    pub setlist: Option<(Setlist, SetlistOrigin)>,
    /// What the file's own tags and neighbours yield now, even when a saved setlist
    /// takes precedence. Lets a tag edit refresh the entries that came from the tags.
    pub discovered: Option<Setlist>,
    /// Tracklist pages referenced in the file's tags.
    pub tracklist_urls: Vec<String>,
    /// The file's tags as playlist info, with what rekordbox's library adds for a file on its USB stick.
    pub track_info: Option<TrackInfo>,
}

pub enum Job {
    ScanInfo { generation: u64, paths: Vec<PathBuf> },
    /// Cheap existence check (one `stat` per file) for entries whose info is cached.
    CheckExists { generation: u64, paths: Vec<PathBuf> },
    /// Embedded cover thumbnails for visible playlist rows. Superseded by any later
    /// request, so fast scrolling does not queue up work for rows already gone.
    Covers { generation: u64, paths: Vec<PathBuf> },
    ExpandPaths { request: u64, paths: Vec<PathBuf> },
    ImportPlaylist { request: u64, path: PathBuf },
    /// Read the library of every rekordbox USB stick that is mounted.
    OpenSticks,
    /// Read the playlist tree of the rekordbox desktop library (`master.db`) with the configured key,
    /// for the user to choose from.
    ListCollection,
    /// Make lists of the chosen part of the desktop library: all of it, and the playlists with these ids.
    OpenCollection { all: bool, ids: HashSet<String>, read_data: bool },
    /// Read the crates, playlists and history of the chosen Mixxx library, for the user to choose from.
    ListMixxx,
    /// Make lists of the chosen part of the Mixxx library: all of it, and the lists with these ids.
    OpenMixxx { all: bool, ids: HashSet<String>, read_data: bool },
    /// Store what rekordbox and Mixxx know about these files in the cache, ahead of their being played.
    /// Dropped when a newer request has replaced `generation`.
    ReadDjData { generation: u64, request: u64, paths: Vec<PathBuf> },
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
    /// Chromaprint of the file's first two minutes, written to the log only (nothing consumes it yet).
    LogFingerprint { path: PathBuf },
}

struct WaveformRequest {
    generation: u64,
    token: u64,
    path: PathBuf,
    /// Prefer the waveform rekordbox analysed when the file is on one of its USB sticks.
    rekordbox: bool,
}

struct TempoRequest {
    generation: u64,
    token: u64,
    path: PathBuf,
    /// One track of a mix: its start and end in seconds. `None` analyses the file as a whole.
    span: Option<(f64, f64)>,
}

struct EntryArtRequest {
    key: String,
    artist: String,
    title: String,
}

struct IdentifyRequest {
    request: u64,
    path: PathBuf,
    tracks: Vec<IdentifyTrack>,
}

/// One track of a mix to name: its setlist entry and where it plays in the file.
pub struct IdentifyTrack {
    pub entry: EntryId,
    pub start: f64,
    pub end: f64,
}

enum OnlineRequest {
    EntryArt(EntryArtRequest),
    TrackCover { request: u64, artist: String, title: String },
}

/// A cover found online for the inspector, not yet part of any file's tags.
pub struct OnlineCover {
    pub data: Arc<[u8]>,
    pub mime: &'static str,
    /// Where it came from, e.g. "Release by Artist on Deezer".
    pub source: String,
}

/// The cover of one track of a mix: found online, or a generated placeholder.
pub struct EntryArt {
    pub art: Thumb,
    pub retro_art: Option<Thumb>,
    /// The cached image, for the OS media overlay. `None` for a placeholder.
    pub file: Option<PathBuf>,
}

/// A playlist file read for import: its title, each entry's path and what the file says about it,
/// and how many lines were skipped.
pub type ImportedList = (Option<String>, Vec<(PathBuf, TrackInfo)>, usize);

pub enum JobResult {
    /// The answer to [`Workers::request_track_cover`]: `Ok(None)` when the services had nothing close enough.
    TrackCover { request: u64, result: Result<Option<OnlineCover>, String> },
    /// The cover of the track with this `entry_art::key`: a generated placeholder when none was
    /// found, an error when the services could not be asked (the next request tries again).
    EntryArt { key: String, result: Result<EntryArt, String> },
    Info { path: PathBuf, info: Result<TrackInfo, String> },
    Missing { paths: Vec<PathBuf> },
    /// `art` is the image's content hash and a small thumbnail, `None` without cover.
    Cover { path: PathBuf, art: Option<(u64, Thumb)> },
    Expanded { request: u64, paths: Vec<PathBuf>, skipped: usize },
    /// The answer to [`Job::OpenSticks`], one entry per stick found.
    Sticks(Vec<crate::rekordbox::Loaded>),
    /// The answer to [`Job::ListCollection`]: the desktop library's playlist tree, or why it could not be read.
    CollectionList(Result<crate::rekordbox::Overview, String>),
    /// The answer to [`Job::ListMixxx`].
    MixxxList(Result<crate::rekordbox::Overview, String>),
    /// The answer to [`Job::OpenMixxx`]: the chosen lists, or why the library could not be read.
    Mixxx { read_data: bool, result: Result<Vec<crate::rekordbox::Loaded>, String> },
    /// The answer to [`Job::OpenCollection`]: the chosen lists, or why the library could not be read.
    Collection { read_data: bool, result: Result<Vec<crate::rekordbox::Loaded>, String> },
    /// One chunk of a [`Job::ReadDjData`] request is done: `done` files looked at, `stored` of them
    /// known to rekordbox or Mixxx and cached, `waveforms` of those with an analysed waveform.
    DjDataRead { request: u64, done: usize, stored: usize, waveforms: usize },
    Imported { request: u64, result: Result<ImportedList, String> },
    Exported { path: PathBuf, result: Result<(), String> },
    NowPlaying(Box<NowPlayingInfo>),
    Inspected { request: u64, path: PathBuf, meta: Result<FileMetadata, String>, rekordbox: Vec<(String, String)> },
    Image { request: u64, thumb: Option<Thumb> },
    Written { request: u64, results: Vec<(PathBuf, Result<nullamp_meta::WriteReport, String>)> },
    SetlistSaved(Result<(), String>),
    Text { request: u64, path: PathBuf, text: Result<String, String> },
    Rediscovered { request: u64, setlist: Option<Setlist> },
    Restored { request: u64, target: PathBuf, result: Result<(), String> },
    TextWritten { path: PathBuf, result: Result<(), String> },
    /// Overview of the track loaded under `token`. Abandoned requests send nothing.
    /// `colors` (one per bucket, red to blue by pitch) only for a waveform that came from rekordbox.
    Waveform { token: u64, result: Result<Waveform, String>, colors: Option<Vec<[u8; 3]>> },
    /// Tempo of the track loaded under `token` (of the part of it between `span`, if that was
    /// asked for), `Ok(None)` for no clear beat. A whole-file request is not answered when the
    /// file's tags already state a BPM, and abandoned requests send nothing.
    Tempo { token: u64, span: Option<(f64, f64)>, result: Result<Option<Tempo>, String> },
    /// `done` of the `total` clips of an [`Workers::identify`] request have been asked about.
    IdentifyProgress { request: u64, done: usize, total: usize },
    /// What Shazam says about one track of the request.
    Identified { request: u64, entry: EntryId, verdict: nullamp_shazam::Verdict },
    /// The request is over: all tracks answered, or why it stopped. A cancelled request sends nothing.
    IdentifyDone { request: u64, result: Result<(), String> },
}

pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct Workers {
    interactive: Sender<Job>,
    bulk: Sender<Job>,
    waveform: Sender<WaveformRequest>,
    waveform_generation: Arc<AtomicU64>,
    tempo: Sender<TempoRequest>,
    tempo_generation: Arc<AtomicU64>,
    online: Sender<OnlineRequest>,
    identify: Sender<IdentifyRequest>,
    /// The identify request that may run; any other one stops at its next step.
    identify_current: Arc<AtomicU64>,
    pub results: Receiver<JobResult>,
    scan_generation: Arc<AtomicU64>,
    cover_generation: Arc<AtomicU64>,
    read_generation: Arc<AtomicU64>,
    desktop: Arc<crate::rekordbox::Desktop>,
    mixxx: Arc<crate::mixxx::Mixxx>,
    next_request: AtomicU64,
}

impl Workers {
    pub fn start(paths: AppPaths, waker: Waker) -> Self {
        let (interactive, interactive_rx) = crossbeam_channel::unbounded::<Job>();
        let (bulk, bulk_rx) = crossbeam_channel::unbounded::<Job>();
        let (results_tx, results) = crossbeam_channel::unbounded();
        let scan_generation = Arc::new(AtomicU64::new(1));
        let cover_generation = Arc::new(AtomicU64::new(1));
        let read_generation = Arc::new(AtomicU64::new(0));
        let sticks = Arc::new(crate::rekordbox::Sticks::default());
        let desktop = Arc::new(crate::rekordbox::Desktop::default());
        let mixxx = Arc::new(crate::mixxx::Mixxx::default());

        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        for i in 0..threads {
            let (irx, brx, tx, w, gen_, cover_gen, read_gen, p, sticks, desktop, mixxx) = (
                interactive_rx.clone(),
                bulk_rx.clone(),
                results_tx.clone(),
                Arc::clone(&waker),
                Arc::clone(&scan_generation),
                Arc::clone(&cover_generation),
                Arc::clone(&read_generation),
                paths.clone(),
                Arc::clone(&sticks),
                Arc::clone(&desktop),
                Arc::clone(&mixxx),
            );
            std::thread::Builder::new()
                .name(format!("nullamp-worker-{i}"))
                .spawn(move || {
                    let ctx = Ctx { paths: p, scan_generation: gen_, cover_generation: cover_gen, read_generation: read_gen, sticks, desktop, mixxx };
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

        let waveform_generation = Arc::new(AtomicU64::new(0));
        let (waveform, waveform_rx) = crossbeam_channel::unbounded::<WaveformRequest>();
        {
            let (tx, w, generation, dir, sticks, desktop, mixxx) = (results_tx.clone(), Arc::clone(&waker), Arc::clone(&waveform_generation), paths.waveform_cache(), Arc::clone(&sticks), Arc::clone(&desktop), Arc::clone(&mixxx));
            std::thread::Builder::new()
                .name("nullamp-waveform".into())
                .spawn(move || {
                    let removed = waveform_cache::prune(&dir, waveform_cache::MAX_AGE);
                    if removed > 0 {
                        log::debug!("removed {removed} unused waveform cache entries");
                    }
                    while let Ok(mut request) = waveform_rx.recv() {
                        // Skipping through a playlist queues requests faster than they run; only the newest matters.
                        while let Ok(newer) = waveform_rx.try_recv() {
                            request = newer;
                        }
                        // On a rekordbox stick the analysis is shown first and what rekordbox knows is cached
                        // afterwards (for the same file played from elsewhere later), so the cache never delays the waveform.
                        let found = request.rekordbox.then(|| sticks.waveform(&request.path).or_else(|| desktop.waveform(&request.path)).or_else(|| mixxx.waveform(&request.path))).flatten();
                        if let Some(found) = &found {
                            let result = JobResult::Waveform { token: request.token, result: Ok(found.wave.clone()), colors: found.colors.clone() };
                            if tx.send(result).is_err() {
                                return;
                            }
                            w();
                        }
                        if let Some(data) = sticks.data(&request.path).or_else(|| desktop.data(&request.path)).or_else(|| mixxx.data(&request.path))
                            && let Ok(fingerprint) = store::fingerprint(&request.path)
                        {
                            waveform_cache::store_rekordbox_data(&dir, &fingerprint, &data);
                            if let Some(found) = &found {
                                waveform_cache::store_rekordbox(&dir, &fingerprint, found);
                            }
                        }
                        if found.is_some() {
                            continue;
                        }
                        if let Some(result) = run_waveform(&dir, &generation, request) {
                            if tx.send(result).is_err() {
                                return;
                            }
                            w();
                        }
                    }
                })
                .expect("spawn waveform worker");
        }

        let tempo_generation = Arc::new(AtomicU64::new(0));
        let (tempo, tempo_rx) = crossbeam_channel::unbounded::<TempoRequest>();
        {
            let (tx, w, generation, dir) = (results_tx.clone(), Arc::clone(&waker), Arc::clone(&tempo_generation), paths.waveform_cache());
            std::thread::Builder::new()
                .name("nullamp-tempo".into())
                .spawn(move || {
                    while let Ok(mut request) = tempo_rx.recv() {
                        while let Ok(newer) = tempo_rx.try_recv() {
                            request = newer;
                        }
                        if let Some(result) = run_tempo(&dir, &generation, request) {
                            if tx.send(result).is_err() {
                                return;
                            }
                            w();
                        }
                    }
                })
                .expect("spawn tempo worker");
        }

        let (online, online_rx) = crossbeam_channel::unbounded::<OnlineRequest>();
        {
            let (tx, w, dir) = (results_tx.clone(), Arc::clone(&waker), paths.track_artwork_cache());
            std::thread::Builder::new()
                .name("nullamp-online".into())
                .spawn(move || {
                    let removed = crate::entry_art::prune(&dir);
                    if removed > 0 {
                        log::debug!("removed {removed} expired track artwork cache entries");
                    }
                    while let Ok(request) = online_rx.recv() {
                        let result = match request {
                            OnlineRequest::EntryArt(request) => run_entry_art(&dir, request),
                            OnlineRequest::TrackCover { request, artist, title } => JobResult::TrackCover { request, result: look_up_track_cover(&artist, &title) },
                        };
                        if tx.send(result).is_err() {
                            return;
                        }
                        w();
                    }
                })
                .expect("spawn online worker");
        }

        let identify_current = Arc::new(AtomicU64::new(0));
        let (identify, identify_rx) = crossbeam_channel::unbounded::<IdentifyRequest>();
        {
            let (tx, w, current) = (results_tx.clone(), Arc::clone(&waker), Arc::clone(&identify_current));
            std::thread::Builder::new()
                .name("nullamp-identify".into())
                .spawn(move || {
                    while let Ok(mut request) = identify_rx.recv() {
                        while let Ok(newer) = identify_rx.try_recv() {
                            request = newer;
                        }
                        let send = |r: JobResult| {
                            let sent = tx.send(r).is_ok();
                            w();
                            sent
                        };
                        run_identify(&current, request, &send);
                    }
                })
                .expect("spawn identify worker");
        }

        Workers {
            interactive,
            bulk,
            waveform,
            waveform_generation,
            tempo,
            tempo_generation,
            online,
            identify,
            identify_current,
            results,
            scan_generation,
            cover_generation,
            read_generation,
            desktop,
            mixxx,
            next_request: AtomicU64::new(1),
        }
    }

    /// Points the desktop rekordbox library at the given key (the standard location), or clears it
    /// when `key` is empty. Takes effect for the next lookup and the next "Open rekordbox collection".
    pub fn set_rekordbox_key(&self, key: &str) {
        let config = (!key.is_empty()).then(|| crate::rekordbox::DesktopConfig::standard(key.to_owned())).flatten();
        self.desktop.set_config(config);
    }

    /// Lets the Mixxx reader look at Mixxx's own library, or stops it. Takes effect for the next lookup.
    pub fn set_mixxx_enabled(&self, on: bool) {
        self.mixxx.set_enabled(on);
    }

    /// Find the cover of one track of a mix: in the cache, else online. Answered with
    /// [`JobResult::EntryArt`] under `key`.
    pub fn request_entry_art(&self, key: String, artist: String, title: String) {
        let _ = self.online.send(OnlineRequest::EntryArt(EntryArtRequest { key, artist, title }));
    }

    /// Name the tracks of the mix at `path` with Shazam, one after the other. Answered with
    /// [`JobResult::IdentifyProgress`] and [`JobResult::Identified`] as it goes and
    /// [`JobResult::IdentifyDone`] at the end, under `request`. A newer request replaces this one.
    pub fn identify(&self, request: u64, path: PathBuf, tracks: Vec<IdentifyTrack>) {
        self.identify_current.store(request, Ordering::SeqCst);
        let _ = self.identify.send(IdentifyRequest { request, path, tracks });
    }

    /// Stop the running identify request before its next clip or request.
    pub fn cancel_identify(&self) {
        self.identify_current.store(0, Ordering::SeqCst);
    }

    /// Find a cover for one file, for the user to add to its artwork. Answered with
    /// [`JobResult::TrackCover`] under `request`. Unlike the mix lookups it always asks the
    /// services and keeps nothing on disk: the user decides whether the image is used.
    pub fn request_track_cover(&self, request: u64, artist: String, title: String) {
        let _ = self.online.send(OnlineRequest::TrackCover { request, artist, title });
    }

    /// Store what rekordbox and Mixxx know about these files in the cache, a chunk at a time so the progress
    /// shows and a newer request can take over. Answered with [`JobResult::DjDataRead`] under the
    /// returned request id.
    pub fn read_dj_data(&self, paths: Vec<PathBuf>) -> u64 {
        let request = self.request_id();
        let generation = self.read_generation.fetch_add(1, Ordering::Relaxed) + 1;
        for chunk in paths.chunks(32) {
            self.submit(Job::ReadDjData { generation, request, paths: chunk.to_vec() });
        }
        request
    }

    pub fn request_id(&self) -> u64 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    pub fn submit(&self, job: Job) {
        let lane = match job {
            Job::ScanInfo { .. } | Job::CheckExists { .. } | Job::ExpandPaths { .. } | Job::ReadDjData { .. } => &self.bulk,
            _ => &self.interactive,
        };
        let _ = lane.send(job);
    }

    /// Compute (or load from the cache) the waveform of `path`, loaded under `token`. A
    /// decode already running for an earlier request stops at its next packet.
    pub fn request_waveform(&self, token: u64, path: PathBuf, rekordbox: bool) {
        let generation = self.waveform_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.waveform.send(WaveformRequest { generation, token, path, rekordbox });
    }

    /// Compute (or load from the cache) the tempo of `path`, loaded under `token`, of the whole
    /// file or of one track of it (`span`). An analysis already running for an earlier request
    /// stops at its next packet, so only the newest request is ever answered.
    pub fn request_tempo(&self, token: u64, path: PathBuf, span: Option<(f64, f64)>) {
        let generation = self.tempo_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tempo.send(TempoRequest { generation, token, path, span });
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

    /// Load cover thumbnails for these rows, abandoning any earlier request that has
    /// not finished. Small chunks keep the interactive lane responsive.
    pub fn request_covers(&self, paths: Vec<PathBuf>) {
        let generation = self.cover_generation.fetch_add(1, Ordering::Relaxed) + 1;
        for chunk in paths.chunks(8) {
            self.submit(Job::Covers { generation, paths: chunk.to_vec() });
        }
    }

    /// Abandon queued scans (e.g. after clearing a large playlist).
    pub fn cancel_scans(&self) {
        self.scan_generation.fetch_add(1, Ordering::Relaxed);
    }
}

/// `None` when a newer request replaced this one while it ran.
fn run_waveform(dir: &Path, current: &AtomicU64, request: WaveformRequest) -> Option<JobResult> {
    let WaveformRequest { generation, token, path, rekordbox } = request;
    let fingerprint = store::fingerprint(&path).ok();
    // The same file once analysed by rekordbox, now played from somewhere else.
    if let Some(found) = rekordbox.then(|| fingerprint.as_deref().and_then(|fp| waveform_cache::load_rekordbox(dir, fp))).flatten() {
        return Some(JobResult::Waveform { token, result: Ok(found.wave), colors: found.colors });
    }
    if let Some(wave) = fingerprint.as_deref().and_then(|fp| waveform_cache::load(dir, fp)) {
        return Some(JobResult::Waveform { token, result: Ok(wave), colors: None });
    }
    let cancelled = || current.load(Ordering::Relaxed) != generation;
    match nullamp_audio::compute_waveform(&path, &cancelled) {
        Ok(wave) => {
            if let Some(fp) = fingerprint {
                waveform_cache::store(dir, &fp, &wave);
            }
            Some(JobResult::Waveform { token, result: Ok(wave), colors: None })
        }
        Err(WaveformError::Cancelled) => None,
        Err(e) => Some(JobResult::Waveform { token, result: Err(e.to_string()), colors: None }),
    }
}

/// `None` when a newer request replaced this one while it ran, for a whole file whose tags
/// already state a BPM (that is the user's or another program's word and is shown as it is), or
/// for a file whose tags switch the tempo off with `-1`.
fn run_tempo(dir: &Path, current: &AtomicU64, request: TempoRequest) -> Option<JobResult> {
    let TempoRequest { generation, token, path, span } = request;
    if (span.is_none() && nullamp_meta::read_bpm(&path).is_some()) || nullamp_meta::read_bpm_off(&path) {
        return None;
    }
    // A track of a mix is cached under its own key, so editing its boundaries analyses it again.
    let key = store::fingerprint(&path).ok().map(|fp| match span {
        Some((from, to)) => format!("{fp}@{}-{}", (from * 1000.0).round(), (to * 1000.0).round()),
        None => fp,
    });
    if let Some(tempo) = key.as_deref().and_then(|k| waveform_cache::load_tempo(dir, k)) {
        return Some(JobResult::Tempo { token, span, result: Ok(tempo) });
    }
    let cancelled = || current.load(Ordering::Relaxed) != generation;
    match nullamp_audio::compute_tempo(&path, span, &cancelled) {
        Ok(tempo) => {
            if let Some(k) = key {
                waveform_cache::store_tempo(dir, &k, tempo);
            }
            Some(JobResult::Tempo { token, span, result: Ok(tempo) })
        }
        Err(TempoError::Cancelled) => None,
        Err(e) => Some(JobResult::Tempo { token, span, result: Err(e.to_string()) }),
    }
}

/// Decode a few clips of each track and ask Shazam about them, [`nullamp_shazam::PAUSE`] apart.
/// Stops quietly once `current` names another request, and stops with an error when Shazam
/// keeps refusing or every clip of a track failed to reach it (offline, say).
fn run_identify(current: &AtomicU64, request: IdentifyRequest, send: &dyn Fn(JobResult) -> bool) {
    use nullamp_shazam::{BACK_OFF, CLIP_SECONDS, CLIPS_PER_TRACK, Error, PAUSE, RATE};
    let id = request.request;
    let alive = || current.load(Ordering::SeqCst) == id;
    // Sleeps in short steps so a cancel takes effect quickly; false once cancelled.
    let wait = |d: std::time::Duration| {
        let until = std::time::Instant::now() + d;
        while std::time::Instant::now() < until {
            if !alive() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        alive()
    };
    let total = request.tracks.len() * CLIPS_PER_TRACK;
    let (mut done, mut first) = (0, true);
    let stop = |message: String| {
        if alive() {
            send(JobResult::IdentifyDone { request: id, result: Err(message) });
        }
    };
    let clock = nullamp_core::time::format_clock;
    let file = request.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for track in &request.tracks {
        log::info!("Shazam: asking about {} to {} of {file}", clock(track.start), clock(track.end));
        let starts = nullamp_shazam::clip_starts(track.start, track.end, CLIPS_PER_TRACK);
        let clips = match nullamp_audio::decode_clips(&request.path, RATE, &starts, CLIP_SECONDS, &|| !alive()) {
            Ok(c) => c,
            Err(nullamp_audio::MonoError::Cancelled) => return,
            Err(e) => return stop(format!("Cannot read the mix: {e}")),
        };
        let mut answers = Vec::new();
        let mut failure = None;
        for (at, clip) in starts.iter().zip(&clips) {
            if !first && !wait(PAUSE) {
                return;
            }
            first = false;
            let mut result = nullamp_shazam::recognise(clip);
            if result == Err(Error::TooMany) {
                log::info!("Shazam refuses requests for now, waiting {} s", BACK_OFF.as_secs());
                if !wait(BACK_OFF) {
                    return;
                }
                result = nullamp_shazam::recognise(clip);
            }
            match result {
                Ok(Some(song)) => {
                    log::info!(
                        "Shazam: clip at {}: {} - {} (at {} in the song, tempo {:+.1}%, pitch {:+.1}%)",
                        clock(*at),
                        song.artist,
                        song.title,
                        clock(song.offset),
                        song.time_skew * 100.0,
                        song.frequency_skew * 100.0
                    );
                    answers.push((*at, song));
                }
                Ok(None) => log::info!("Shazam: clip at {}: no match", clock(*at)),
                Err(Error::TooMany) => return stop("Shazam refuses more requests for now; try again in a few minutes".into()),
                Err(e) => {
                    log::warn!("Shazam: clip at {}: {e}", clock(*at));
                    failure = Some(e.to_string());
                }
            }
            done += 1;
            if !alive() || !send(JobResult::IdentifyProgress { request: id, done, total }) {
                return;
            }
        }
        if let (Some(e), true) = (&failure, answers.is_empty()) {
            return stop(format!("Cannot reach Shazam: {e}"));
        }
        let verdict = nullamp_shazam::agree(&answers);
        match &verdict {
            nullamp_shazam::Verdict::Named { song, support } => log::info!("Shazam: named {} - {} ({support} of {} clips agree)", song.artist, song.title, starts.len()),
            nullamp_shazam::Verdict::Unsure { candidates } if candidates.is_empty() => log::info!("Shazam: no clip matched a song"),
            nullamp_shazam::Verdict::Unsure { candidates } => log::info!(
                "Shazam: unsure, the clips do not agree: {}",
                candidates.iter().map(|s| format!("{} - {}", s.artist, s.title)).collect::<Vec<_>>().join(" | ")
            ),
        }
        if !alive() || !send(JobResult::Identified { request: id, entry: track.entry, verdict }) {
            return;
        }
    }
    if alive() {
        send(JobResult::IdentifyDone { request: id, result: Ok(()) });
    }
}

fn run_entry_art(dir: &Path, request: EntryArtRequest) -> JobResult {
    let EntryArtRequest { key, artist, title } = request;
    let cached = match crate::entry_art::load(dir, &key) {
        Some(crate::entry_art::Cached::Image(file)) => std::fs::read(&file).ok().and_then(|bytes| entry_art_from(&bytes, file)).map(Some),
        Some(crate::entry_art::Cached::Nothing) => Some(None),
        None => None,
    };
    let found = match cached {
        Some(art) => Ok(art),
        None => look_up_entry_art(dir, &key, &artist, &title),
    };
    let result = found.map(|art| art.unwrap_or_else(|| placeholder_entry_art(&key)));
    JobResult::EntryArt { key, result }
}

fn look_up_entry_art(dir: &Path, key: &str, artist: &str, title: &str) -> Result<Option<EntryArt>, String> {
    let program = concat!("Nullamp/", env!("CARGO_PKG_VERSION"));
    let Some(found) = nullamp_artwork::find(artist, title, program).map_err(|e| e.to_string())? else {
        log::info!("no artwork found online for {artist} - {title}");
        crate::entry_art::store_nothing(dir, key);
        return Ok(None);
    };
    let bytes = nullamp_artwork::download(&found.candidate.image_url, program).map_err(|e| e.to_string())?;
    let file = crate::entry_art::store_image(dir, key, &bytes).ok_or("cannot write the artwork cache")?;
    log::info!("artwork for {artist} - {title}: {} by {} on {}", found.candidate.release, found.candidate.artist, found.source.name());
    entry_art_from(&bytes, file).map(Some).ok_or_else(|| "the cover is not an image".to_owned())
}

fn look_up_track_cover(artist: &str, title: &str) -> Result<Option<OnlineCover>, String> {
    let program = concat!("Nullamp/", env!("CARGO_PKG_VERSION"));
    let Some(found) = nullamp_artwork::find(artist, title, program).map_err(|e| e.to_string())? else {
        log::info!("no artwork found online for {artist} - {title}");
        return Ok(None);
    };
    let bytes = nullamp_artwork::download(&found.candidate.image_url, program).map_err(|e| e.to_string())?;
    let mime = if bytes.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
    let source = format!("{} by {} on {}", found.candidate.release, found.candidate.artist, found.source.name());
    log::info!("artwork for {artist} - {title}: {source}");
    Ok(Some(OnlineCover { data: Arc::from(bytes), mime, source }))
}

fn entry_art_from(bytes: &[u8], file: PathBuf) -> Option<EntryArt> {
    let art = decode_thumb(bytes, 512)?;
    let retro_art = crate::retro_art::retro_thumb(&art);
    Some(EntryArt { art, retro_art, file: Some(file) })
}

/// Drawn on each request instead of cached, so a better generator also redraws old placeholders.
fn placeholder_entry_art(key: &str) -> EntryArt {
    let art = crate::placeholder_art::generate(key);
    let retro_art = crate::retro_art::retro_thumb(&art);
    EntryArt { art, retro_art, file: None }
}

/// The placeholder for a file without a cover, drawn from its artist and title (its file name
/// when the tags have neither) so the same song always gets the same picture.
fn placeholder_file_art(path: &Path, meta: Option<&FileMetadata>) -> Thumb {
    let field = |f| meta.and_then(|m| m.fields.get(&f)).map(|v| v.trim()).unwrap_or_default();
    let (artist, title) = (field(nullamp_meta::Field::Artist), field(nullamp_meta::Field::Title));
    let key = if artist.is_empty() && title.is_empty() {
        crate::entry_art::key("", &path.file_stem().unwrap_or_default().to_string_lossy())
    } else {
        crate::entry_art::key(artist, title)
    };
    crate::placeholder_art::generate(&key)
}

struct Ctx {
    paths: AppPaths,
    scan_generation: Arc<AtomicU64>,
    cover_generation: Arc<AtomicU64>,
    read_generation: Arc<AtomicU64>,
    sticks: Arc<crate::rekordbox::Sticks>,
    desktop: Arc<crate::rekordbox::Desktop>,
    mixxx: Arc<crate::mixxx::Mixxx>,
}

/// Stores what the DJ software knows about each of `paths` in the cache, by the file's fingerprint
/// (what playing it would store): rekordbox's record and waveform for a file on a stick or in the
/// desktop library, else Mixxx's for a file in its library. Returns how many files that was and how
/// many of them had an analysed waveform. A file that cannot be read is skipped.
fn read_dj_data(ctx: &Ctx, paths: Vec<PathBuf>) -> (usize, usize) {
    let dir = ctx.paths.waveform_cache();
    let (mut stored, mut waveforms) = (0, 0);
    for path in paths {
        let track = ctx.sticks.track(&path).or_else(|| ctx.desktop.track(&path));
        let Some(data) = track.as_ref().map(crate::rekordbox::Data::of).or_else(|| ctx.mixxx.data(&path)) else { continue };
        let Ok(fingerprint) = store::fingerprint(&path) else { continue };
        waveform_cache::store_rekordbox_data(&dir, &fingerprint, &data);
        stored += 1;
        if let Some(found) = track.and_then(|t| crate::rekordbox::analysed(&path, &t)).or_else(|| ctx.mixxx.waveform(&path)) {
            waveform_cache::store_rekordbox(&dir, &fingerprint, &found);
            waveforms += 1;
        }
    }
    (stored, waveforms)
}

/// Longest side of playlist cover thumbnails: the hover preview at full size, the row thumbnail mipmapped down.
const COVER_THUMB: u32 = 128;

fn run_job(ctx: &Ctx, job: Job) -> Vec<JobResult> {
    match job {
        Job::ScanInfo { generation, paths } => {
            let mut out = Vec::with_capacity(paths.len());
            for path in paths {
                if ctx.scan_generation.load(Ordering::Relaxed) != generation {
                    break;
                }
                let mut info = nullamp_meta::read_track_info(&path).map_err(|e| e.to_string());
                if let Ok(info) = &mut info
                    && !ctx.sticks.fill(&path, info)
                    && !ctx.desktop.fill(&path, info)
                {
                    ctx.mixxx.fill(&path, info);
                }
                out.push(JobResult::Info { path, info });
            }
            out
        }
        Job::Covers { generation, paths } => {
            let mut out = Vec::with_capacity(paths.len());
            for path in paths {
                if ctx.cover_generation.load(Ordering::Relaxed) != generation {
                    break;
                }
                let art = nullamp_meta::read_front_cover(&path).ok().flatten().and_then(|c| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&*c.data, &mut h);
                    decode_thumb(&c.data, COVER_THUMB).map(|t| (std::hash::Hasher::finish(&h), t))
                });
                out.push(JobResult::Cover { path, art });
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
        Job::OpenSticks => vec![JobResult::Sticks(crate::rekordbox::load_mounted_sticks())],
        Job::ListCollection => vec![JobResult::CollectionList(ctx.desktop.overview())],
        Job::OpenCollection { all, ids, read_data } => vec![JobResult::Collection { read_data, result: ctx.desktop.open_playlists(all, &ids) }],
        Job::ListMixxx => vec![JobResult::MixxxList(ctx.mixxx.overview())],
        Job::OpenMixxx { all, ids, read_data } => vec![JobResult::Mixxx { read_data, result: ctx.mixxx.open_lists(all, &ids) }],
        Job::ReadDjData { generation, request, paths } => {
            if ctx.read_generation.load(Ordering::Relaxed) != generation {
                return Vec::new();
            }
            let done = paths.len();
            let (stored, waveforms) = read_dj_data(ctx, paths);
            vec![JobResult::DjDataRead { request, done, stored, waveforms }]
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
            // From the stick's library, the desktop collection or Mixxx when the file is in one, else
            // what was cached when it last played from one.
            let record = ctx.sticks.data(&path).or_else(|| ctx.desktop.data(&path)).or_else(|| ctx.mixxx.data(&path)).or_else(|| {
                let fingerprint = store::fingerprint(&path).ok()?;
                waveform_cache::load_rekordbox_data(&ctx.paths.waveform_cache(), &fingerprint)
            });
            let rekordbox = record.map(|d| d.scalars()).unwrap_or_default();
            vec![JobResult::Inspected { request, path, meta, rekordbox }]
        }
        Job::LogFingerprint { path } => {
            match nullamp_audio::compute_fingerprint(&path) {
                Ok(fp) => {
                    log::info!("fingerprint of {} ({:.1}s of {}s)", path.display(), fp.covered, fp.duration);
                    log::info!("curl -d 'client=KEY&meta=recordings+releasegroups&duration={}&fingerprint={}' https://api.acoustid.org/v2/lookup", fp.duration, fp.text);
                }
                Err(e) => log::warn!("cannot fingerprint {}: {e}", path.display()),
            }
            Vec::new()
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
    let discovered = discover_local_setlist(&path, meta.as_ref().ok());
    let setlist = choose_setlist(saved.map(|s| s.setlist), discovered.as_ref());
    let tracklist_urls = meta.as_ref().map(tracklist_urls).unwrap_or_default();
    let retro_art = match &art {
        Some(art) => crate::retro_art::retro_thumb(art),
        None => {
            // The same generated cover a mix track gets, so a file without one is not a bare record.
            let placeholder = placeholder_file_art(&path, meta.as_ref().ok());
            let retro = crate::retro_art::retro_thumb(&placeholder);
            art = Some(placeholder);
            retro
        }
    };
    let track_info = meta.as_ref().ok().map(|m| {
        let mut info = m.track_info();
        // A file in rekordbox (a stick, or the desktop library) takes its key and BPM from there, one
        // in the Mixxx library from Mixxx; the same file anywhere else from what was cached when it
        // last played from one of them.
        if !ctx.sticks.fill(&path, &mut info)
            && !ctx.desktop.fill(&path, &mut info)
            && !ctx.mixxx.fill(&path, &mut info)
            && let Some(data) = fingerprint.as_deref().and_then(|fp| waveform_cache::load_rekordbox_data(&ctx.paths.waveform_cache(), fp))
        {
            crate::rekordbox::fill_from_cache(&mut info, &data);
        }
        info
    });
    NowPlayingInfo { token, path, meta, art, retro_art, art_file, fingerprint, setlist, discovered, tracklist_urls, track_info }
}

/// A saved setlist wins over what the file's own sources hold, because it carries the
/// user's work. The exception is an untimed list nobody edited (text read before a parser
/// learned the cue sheet, say): it stays saved on disk but gives way to a timed discovery.
fn choose_setlist(saved: Option<Setlist>, discovered: Option<&Setlist>) -> Option<(Setlist, SetlistOrigin)> {
    match (saved, discovered) {
        (Some(s), Some(d)) if s.is_untouched_reference() && d.has_times() => Some((d.clone(), SetlistOrigin::Discovered)),
        (Some(s), _) => Some((s, SetlistOrigin::Saved)),
        (None, d) => d.map(|d| (d.clone(), SetlistOrigin::Discovered)),
    }
}

/// Local setlist sources, in order: a cue sheet with exactly the file's name (`mix.cue`
/// for `mix.wav`, which the user put there on purpose), embedded chapters, a cue sheet
/// stored in a tag, any other cue sheet next to the file.
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

    let same_name = path.with_extension("cue");
    if let Some(s) = setlist_from_cue_file(&same_name, &file_name, &mix) {
        return Some(s);
    }

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
                    e.provenance = Provenance::CueSheet { path: Provenance::EMBEDDED_CUE_PATH.into() };
                }
                fill_mix(&mut s.mix, &mix);
                return Some(s);
            }
        }
    }
    for cue_path in adjacent_cue_sheets(path) {
        if let Some(s) = setlist_from_cue_file(&cue_path, &file_name, &mix) {
            return Some(s);
        }
    }
    // A plain track list in the description, often without times: reference only.
    let described = meta.and_then(|m| {
        let description = m.other_items.iter().find(|(k, _)| k == "Description").map(|(_, v)| v);
        m.fields.get(&nullamp_meta::Field::Comment).or(description).and_then(|t| tracklist_text::parse_description(t))
    });
    described.map(|mut s| {
        fill_mix(&mut s.mix, &mix);
        s
    })
}

fn setlist_from_cue_file(cue_path: &Path, audio_file_name: &str, mix: &MixInfo) -> Option<Setlist> {
    let bytes = std::fs::read(cue_path).ok()?;
    let mut s = cue::parse(&bytes).to_setlist(audio_file_name, &cue_path.to_string_lossy())?;
    if s.entries.is_empty() {
        return None;
    }
    fill_mix(&mut s.mix, mix);
    Some(s)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures").join(name)
    }

    #[test]
    fn an_untouched_untimed_saved_setlist_yields_to_a_timed_discovery() {
        let entry = |start, provenance| nullamp_core::setlist::SetlistEntry::new("A", "One", start, provenance);
        let imported = || Provenance::Imported { origin_url: None };
        let stale = Setlist { entries: vec![entry(None, imported()), entry(None, imported())], ..Default::default() };
        let found = Setlist { entries: vec![entry(Some(0.0), Provenance::EmbeddedChapters)], ..Default::default() };
        let untimed_found = Setlist { entries: vec![entry(None, Provenance::TagDescription)], ..Default::default() };

        let (s, origin) = choose_setlist(Some(stale.clone()), Some(&found)).unwrap();
        assert_eq!((s.entries.len(), origin), (1, SetlistOrigin::Discovered));

        // Nothing better to offer: the saved list stays.
        assert_eq!(choose_setlist(Some(stale.clone()), Some(&untimed_found)).unwrap().1, SetlistOrigin::Saved);
        assert_eq!(choose_setlist(Some(stale.clone()), None).unwrap().1, SetlistOrigin::Saved);

        // The user's work stays: a correction, an entry typed in, or a timed list.
        let mut edited = stale.clone();
        edited.entries[0].user_edited = true;
        assert_eq!(choose_setlist(Some(edited), Some(&found)).unwrap().1, SetlistOrigin::Saved);
        let mut typed = stale.clone();
        typed.entries.push(entry(None, Provenance::Manual));
        assert_eq!(choose_setlist(Some(typed), Some(&found)).unwrap().1, SetlistOrigin::Saved);
        let timed = Setlist { entries: vec![entry(Some(5.0), imported())], ..Default::default() };
        assert_eq!(choose_setlist(Some(timed), Some(&found)).unwrap().1, SetlistOrigin::Saved);

        assert_eq!(choose_setlist(None, Some(&found)).unwrap().1, SetlistOrigin::Discovered);
        assert!(choose_setlist(None, None).is_none());
    }

    #[test]
    fn reading_rekordbox_ahead_caches_the_record_and_waveform_of_stick_files_only() {
        let home = tempfile::tempdir().unwrap();
        let ctx = Ctx {
            paths: AppPaths { config: home.path().join("config"), data: home.path().join("data"), cache: home.path().join("cache") },
            scan_generation: Arc::new(AtomicU64::new(1)),
            cover_generation: Arc::new(AtomicU64::new(1)),
            read_generation: Arc::new(AtomicU64::new(1)),
            sticks: Arc::new(crate::rekordbox::Sticks::default()),
            desktop: Arc::new(crate::rekordbox::Desktop::default()),
            mixxx: Arc::new(crate::mixxx::Mixxx::default()),
        };
        // The fixture stick has no audio files, so copy its library and add one.
        let stick = home.path().join("STICK");
        for file in ["PIONEER/rekordbox/export.pdb", "PIONEER/USBANLZ/P011/00020633/ANLZ0000.DAT", "PIONEER/USBANLZ/P011/00020633/ANLZ0000.EXT"] {
            std::fs::create_dir_all(stick.join(file).parent().unwrap()).unwrap();
            std::fs::copy(fixture("rekordbox").join(file), stick.join(file)).unwrap();
        }
        let on_stick = stick.join("Contents/Veracocha/Carte Blanche/Veracocha - Carte Blanche (Original Mix).mp3");
        std::fs::create_dir_all(on_stick.parent().unwrap()).unwrap();
        std::fs::write(&on_stick, vec![7u8; 4096]).unwrap();
        let elsewhere = fixture("beat_120.mp3");
        let (stored, waveforms) = read_dj_data(&ctx, vec![on_stick.clone(), elsewhere.clone(), home.path().join("gone.mp3")]);
        assert_eq!((stored, waveforms), (1, 1));
        let (dir, fingerprint) = (ctx.paths.waveform_cache(), store::fingerprint(&on_stick).unwrap());
        let data = waveform_cache::load_rekordbox_data(&dir, &fingerprint).expect("record cached");
        assert_eq!((data.key.as_deref(), data.bpm), (Some("Bbm"), Some(140.02)));
        assert!(waveform_cache::load_rekordbox(&dir, &fingerprint).is_some(), "waveform cached");
        assert!(waveform_cache::load_rekordbox_data(&dir, &store::fingerprint(&elsewhere).unwrap()).is_none());
    }

    #[test]
    fn reading_ahead_caches_the_record_and_waveform_of_a_file_in_mixxx() {
        let home = tempfile::tempdir().unwrap();
        let file = fixture("beat_120.mp3");
        let mut track = crate::mixxx::tests::track(1, file.to_str().unwrap());
        (track.key, track.bpm, track.rating, track.analysis) = (Some("Am".into()), Some(120.0), 3, Some(fixture("mixxx/analysis/11")));
        let library = nullamp_mixxx::Library { tracks: vec![track], lists: Vec::new() };
        let mixxx = crate::mixxx::tests::mixxx_with_library(home.path(), library);
        mixxx.set_enabled(true);
        let ctx = Ctx {
            paths: AppPaths { config: home.path().join("config"), data: home.path().join("data"), cache: home.path().join("cache") },
            scan_generation: Arc::new(AtomicU64::new(1)),
            cover_generation: Arc::new(AtomicU64::new(1)),
            read_generation: Arc::new(AtomicU64::new(1)),
            sticks: Arc::new(crate::rekordbox::Sticks::default()),
            desktop: Arc::new(crate::rekordbox::Desktop::default()),
            mixxx: Arc::new(mixxx),
        };
        let other = fixture("tone_mono_22k.wav");
        let (stored, waveforms) = read_dj_data(&ctx, vec![file.clone(), other.clone()]);
        assert_eq!((stored, waveforms), (1, 1));
        let (dir, fingerprint) = (ctx.paths.waveform_cache(), store::fingerprint(&file).unwrap());
        let data = waveform_cache::load_rekordbox_data(&dir, &fingerprint).expect("record cached");
        assert_eq!((data.key.as_deref(), data.bpm, data.rating, data.color.as_deref()), (Some("Am"), Some(120.0), 3, Some("#00ff80")));
        let found = waveform_cache::load_rekordbox(&dir, &fingerprint).expect("waveform cached");
        assert_eq!(found.wave.peak.len(), 8, "one bucket per sample of the Mixxx overview");
        assert_eq!(found.wave.peak.iter().max(), Some(&255), "scaled to full");
        assert_eq!(found.colors.map(|c| c.len()), Some(8));
        assert!(waveform_cache::load_rekordbox_data(&dir, &store::fingerprint(&other).unwrap()).is_none());
    }

    #[test]
    fn a_superseded_read_does_nothing() {
        let home = tempfile::tempdir().unwrap();
        let ctx = Ctx {
            paths: AppPaths { config: home.path().into(), data: home.path().into(), cache: home.path().into() },
            scan_generation: Arc::new(AtomicU64::new(1)),
            cover_generation: Arc::new(AtomicU64::new(1)),
            read_generation: Arc::new(AtomicU64::new(2)),
            sticks: Arc::new(crate::rekordbox::Sticks::default()),
            desktop: Arc::new(crate::rekordbox::Desktop::default()),
            mixxx: Arc::new(crate::mixxx::Mixxx::default()),
        };
        let job = Job::ReadDjData { generation: 1, request: 7, paths: vec![fixture("beat_120.mp3")] };
        assert!(run_job(&ctx, job).is_empty());
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
    fn same_name_cue_beats_chapters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2019-07-29_22h29m03s.mp3");
        std::fs::copy(fixture("mix_chapters.mp3"), &path).unwrap();
        // The sheet names a WAV, as rekordbox recordings do.
        std::fs::write(
            dir.path().join("2019-07-29_22h29m03s.cue"),
            "FILE \"2019-07-29_22h29m03s.wav\" WAVE\n TRACK 01 AUDIO\n TITLE \"Mine\"\n PERFORMER \"P\"\n INDEX 01 00:00:00\n",
        )
        .unwrap();
        let meta = nullamp_meta::read(&path).unwrap();
        assert!(meta.chapters.len() >= 2);
        let s = discover_local_setlist(&path, Some(&meta)).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].title, "Mine");
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
    fn a_file_without_a_cover_gets_the_placeholder_of_its_artist_and_title() {
        let path = fixture("tagged_v24.mp3");
        let mut meta = nullamp_meta::read(&path).unwrap();
        meta.fields.insert(nullamp_meta::Field::Artist, "Some Artist".into());
        meta.fields.insert(nullamp_meta::Field::Title, "Some Title".into());
        let art = placeholder_file_art(&path, Some(&meta));
        assert_eq!(art.rgba, crate::placeholder_art::generate(&crate::entry_art::key("Some Artist", "Some Title")).rgba);
        // Without tags the file name stands in, and differs from another file's.
        let other = placeholder_file_art(&fixture("tone.flac"), None);
        assert_ne!(placeholder_file_art(&path, None).rgba, other.rgba);
    }

    /// Needs the network.
    #[test]
    #[ignore = "asks Deezer; run with --ignored"]
    fn looks_up_and_caches_entry_art() {
        let dir = tempfile::tempdir().unwrap();
        let (artist, title) = ("Anil Aras", "Dexed Tek");
        let key = crate::entry_art::key(artist, title);
        let request = || EntryArtRequest { key: key.clone(), artist: artist.into(), title: title.into() };
        let JobResult::EntryArt { result: Ok(first), .. } = run_entry_art(dir.path(), request()) else { panic!("no cover found") };
        let file = first.file.clone().expect("a found cover has a file");
        assert!(first.art.size[0] >= 256 && first.retro_art.is_some());
        assert!(matches!(crate::entry_art::load(dir.path(), &key), Some(crate::entry_art::Cached::Image(p)) if p == file));
        let JobResult::EntryArt { result: Ok(again), .. } = run_entry_art(dir.path(), request()) else { panic!("cache missed") };
        assert_eq!(again.file, first.file);
    }

    #[test]
    fn a_cached_miss_gets_a_placeholder_without_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let key = crate::entry_art::key("Pete Oak", "Toro");
        crate::entry_art::store_nothing(dir.path(), &key);
        let request = EntryArtRequest { key: key.clone(), artist: "Pete Oak".into(), title: "Toro".into() };
        let JobResult::EntryArt { result: Ok(art), .. } = run_entry_art(dir.path(), request) else { panic!("no placeholder") };
        assert!(art.file.is_none() && art.retro_art.is_some());
        assert_eq!(art.art.rgba, crate::placeholder_art::generate(&key).rgba);
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
