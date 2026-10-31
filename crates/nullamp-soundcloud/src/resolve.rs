//! Resolve a track address through `api-v2.soundcloud.com` and pick a transcoding to download.
//!
//! `/resolve` is the one API hop that turns a canonical track URL into the full track object.
//! `media.transcodings[]` lists the formats the uploader's audio was encoded into (progressive MP3 is
//! rare for a long upload; HLS AAC in fMP4 is the usual case for a DJ set).

use serde_json::Value;

use crate::{Error, Track, USER_AGENT};

const RESOLVE_MAX_BYTES: u64 = 2 << 20;
const TRANSCODE_MAX_BYTES: u64 = 32 << 10;
const TIMEOUT_SECS: u32 = 20;

/// The track plus the fields a download needs that are not shown to the caller: the signed-playlist
/// endpoint and the authorization token that opens it.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub track: Track,
    pub transcoding_url: String,
    /// The uploader's per-request token. Opens the signed playlist URL.
    pub track_authorization: String,
    /// Short label used in log lines: `"aac_160k hls"`.
    pub format: String,
}

/// Call `/resolve` and parse the response.
pub fn resolve(track_url: &str, client_id: &str) -> Result<Resolved, Error> {
    let encoded = url_encode(track_url);
    let url = format!("https://api-v2.soundcloud.com/resolve?url={encoded}&client_id={client_id}");
    let request = nullamp_http::Request::get(&url, USER_AGENT).max_bytes(RESOLVE_MAX_BYTES).timeout_secs(TIMEOUT_SECS);
    let response = nullamp_http::send(&request)?;
    match response.status {
        200 => parse(&response.text()),
        401 | 403 => Err(Error::NoClientId),
        404 => Err(Error::NotPublic),
        code => Err(Error::Fetch(format!("SoundCloud answered HTTP {code}"))),
    }
}

/// Parse the body of a `/resolve` response. Separated out so the test suite can run it.
pub fn parse(body: &str) -> Result<Resolved, Error> {
    let json: Value = serde_json::from_str(body).map_err(|e| Error::Reply(format!("Unreadable resolve answer: {e}")))?;
    match json.get("kind").and_then(Value::as_str) {
        Some("track") => {}
        Some(other) => return Err(Error::Reply(format!("The address resolves to a {other}, not a track"))),
        None => return Err(Error::Reply("The resolve answer is not a track".into())),
    }
    let text = |key: &str| json.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let user = json.get("user").and_then(|u| u.get("username")).and_then(Value::as_str).unwrap_or_default().to_owned();
    let track = Track {
        id: json.get("id").and_then(Value::as_u64).ok_or_else(|| Error::Reply("The resolve answer has no id".into()))?,
        title: text("title"),
        artist: user,
        duration_ms: json.get("duration").and_then(Value::as_u64).unwrap_or(0),
        artwork_url: Some(text("artwork_url")).filter(|u| u.starts_with("https://")).map(|u| u.replace("-large.jpg", "-t500x500.jpg")),
        permalink_url: text("permalink_url"),
    };
    let authorization = json.get("track_authorization").and_then(Value::as_str).unwrap_or_default().to_owned();
    if authorization.is_empty() {
        return Err(Error::NoAudio);
    }
    let (transcoding_url, format) = pick_transcoding(&json)?;
    Ok(Resolved { track, transcoding_url, track_authorization: authorization, format })
}

/// Walk `media.transcodings[]` and return the preferred URL. Order: progressive MP3 (one GET, no
/// reassembly) first, then HLS variants by the bitrate their preset name advertises. Master
/// playlists (`mime_type: audio/mpegurl`), snippets and DRM-protected variants are filtered out.
fn pick_transcoding(json: &Value) -> Result<(String, String), Error> {
    let list = json.get("media").and_then(|m| m.get("transcodings")).and_then(Value::as_array).ok_or(Error::NoAudio)?;
    let eligible: Vec<Variant> = list.iter().filter_map(Variant::from).filter(|v| !v.snippet && !v.url.is_empty() && !is_master_playlist(&v.mime) && is_known_protocol(&v.protocol)).collect();
    let best = eligible.into_iter().max_by_key(score).ok_or(Error::NoAudio)?;
    let label = format!("{} {}", best.preset, best.protocol);
    Ok((best.url, label))
}

