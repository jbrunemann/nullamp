//! Online metadata lookups.
//!
//! Everything here is optional: the player works fully offline, and every request
//! runs on a background worker, never on the playback or UI path.
//!
//! Providers sit behind small traits ([`RecordingSource`], [`ArtworkSource`],
//! [`MixSource`]) so a service can be replaced without touching the player. Current
//! implementations:
//!
//! | Provider          | Used for                         | Access                         |
//! |-------------------|----------------------------------|--------------------------------|
//! | MusicBrainz       | recording / release metadata     | public API, 1 req/s, UA needed |
//! | Cover Art Archive | release artwork                  | public API                     |
//! | Mixcloud          | DJ mix identity, artwork         | public read API                |
//!
//! 1001Tracklists has no public API and its site restricts automated access, so it
//! is not queried. Users can paste a tracklist copied from its pages instead
//! (see `nullamp_core::tracklist_text`), and the page URL is kept as provenance.
//!
//! All responses are untrusted: sizes are capped, JSON is parsed into narrow
//! structures, and images are only fetched from known hosts over HTTPS.

pub mod coverart;
pub mod mixcloud;
pub mod musicbrainz;
pub mod query;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

pub use coverart::{CoverArtArchive, CoverImage};
pub use mixcloud::{MixDetails, MixMatch, Mixcloud};
pub use musicbrainz::{MusicBrainz, RecordingMatch, ReleaseRef};

/// Contact information sent in the User-Agent, as MusicBrainz asks of API clients.
/// Set `NULLAMP_CONTACT` (an e-mail address or project URL) when building releases.
pub const CONTACT: &str = match option_env!("NULLAMP_CONTACT") {
    Some(c) => c,
    None => "contact not configured",
};

pub fn user_agent() -> String {
    format!("Nullamp/{} ( {CONTACT} )", env!("CARGO_PKG_VERSION"))
}

const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 15 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OnlineError {
    #[error("network unavailable: {0}")]
    Network(String),
    #[error("{service} returned HTTP {status}")]
    Http { service: &'static str, status: u16 },
    #[error("{0} is rate limiting requests; try again shortly")]
    RateLimited(&'static str),
    #[error("unexpected response: {0}")]
    Parse(String),
    #[error("response too large")]
    TooLarge,
    #[error("refusing to fetch from {0}")]
    DisallowedHost(String),
}

/// Shared HTTP client with per-host rate limiting and an on-disk response cache.
pub struct Http {
    agent: ureq::Agent,
    next_slot: Mutex<HashMap<String, Instant>>,
    cache: Option<DiskCache>,
}

impl Http {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(user_agent())
            .https_only(true)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .build();
        Http { agent: config.into(), next_slot: Mutex::new(HashMap::new()), cache: cache_dir.map(DiskCache::new) }
    }

    /// Block until a request to `host` is allowed (at most one per `interval`).
    fn throttle(&self, host: &str, interval: Duration) {
        let wait = {
            let mut slots = self.next_slot.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let slot = slots.get(host).copied().unwrap_or(now).max(now);
            slots.insert(host.to_owned(), slot + interval);
            slot - now
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// GET JSON with caching. `ttl` bounds how long a cached response is reused.
    /// Redirects are followed only to `allowed_hosts` (empty: same host only).
    pub(crate) fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        service: &'static str,
        url: &str,
        ttl: Duration,
        allowed_hosts: &[&str],
    ) -> Result<T, OnlineError> {
        if let Some(bytes) = self.cache.as_ref().and_then(|c| c.get(url, ttl)) {
            if let Ok(v) = serde_json::from_slice(&bytes) {
                return Ok(v);
            }
        }
        let bytes = self.get(service, url, MAX_JSON_BYTES, allowed_hosts)?;
        let value = serde_json::from_slice(&bytes).map_err(|e| OnlineError::Parse(e.to_string()))?;
        if let Some(c) = &self.cache {
            c.put(url, &bytes);
        }
        Ok(value)
    }

    /// Download an image from an allowed host (with caching).
    pub fn get_image(&self, url: &str, allowed_hosts: &[&str]) -> Result<Vec<u8>, OnlineError> {
        if let Some(bytes) = self.cache.as_ref().and_then(|c| c.get(url, Duration::from_secs(90 * 86_400))) {
            return Ok(bytes);
        }
        let bytes = self.get("image host", url, MAX_IMAGE_BYTES, allowed_hosts)?;
        if !looks_like_image(&bytes) {
            return Err(OnlineError::Parse("not an image".into()));
        }
        if let Some(c) = &self.cache {
            c.put(url, &bytes);
        }
        Ok(bytes)
    }

    /// GET with manual redirect handling so every hop can be checked.
    /// An empty `allowed_hosts` means "same host as the original URL only".
    fn get(&self, service: &'static str, url: &str, limit: u64, allowed_hosts: &[&str]) -> Result<Vec<u8>, OnlineError> {
        let first_host = host_of(url).ok_or_else(|| OnlineError::DisallowedHost(url.into()))?;
        let mut current = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            let host = host_of(&current).ok_or_else(|| OnlineError::DisallowedHost(current.clone()))?;
            let allowed = if allowed_hosts.is_empty() { host == first_host } else { host_allowed(&host, allowed_hosts) };
            if !allowed {
                return Err(OnlineError::DisallowedHost(host));
            }
            self.throttle(&host, Duration::from_millis(1100));
            let mut resp = self.agent.get(&current).header("Accept", "application/json, image/*").call().map_err(|e| {
                OnlineError::Network(e.to_string())
            })?;
            let status = resp.status().as_u16();
            match status {
                200 => {
                    let mut reader = resp.body_mut().as_reader().take(limit + 1);
                    let mut buf = Vec::new();
                    reader.read_to_end(&mut buf).map_err(|e| OnlineError::Network(e.to_string()))?;
                    if buf.len() as u64 > limit {
                        return Err(OnlineError::TooLarge);
                    }
                    return Ok(buf);
                }
                301 | 302 | 303 | 307 | 308 => {
                    let location = resp
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| OnlineError::Parse("redirect without location".into()))?;
                    current = resolve_redirect(&current, location);
                }
                429 | 503 => return Err(OnlineError::RateLimited(service)),
                _ => return Err(OnlineError::Http { service, status }),
            }
        }
        Err(OnlineError::Parse("too many redirects".into()))
    }
}

pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn host_allowed(host: &str, allowed: &[&str]) -> bool {
    allowed.iter().any(|a| host == *a || host.ends_with(&format!(".{a}")))
}

fn resolve_redirect(base: &str, location: &str) -> String {
    if location.starts_with("https://") || location.starts_with("http://") {
        location.to_owned()
    } else if let Some(path) = location.strip_prefix('/') {
        let origin_end = base["https://".len()..].find('/').map(|i| i + "https://".len()).unwrap_or(base.len());
        format!("{}/{}", &base[..origin_end], path)
    } else {
        let dir_end = base.rfind('/').unwrap_or(base.len());
        format!("{}/{}", &base[..dir_end], location)
    }
}

fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"\x89PNG\r\n\x1a\n")
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

/// Responses stored as files named by a stable hash of the URL.
struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    fn new(dir: PathBuf) -> Self {
        DiskCache { dir }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}", fnv1a(key.as_bytes())))
    }

    fn get(&self, key: &str, ttl: Duration) -> Option<Vec<u8>> {
        let p = self.path(key);
        let age = std::fs::metadata(&p).ok()?.modified().ok()?.elapsed().unwrap_or_default();
        if age > ttl {
            return None;
        }
        std::fs::read(p).ok()
    }

    fn put(&self, key: &str, bytes: &[u8]) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let p = self.path(key);
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// Remove cache files older than `max_age`. Returns the number removed.
pub fn prune_cache(dir: &Path, max_age: Duration) -> usize {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    read.filter_map(|e| e.ok())
        .filter(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// 64-bit FNV-1a: a stable hash for cache keys (std's hasher is not guaranteed stable).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3))
}

/// A recording-metadata service.
pub trait RecordingSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn search(&self, http: &Http, artist: &str, title: &str) -> Result<Vec<RecordingMatch>, OnlineError>;
}

/// An artwork service keyed by release.
pub trait ArtworkSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn images_for_release(&self, http: &Http, release_id: &str) -> Result<Vec<CoverImage>, OnlineError>;
    fn allowed_hosts(&self) -> &'static [&'static str];
}

/// A DJ-mix service (identity, artwork, and tracklists where available).
pub trait MixSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn search(&self, http: &Http, query: &str) -> Result<Vec<MixMatch>, OnlineError>;
    fn details(&self, http: &Http, m: &MixMatch) -> Result<MixDetails, OnlineError>;
    fn allowed_hosts(&self) -> &'static [&'static str];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_redirects() {
        assert_eq!(host_of("https://CoverArtArchive.org/release/x"), Some("coverartarchive.org".into()));
        assert_eq!(host_of("https://user@host.example:443/p"), Some("host.example".into()));
        assert_eq!(host_of("http://insecure.example/"), None, "https only");
        assert!(host_allowed("ia800.us.archive.org", &["archive.org"]));
        assert!(!host_allowed("evilarchive.org", &["archive.org"]));
        assert_eq!(resolve_redirect("https://a.example/x/y", "/z"), "https://a.example/z");
        assert_eq!(resolve_redirect("https://a.example/x/y", "w"), "https://a.example/x/w");
        assert_eq!(resolve_redirect("https://a.example/x", "https://b.example/q"), "https://b.example/q");
    }

    #[test]
    fn image_sniffing() {
        assert!(looks_like_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(looks_like_image(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!looks_like_image(b"<html>"));
    }

    #[test]
    fn cache_round_trip_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path().to_path_buf());
        c.put("https://x/y", b"hello");
        assert_eq!(c.get("https://x/y", Duration::from_secs(60)).as_deref(), Some(&b"hello"[..]));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.get("https://x/y", Duration::from_millis(1)), None);
        assert_eq!(prune_cache(dir.path(), Duration::from_millis(1)), 1);
    }

    #[test]
    fn throttle_spaces_requests() {
        let http = Http::new(None);
        let t = Instant::now();
        http.throttle("h", Duration::from_millis(50));
        http.throttle("h", Duration::from_millis(50));
        http.throttle("other", Duration::from_millis(50));
        let elapsed = t.elapsed();
        assert!(elapsed >= Duration::from_millis(45) && elapsed < Duration::from_millis(150), "{elapsed:?}");
    }
}
