//! Reads the tracks of a public Spotify playlist, anonymously.
//!
//! Spotify's Web API wants a login or a developer key, but the page it serves for embedding a playlist
//! (`open.spotify.com/embed/playlist/ID`) is public and carries the playlist as JSON: its name and, for
//! each track, the title, the artists and the length. That is what a browser gets, so that is what is
//! read here; no token is used and nothing is made up. The page lists the first [`PAGE`] tracks of a
//! playlist and no more, so a longer one is cut and [`Playlist::cut`] says so. A private playlist and
//! one that does not exist look the same to an outsider. The page layout is not documented and can
//! change, in which case [`parse`] fails with [`Error::Reply`].
//!
//! The page is fetched by `nullamp-http` (`curl`, or ureq on Android). [`fetch`] blocks for up to
//! [`TIMEOUT_SECS`] seconds: call it from a worker thread.

use serde_json::Value;

/// How long the request may take.
pub const TIMEOUT_SECS: u32 = 20;
/// The embed page of a playlist is about 100 KB; anything much longer is not one.
const MAX_BYTES: u64 = 4 << 20;
/// How many tracks the embed page lists at most.
pub const PAGE: usize = 100;
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// The text is not a link to a Spotify playlist.
    #[error("\"{0}\" is not a link to a Spotify playlist")]
    Address(String),
    /// No network, a server error, a timeout. Carries curl's message.
    #[error("{0}")]
    Fetch(String),
    /// Spotify has no such playlist for an anonymous visitor: it is private, or the link is wrong.
    #[error("Spotify has no public playlist with this link; it may be private")]
    NotPublic,
    /// The answer is not the page that was asked for.
    #[error("{0}")]
    Reply(String),
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

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// The artists, "A, B".
    pub artist: String,
    pub title: String,
    /// In seconds.
    pub duration: Option<f64>,
    /// `spotify:track:ID`.
    pub uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub name: String,
    pub id: String,
    pub tracks: Vec<Track>,
    /// The page was full ([`PAGE`] tracks), so the playlist may have more than it shows.
    pub cut: bool,
}

/// The id in a playlist link: `https://open.spotify.com/playlist/ID?si=...` (also with a language
/// part such as `/intl-de/` or `/embed/`), `spotify:playlist:ID`, or the bare id.
pub fn playlist_id(text: &str) -> Option<String> {
    let text = text.trim();
    let id = match text.strip_prefix("spotify:playlist:") {
        Some(id) => id,
        None if text.contains('/') => {
            let rest = text.strip_prefix("https://")?;
            let (host, path) = rest.split_once('/')?;
            if host != "open.spotify.com" {
                return None;
            }
            let path = path.split(['?', '#']).next()?;
            let mut parts = path.split('/');
            parts.find(|p| *p == "playlist")?;
            parts.next()?
        }
        None => text,
    };
    (id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_owned())
}

/// Reads the playlist `text` links to.
pub fn fetch(text: &str) -> Result<Playlist, Error> {
    let id = playlist_id(text).ok_or_else(|| Error::Address(text.trim().to_owned()))?;
    let url = format!("https://open.spotify.com/embed/playlist/{id}");
    let mut request = nullamp_http::Request::get(&url, USER_AGENT);
    request.max_bytes = MAX_BYTES;
    request.timeout_secs = TIMEOUT_SECS;
    let response = nullamp_http::send(&request)?;
    if response.status == 404 {
        return Err(Error::NotPublic);
    }
    parse(&response.ok()?.text())
}

/// The playlist in an embed page. Episodes of podcasts are left out.
pub fn parse(html: &str) -> Result<Playlist, Error> {
    const OPEN: &str = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html.find(OPEN).map(|i| i + OPEN.len()).ok_or_else(|| Error::Reply("The page holds no playlist data".into()))?;
    let json = &html[start..start + html[start..].find("</script>").ok_or_else(|| Error::Reply("The page holds no playlist data".into()))?];
    let data: Value = serde_json::from_str(json).map_err(|e| Error::Reply(format!("The playlist data is not readable: {e}")))?;
    let page = &data["props"]["pageProps"];
    if page["status"].as_u64() == Some(404) {
        return Err(Error::NotPublic);
    }
    let entity = &page["state"]["data"]["entity"];
    let list = entity["trackList"].as_array().ok_or_else(|| Error::Reply("The page lists no tracks".into()))?;
    let text = |v: &Value| v.as_str().map(clean).filter(|s| !s.is_empty());
    let tracks: Vec<Track> = list
        .iter()
        .filter(|t| t["entityType"].as_str().is_none_or(|k| k == "track"))
        .filter_map(|t| {
            Some(Track {
                title: text(&t["title"])?,
                artist: text(&t["subtitle"]).unwrap_or_default(),
                duration: t["duration"].as_f64().filter(|ms| *ms > 0.0).map(|ms| (ms / 1000.0).round()),
                uri: t["uri"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect();
    Ok(Playlist {
        name: text(&entity["name"]).or_else(|| text(&entity["title"])).unwrap_or_else(|| "Spotify playlist".into()),
        id: entity["id"].as_str().unwrap_or_default().to_owned(),
        cut: list.len() >= PAGE,
        tracks,
    })
}

/// Spotify joins artists with a comma and a no-break space.
fn clean(s: &str) -> String {
    s.replace('\u{a0}', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
