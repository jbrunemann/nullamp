//! Downloads a SoundCloud track or DJ set.
//!
//! SoundCloud serves audio as HLS: a short-lived signed `.m3u8` that lists an fMP4 init segment and a
//! few hundred media segments. [`download`] walks the three API hops the web player does (resolve the
//! track address, request the signed playlist, read the segment list), fetches the segments in
//! parallel and concatenates them (init first, then each segment in order) into a single `.m4a` file.
//! That file is a valid fragmented MP4 and plays in symphonia (which nullamp uses to decode), in
//! ffmpeg and in QuickTime.
//!
//! The API wants a `client_id`: a long alphanumeric string the web player gets by loading its own
//! bundles. [`client_id::fetch`] loads the player and scrapes the first bundle whose body carries
//! `client_id:"..."`. The id is cached under `cache_dir()/soundcloud/client_id.txt` for a day, so a
//! long session makes one scrape. Scraping and using that id to pull audio is against SoundCloud's
//! terms of service; the user chose this knowing that, like `nullamp-shazam` and `nullamp-tracklists`.
//!
//! All requests go through `nullamp-http` (`curl`, or `ureq` on Android/iOS). [`download`] blocks for
//! as long as the download takes (seconds for a track, a minute or two for a long set): call it from
//! a worker thread.

pub mod client_id;
pub mod fmp4;
pub mod hls;
pub mod resolve;

pub(crate) mod download;

use std::path::{Path, PathBuf};

/// User-Agent used for every request. SoundCloud itself does not seem to check this, but the signed
/// CloudFront URLs that serve segments 403 anything without a browser-shaped UA.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
/// Default worker threads for the parallel segment fetch. Four keeps curl spawns reasonable.
pub const WORKERS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// Not a soundcloud.com address (or an `on.soundcloud.com` short link).
    #[error("\"{0}\" is not a SoundCloud track address")]
    Address(String),
    /// No network, a server error, a timeout. Carries curl's message.
    #[error("{0}")]
    Fetch(String),
    /// SoundCloud has no such track for an anonymous visitor: the uploader removed it, it is private,
    /// or the link is wrong.
    #[error("SoundCloud has no public track at this address; it may be private")]
    NotPublic,
    /// The web player's `client_id` could not be scraped. The bundle layout changed.
    #[error("Could not find SoundCloud's client_id in any of the player bundles")]
    NoClientId,
    /// The track has no reachable audio stream: a preview-only upload, a Go+ exclusive, or a format
    /// this crate does not know how to download.
    #[error("This track has no downloadable audio stream")]
    NoAudio,
    /// The resolve answer or the playlist did not look like what the browser gets.
    #[error("{0}")]
    Reply(String),
    #[error("{0}")]
    Io(String),
}

impl From<nullamp_http::Error> for Error {
    fn from(e: nullamp_http::Error) -> Self {
        match e {
            nullamp_http::Error::NoCurl => Error::NoCurl,
            nullamp_http::Error::Fetch(m) => Error::Fetch(m),
            nullamp_http::Error::TooLong => Error::Reply("The answer is too long".into()),
        }
    }
}

/// The facts about a track the resolve endpoint returns. Enough to tag the file and to let a caller
/// show a progress line that names the track.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u64,
    pub title: String,
    /// `user.username` of the uploader.
    pub artist: String,
    pub duration_ms: u64,
    /// The 500 px square JPEG the web player shows. `None` for a track with no cover.
    pub artwork_url: Option<String>,
    /// Canonical `https://soundcloud.com/<user>/<slug>` address.
    pub permalink_url: String,
}

/// What [`download`] produced.
#[derive(Clone, Debug)]
pub struct Downloaded {
    pub track: Track,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Progress ticks the download emits. The caller may use them for a progress bar.
#[derive(Clone, Debug)]
pub enum Step<'a> {
    /// Looking up the `client_id`. Only sent when it is not in the cache.
    LookingUpClientId,
    /// About to resolve the track address and ask for the signed playlist.
    Resolving,
    /// The playlist is in; segment downloads are about to start.
    Streaming { track: &'a Track, segments: usize },
    /// `done` of `total` segments have been written to disk, totalling `bytes`.
    Segment { done: usize, total: usize, bytes: u64 },
    /// The file is on disk.
    Done { path: &'a Path, bytes: u64 },
}

/// Download the track at `url` into `out_dir`. `cache_dir`, if given, is the folder in which the
/// scraped `client_id` is remembered between calls (one subfolder is created there). `progress` is
/// called from the thread that drives the download, in order; it may update a UI or do nothing.
///
/// The output file is `<out_dir>/<artist> - <title>.m4a`. If a file with that name is there, it is
/// overwritten.
pub fn download(url: &str, out_dir: &Path, cache_dir: Option<&Path>, progress: impl FnMut(Step)) -> Result<Downloaded, Error> {
    download::download(url, out_dir, cache_dir, progress)
}

/// A filename the host filesystems accept, built from the artist and the title. The track id is
/// appended to tell two uploads with the same name apart.
pub fn file_name(artist: &str, title: &str, id: u64) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '\0' => ' ',
                '<' | '>' | '"' | '|' | '?' | '*' => ' ',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (artist, title) = (clean(artist), clean(title));
    let stem = if artist.is_empty() { title } else if title.is_empty() { artist } else { format!("{artist} - {title}") };
    // Keep names short enough for every filesystem the project targets.
    let stem = if stem.chars().count() > 150 { stem.chars().take(150).collect::<String>() } else { stem };
    if stem.is_empty() { format!("soundcloud-{id}.m4a") } else { format!("{stem}.m4a") }
}

#[cfg(test)]
mod tests;
