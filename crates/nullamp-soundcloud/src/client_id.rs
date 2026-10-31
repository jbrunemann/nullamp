//! Finding and remembering the web player's `client_id`.
//!
//! The id is a long alphanumeric string embedded in one of the JS bundles the player loads. The
//! bundle that holds it is the one that bootstraps the authenticated-session API client; the others
//! do not mention it. The id itself rotates between site deploys (SoundCloud runs several at once
//! and any one works), so the file cache has a short lifetime.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, USER_AGENT};

const BOOTSTRAP_URL: &str = "https://soundcloud.com/discover";
const BUNDLE_PREFIX: &str = "https://a-v2.sndcdn.com/assets/";
const BUNDLE_MAX_BYTES: u64 = 8 << 20;
const BOOTSTRAP_MAX_BYTES: u64 = 2 << 20;
const TIMEOUT_SECS: u32 = 20;
/// How long a cached id is trusted. One day is well short of the quiet rotation cadence we see.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Return a valid `client_id`, reading `<cache_dir>/soundcloud/client_id.txt` first if it exists and
/// is recent enough. If `cache_dir` is `None`, every call scrapes.
pub fn fetch(cache_dir: Option<&Path>) -> Result<String, Error> {
    if let Some(dir) = cache_dir {
        if let Some(id) = read_cache(&cache_path(dir)) {
            return Ok(id);
        }
    }
    let id = scrape()?;
    if let Some(dir) = cache_dir {
        let _ = write_cache(&cache_path(dir), &id);
    }
    Ok(id)
}

/// `true` if the cache file holds a valid, recent id. [`fetch`] uses this to decide whether a
/// progress tick about scraping is needed.
pub fn cache_fresh(cache_dir: &Path) -> bool {
    read_cache(&cache_path(cache_dir)).is_some()
}

fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("soundcloud").join("client_id.txt")
}

fn read_cache(path: &Path) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let mut lines = body.lines();
    let stamp: u64 = lines.next()?.parse().ok()?;
    let id = lines.next()?.trim().to_owned();
    if !is_valid_id(&id) {
        return None;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if now.saturating_sub(stamp) > CACHE_TTL.as_secs() {
        return None;
    }
    Some(id)
}

fn write_cache(path: &Path, id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    fs::write(path, format!("{now}\n{id}\n"))
}

/// Fetch the player homepage, enumerate the bundle URLs it references, and scan each until one
/// yields a `client_id`. The hit is usually in a mid- or high-numbered bundle, so the order is
/// descending by filename.
pub fn scrape() -> Result<String, Error> {
    let home = home_page()?;
    let mut bundles = bundle_urls(&home);
    bundles.sort_by(|a, b| b.cmp(a));
    for url in bundles {
        let bundle = match fetch_bundle(&url) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Some(id) = extract_id(&bundle) {
            return Ok(id);
        }
    }
    Err(Error::NoClientId)
}

fn home_page() -> Result<String, Error> {
    let request = nullamp_http::Request::get(BOOTSTRAP_URL, USER_AGENT).max_bytes(BOOTSTRAP_MAX_BYTES).timeout_secs(TIMEOUT_SECS);
    let response = nullamp_http::send(&request)?.ok()?;
    Ok(response.text())
}

fn fetch_bundle(url: &str) -> Result<String, Error> {
    let request = nullamp_http::Request::get(url, USER_AGENT).max_bytes(BUNDLE_MAX_BYTES).timeout_secs(TIMEOUT_SECS);
    let response = nullamp_http::send(&request)?.ok()?;
    Ok(response.text())
}

/// Collect every `<script src="https://a-v2.sndcdn.com/assets/*.js">` in the homepage.
pub fn bundle_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(BUNDLE_PREFIX) {
        rest = &rest[i..];
        // The URL ends at the first character that cannot be in one.
        let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '>' | '<')).unwrap_or(rest.len());
        let url = &rest[..end];
        if url.ends_with(".js") && !urls.iter().any(|u: &String| u == url) {
            urls.push(url.to_owned());
        }
        rest = &rest[end..];
    }
    urls
}

/// Find a `client_id:"..."` literal in a JS bundle. Also accepts `client_id="..."` and
/// `"client_id":"..."` in case the minifier rearranges things.
pub fn extract_id(bundle: &str) -> Option<String> {
    for marker in ["client_id:\"", "client_id=\"", "\"client_id\":\""] {
        let mut rest = bundle;
        while let Some(i) = rest.find(marker) {
            let after = &rest[i + marker.len()..];
            let end = after.find('"')?;
            let candidate = &after[..end];
            if is_valid_id(candidate) {
                return Some(candidate.to_owned());
            }
            rest = &after[end..];
        }
    }
    None
}

fn is_valid_id(id: &str) -> bool {
    id.len() >= 20 && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}
