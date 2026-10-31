//! The orchestration: parse the address, scrape the `client_id`, resolve, get the signed playlist,
//! fetch init + segments in parallel, write the file.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::{Downloaded, Error, Step, Track, USER_AGENT, WORKERS, client_id, file_name, fmp4, hls, resolve};

const SEGMENT_MAX_BYTES: u64 = 32 << 20;
const INIT_MAX_BYTES: u64 = 1 << 20;
const PROGRESSIVE_MAX_BYTES: u64 = 512 << 20;
const SEGMENT_TIMEOUT_SECS: u32 = 60;
const PLAYLIST_MAX_BYTES: u64 = 2 << 20;

/// What `track_url(url)` can return. SoundCloud `on.soundcloud.com/<code>` short links need a HEAD/GET to resolve;
/// nullamp-http follows redirects on a GET so we read the final location out of the response.
pub(crate) enum TrackUrl {
    Direct(String),
    Short(String),
}

/// Entry point. See [`crate::download`].
pub fn download(url: &str, out_dir: &Path, cache_dir: Option<&Path>, mut progress: impl FnMut(Step)) -> Result<Downloaded, Error> {
    let track_url = canonical_track_url(url)?;

    let scraping = cache_dir.is_some_and(|dir| !client_id::cache_fresh(dir));
    let cache_scrape_needed = cache_dir.is_none() || scraping;
    if cache_scrape_needed {
        progress(Step::LookingUpClientId);
    }
    let client_id = client_id::fetch(cache_dir)?;

    progress(Step::Resolving);
    let resolved = resolve::resolve(&track_url, &client_id)?;
    let signed_url = resolve::signed_playlist_url(&resolved.transcoding_url, &client_id, &resolved.track_authorization)?;

    std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(format!("Cannot create {}: {e}", out_dir.display())))?;
    let path = out_dir.join(file_name(&resolved.track.artist, &resolved.track.title, resolved.track.id));

    let bytes = if resolved.format.contains("progressive") {
        write_progressive(&signed_url, &path, &resolved.track, &mut progress)?
    } else {
        write_hls(&signed_url, &path, &resolved.track, &mut progress)?
    };

    progress(Step::Done { path: &path, bytes });
    Ok(Downloaded { track: resolved.track, path, bytes })
}

fn write_progressive(signed_url: &str, path: &Path, track: &Track, progress: &mut dyn FnMut(Step)) -> Result<u64, Error> {
    progress(Step::Streaming { track, segments: 1 });
    let request = nullamp_http::Request::get(signed_url, USER_AGENT).max_bytes(PROGRESSIVE_MAX_BYTES).timeout_secs(SEGMENT_TIMEOUT_SECS * 4);
    let response = nullamp_http::send(&request)?.ok()?;
    let bytes = response.body.len() as u64;
    std::fs::write(path, &response.body).map_err(|e| Error::Io(format!("Cannot write {}: {e}", path.display())))?;
    progress(Step::Segment { done: 1, total: 1, bytes });
    Ok(bytes)
}

fn write_hls(signed_url: &str, path: &Path, track: &Track, progress: &mut dyn FnMut(Step)) -> Result<u64, Error> {
    let request = nullamp_http::Request::get(signed_url, USER_AGENT).max_bytes(PLAYLIST_MAX_BYTES).timeout_secs(SEGMENT_TIMEOUT_SECS);
    let playlist_text = nullamp_http::send(&request)?.ok()?.text();
    let playlist = hls::parse(&playlist_text)?;

    let mut file = File::create(path).map_err(|e| Error::Io(format!("Cannot create {}: {e}", path.display())))?;
    let mut bytes = 0u64;

    if let Some(init_url) = &playlist.init {
        let mut init = fetch_bytes(init_url, INIT_MAX_BYTES)?;
        // Rewrite mvhd/mdhd duration so decoders don't report "ten seconds" from the first fragment.
        fmp4::patch_init_duration(&mut init, playlist.duration_secs());
        file.write_all(&init).map_err(|e| Error::Io(e.to_string()))?;
        bytes += init.len() as u64;
    }

    progress(Step::Streaming { track, segments: playlist.segments.len() });

    bytes += fetch_segments_in_order(&playlist.segments, playlist.init.is_some(), &mut file, |done, total, written| {
        progress(Step::Segment { done, total, bytes: bytes + written });
    })?;

    Ok(bytes)
}

fn fetch_bytes(url: &str, max: u64) -> Result<Vec<u8>, Error> {
    let request = nullamp_http::Request::get(url, USER_AGENT).max_bytes(max).timeout_secs(SEGMENT_TIMEOUT_SECS);
    Ok(nullamp_http::send(&request)?.ok()?.body)
}

