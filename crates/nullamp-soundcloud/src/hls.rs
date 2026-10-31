//! Minimal reader of the m3u8 variant SoundCloud sends: `#EXT-X-MAP:URI="init.mp4"` on top, then
//! `#EXTINF:<secs>,` lines each followed by a segment URL on the next non-tag line. No encryption,
//! no byte ranges, no multi-variant.

use crate::Error;

/// What a playlist carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    /// `#EXT-X-MAP:URI="..."`. For a progressive-MP3 playlist this is absent, but a progressive URL
    /// never routes through this parser (it is downloaded whole).
    pub init: Option<String>,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub duration_secs: f64,
    pub url: String,
}

/// Parse an m3u8. `Err(Error::Reply)` for a body that lacks a `#EXTM3U` header or has no segments.
pub fn parse(body: &str) -> Result<Playlist, Error> {
    let body = body.trim_start_matches('\u{feff}');
    let mut lines = body.lines().map(str::trim);
    let first = lines.next().unwrap_or_default();
    if !first.starts_with("#EXTM3U") {
        return Err(Error::Reply("The playlist does not start with #EXTM3U".into()));
    }
    let mut init = None;
    let mut segments = Vec::new();
    let mut pending_duration: Option<f64> = None;
    for line in body.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            init = parse_uri_attr(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending_duration = rest.split(',').next().and_then(|s| s.trim().parse::<f64>().ok());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let duration = pending_duration.take().ok_or_else(|| Error::Reply("A segment URL has no #EXTINF before it".into()))?;
        segments.push(Segment { duration_secs: duration, url: line.to_owned() });
    }
    if segments.is_empty() {
        return Err(Error::Reply("The playlist has no segments".into()));
    }
    Ok(Playlist { init, segments })
}

/// Pull the `URI="..."` out of an attribute list like `URI="https://.../init.mp4",BYTERANGE=...`.
fn parse_uri_attr(rest: &str) -> Option<String> {
    let i = rest.find("URI=\"")? + "URI=\"".len();
    let after = &rest[i..];
    let end = after.find('"')?;
    Some(after[..end].to_owned())
}

impl Playlist {
    /// Rough playback length, used for progress bars.
    pub fn duration_secs(&self) -> f64 {
        self.segments.iter().map(|s| s.duration_secs).sum()
    }
}
