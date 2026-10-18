//! MusicBrainz recording search. <https://musicbrainz.org/doc/MusicBrainz_API>

use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::{Http, OnlineError, RecordingSource};

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseRef {
    pub id: String,
    pub title: String,
    pub date: Option<String>,
    /// Release group primary type, e.g. "Album", "Single".
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingMatch {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub length: Option<f64>,
    pub disambiguation: Option<String>,
    /// MusicBrainz search score, 0..=100.
    pub score: u8,
    pub releases: Vec<ReleaseRef>,
    pub url: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    recordings: Vec<Recording>,
}

#[derive(Deserialize)]
struct Recording {
    id: String,
    #[serde(default)]
    score: Option<u8>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    length: Option<u64>,
    #[serde(default)]
    disambiguation: Option<String>,
    #[serde(default, rename = "artist-credit")]
    artist_credit: Vec<Credit>,
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Credit {
    #[serde(default)]
    name: String,
    #[serde(default)]
    joinphrase: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default, rename = "release-group")]
    release_group: Option<ReleaseGroup>,
}

#[derive(Deserialize)]
struct ReleaseGroup {
    #[serde(default, rename = "primary-type")]
    primary_type: Option<String>,
}

pub struct MusicBrainz;

/// Escape a value for use inside a quoted Lucene phrase.
fn phrase(s: &str) -> String {
    let escaped: String = s.chars().flat_map(|c| if c == '"' || c == '\\' { vec!['\\', c] } else { vec![c] }).collect();
    format!("\"{escaped}\"")
}

pub(crate) fn search_url(artist: &str, title: &str) -> String {
    let mut q = format!("recording:{}", phrase(title.trim()));
    if !artist.trim().is_empty() {
        q.push_str(&format!(" AND artist:{}", phrase(artist.trim())));
    }
    format!("https://musicbrainz.org/ws/2/recording/?query={}&fmt=json&limit=5", utf8_percent_encode(&q, NON_ALPHANUMERIC))
}

fn convert(r: Recording) -> RecordingMatch {
    let artist = r.artist_credit.iter().map(|c| format!("{}{}", c.name, c.joinphrase.as_deref().unwrap_or(""))).collect::<String>();
    RecordingMatch {
        url: format!("https://musicbrainz.org/recording/{}", r.id),
        id: r.id,
        title: r.title,
        artist: artist.trim().to_owned(),
        length: r.length.map(|ms| ms as f64 / 1000.0),
        disambiguation: r.disambiguation.filter(|d| !d.is_empty()),
        score: r.score.unwrap_or(0),
        releases: r
            .releases
            .into_iter()
            .map(|rel| ReleaseRef {
                id: rel.id,
                title: rel.title,
                date: rel.date.filter(|d| !d.is_empty()),
                kind: rel.release_group.and_then(|g| g.primary_type),
            })
            .collect(),
    }
}

impl RecordingSource for MusicBrainz {
    fn name(&self) -> &'static str {
        "MusicBrainz"
    }

    fn search(&self, http: &Http, artist: &str, title: &str) -> Result<Vec<RecordingMatch>, OnlineError> {
        if title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let resp: SearchResponse = http.get_json("MusicBrainz", &search_url(artist, title), Duration::from_secs(14 * 86_400), &[])?;
        let mut out: Vec<RecordingMatch> = resp.recordings.into_iter().map(convert).collect();
        // Prefer studio releases over DJ-mix compilations that merely contain the track.
        for m in &mut out {
            m.releases.sort_by_key(|r| match r.kind.as_deref() {
                Some("Album") => 0,
                Some("Single") | Some("EP") => 1,
                _ => 2,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_escaped_query() {
        let url = search_url("Above & Beyond", "Sun \"&\" Moon");
        assert!(url.starts_with("https://musicbrainz.org/ws/2/recording/?query=recording%3A%22Sun%20%5C%22%26%5C%22%20Moon%22%20AND%20artist%3A%22Above%20%26%20Beyond%22"));
    }

    #[test]
    fn parses_response() {
        let json = r#"{"recordings":[{"id":"r1","score":100,"title":"Strobe","length":267000,
            "artist-credit":[{"name":"deadmau5","joinphrase":" feat. "},{"name":"Guest"}],
            "releases":[{"id":"a","title":"Mix","release-group":{"primary-type":"Other"}},
                        {"id":"b","title":"For Lack of a Better Name","date":"2009-09-22","release-group":{"primary-type":"Album"}}],
            "unexpected":{"nested":[1,2,3]}}]}"#;
        let r: SearchResponse = serde_json::from_str(json).unwrap();
        let m = convert(r.recordings.into_iter().next().unwrap());
        assert_eq!(m.artist, "deadmau5 feat. Guest");
        assert_eq!(m.length, Some(267.0));
        assert_eq!(m.releases.len(), 2);
        assert_eq!(m.url, "https://musicbrainz.org/recording/r1");
    }
}