fn is_master_playlist(mime: &str) -> bool {
    mime == "audio/mpegurl" || mime == "application/vnd.apple.mpegurl"
}

fn is_known_protocol(protocol: &str) -> bool {
    matches!(protocol, "progressive" | "hls")
}

/// Rank an audio variant. A progressive stream always beats HLS (one GET, no muxing). Within a
/// protocol, higher advertised bitrate (`aac_160k` > `aac_96k`) wins. A "hq" quality label adds a
/// small bonus for the uncommon track where SoundCloud publishes an HQ Go+ stream to anonymous.
fn score(v: &Variant) -> i32 {
    let mut s = match v.protocol.as_str() {
        "progressive" => 1000,
        "hls" => 500,
        _ => 0,
    };
    if let Some(kbit) = preset_bitrate(&v.preset) {
        s += kbit as i32;
    }
    if v.quality == "hq" {
        s += 50;
    }
    s
}

/// Pull a kilobit count out of a preset name: `aac_160k` → 160, `aac_96k` → 96, `mp3_1_0` → None.
fn preset_bitrate(preset: &str) -> Option<u32> {
    let tail = preset.rsplit('_').next()?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = digits.parse().ok()?;
    tail.ends_with('k').then_some(n)
}

#[derive(Debug)]
struct Variant {
    url: String,
    preset: String,
    protocol: String,
    mime: String,
    quality: String,
    snippet: bool,
}

impl Variant {
    fn from(v: &Value) -> Option<Self> {
        let format = v.get("format")?;
        Some(Variant {
            url: v.get("url").and_then(Value::as_str).unwrap_or_default().to_owned(),
            preset: v.get("preset").and_then(Value::as_str).unwrap_or_default().to_owned(),
            protocol: format.get("protocol").and_then(Value::as_str).unwrap_or_default().to_owned(),
            mime: format.get("mime_type").and_then(Value::as_str).unwrap_or_default().to_owned(),
            quality: v.get("quality").and_then(Value::as_str).unwrap_or_default().to_owned(),
            snippet: v.get("snipped").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Ask the transcoding endpoint for the signed playlist URL.
pub fn signed_playlist_url(transcoding_url: &str, client_id: &str, track_authorization: &str) -> Result<String, Error> {
    let joiner = if transcoding_url.contains('?') { '&' } else { '?' };
    let authz = url_encode(track_authorization);
    let url = format!("{transcoding_url}{joiner}client_id={client_id}&track_authorization={authz}");
    let request = nullamp_http::Request::get(&url, USER_AGENT).max_bytes(TRANSCODE_MAX_BYTES).timeout_secs(TIMEOUT_SECS);
    let response = nullamp_http::send(&request)?.ok()?;
    parse_signed_url(&response.text())
}

/// Parse the `{"url": "..."}` body the transcoding endpoint returns.
pub fn parse_signed_url(body: &str) -> Result<String, Error> {
    let json: Value = serde_json::from_str(body).map_err(|e| Error::Reply(format!("Unreadable transcoding answer: {e}")))?;
    let url = json.get("url").and_then(Value::as_str).unwrap_or_default();
    if url.starts_with("https://") { Ok(url.to_owned()) } else { Err(Error::Reply("The transcoding answer has no URL".into())) }
}

/// Minimal percent-encoding for the characters a track URL may hold. SoundCloud's resolve endpoint
/// accepts a bare `https://` prefix fine but complains about query-string separators in the value.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b':' => out.push_str("%3A"),
            b'/' => out.push_str("%2F"),
            b'?' => out.push_str("%3F"),
            b'#' => out.push_str("%23"),
            b'&' => out.push_str("%26"),
            b'=' => out.push_str("%3D"),
            b'+' => out.push_str("%2B"),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