/// Fetch segments on `WORKERS` threads and write them to `dest` in index order. Returns the total
/// bytes written. Errors from any worker short-circuit the whole download.
/// `fragmented` says the segments are CMAF fragments, whose segment index must go (see [`fmp4`]).
fn fetch_segments_in_order(segments: &[hls::Segment], fragmented: bool, dest: &mut File, mut progress: impl FnMut(usize, usize, u64)) -> Result<u64, Error> {
    let total = segments.len();
    let (tx_work, rx_work) = crossbeam_channel::unbounded::<usize>();
    let (tx_done, rx_done) = crossbeam_channel::unbounded::<(usize, Result<Vec<u8>, Error>)>();
    for i in 0..total {
        tx_work.send(i).unwrap();
    }
    drop(tx_work);

    let urls: Vec<&str> = segments.iter().map(|s| s.url.as_str()).collect();
    let mut written = 0u64;

    std::thread::scope(|scope| -> Result<u64, Error> {
        for _ in 0..WORKERS.min(total.max(1)) {
            let rx_work = rx_work.clone();
            let tx_done = tx_done.clone();
            let urls = &urls;
            scope.spawn(move || {
                while let Ok(idx) = rx_work.recv() {
                    let result = fetch_bytes(urls[idx], SEGMENT_MAX_BYTES);
                    if tx_done.send((idx, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(tx_done);

        let mut pending: HashMap<usize, Vec<u8>> = HashMap::new();
        let mut next = 0usize;
        while next < total {
            let (idx, result) = rx_done.recv().map_err(|_| Error::Fetch("Download workers stopped unexpectedly".into()))?;
            let body = result?;
            pending.insert(idx, body);
            while let Some(body) = pending.remove(&next) {
                let body = if fragmented { fmp4::strip_segment_index(&body) } else { std::borrow::Cow::Borrowed(body.as_slice()) };
                dest.write_all(&body).map_err(|e| Error::Io(e.to_string()))?;
                written += body.len() as u64;
                next += 1;
                progress(next, total, written);
            }
        }
        Ok(written)
    })
}

/// Turn a visitor-facing address into the one the resolve endpoint accepts. `on.soundcloud.com/<id>`
/// is a redirect to the canonical page; a GET follows it (nullamp-http honours redirects) and we
/// re-send its final URL. A direct address is used as-is after stripping query and fragment.
fn canonical_track_url(url: &str) -> Result<String, Error> {
    match track_url_shape(url)? {
        TrackUrl::Direct(canonical) => Ok(canonical),
        TrackUrl::Short(short) => resolve_short(&short),
    }
}

pub(crate) fn track_url_shape(url: &str) -> Result<TrackUrl, Error> {
    let trimmed = url.trim();
    let no_scheme = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")).ok_or_else(|| Error::Address(trimmed.to_owned()))?;
    let (host, path) = no_scheme.split_once('/').unwrap_or((no_scheme, ""));
    let host = host.strip_prefix("www.").unwrap_or(host);
    match host {
        "on.soundcloud.com" => {
            let code = path.split(['?', '#']).next().unwrap_or("").trim_matches('/');
            if code.is_empty() {
                return Err(Error::Address(trimmed.to_owned()));
            }
            Ok(TrackUrl::Short(format!("https://on.soundcloud.com/{code}")))
        }
        "soundcloud.com" | "m.soundcloud.com" => {
            let path = path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            // A track is /<user>/<slug>, optionally /<user>/<slug>/<secret-token>.
            match parts.as_slice() {
                [user, slug] | [user, slug, _] if !is_reserved(user) && !is_reserved(slug) => Ok(TrackUrl::Direct(format!("https://soundcloud.com/{user}/{slug}"))),
                _ => Err(Error::Address(trimmed.to_owned())),
            }
        }
        _ => Err(Error::Address(trimmed.to_owned())),
    }
}

/// Pages under these top-level paths are never tracks.
fn is_reserved(part: &str) -> bool {
    matches!(part, "discover" | "stream" | "upload" | "search" | "you" | "settings" | "notifications" | "messages" | "pages" | "pro" | "sets" | "tracks" | "albums" | "reposts" | "likes" | "following" | "followers")
}

fn resolve_short(url: &str) -> Result<String, Error> {
    let request = nullamp_http::Request::get(url, USER_AGENT).max_bytes(PLAYLIST_MAX_BYTES).timeout_secs(SEGMENT_TIMEOUT_SECS);
    let response = nullamp_http::send(&request)?;
    if response.status == 404 {
        return Err(Error::NotPublic);
    }
    // The HTML carries the canonical URL in <link rel="canonical" href="..."> and in <meta property="og:url">.
    let html = response.text();
    for marker in ["<link rel=\"canonical\" href=\"", "<meta property=\"og:url\" content=\""] {
        if let Some(i) = html.find(marker) {
            let after = &html[i + marker.len()..];
            if let Some(end) = after.find('"') {
                let canonical = &after[..end];
                if canonical.starts_with("https://soundcloud.com/") {
                    return track_url_shape(canonical).and_then(|shape| match shape {
                        TrackUrl::Direct(u) => Ok(u),
                        TrackUrl::Short(_) => Err(Error::Address(url.to_owned())),
                    });
                }
            }
        }
    }
    Err(Error::Reply("The short link's target could not be read".into()))
}
