//! Mixcloud: DJ mix identity and artwork. <https://www.mixcloud.com/developers/>
//!
//! The public API still returns a `sections` field, but in practice it is empty for
//! most uploads (tracklists are no longer exposed). When a description contains a
//! timestamped tracklist, it is parsed as a suggestion.

use std::time::Duration;

use nullamp_core::setlist::{MixInfo, Provenance, Setlist, SetlistEntry, Timing, Verification};
use nullamp_core::tracklist_text;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::query::rank_score;
use crate::{Http, MixSource, OnlineError};

const PROVIDER: &str = "Mixcloud";

#[derive(Clone, Debug, PartialEq)]
pub struct MixMatch {
    pub key: String,
    pub url: String,
    pub name: String,
    pub dj: Option<String>,
    pub created: Option<String>,
    pub duration: Option<f64>,
    pub picture: Option<String>,
    /// Ranking score relative to the query (higher is better).
    pub score: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixDetails {
    pub mix: MixInfo,
    pub setlist: Option<Setlist>,
    pub picture: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<Cloudcast>,
}

#[derive(Deserialize)]
struct Cloudcast {
    key: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    created_time: Option<String>,
    #[serde(default)]
    audio_length: Option<f64>,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    pictures: Option<Pictures>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    sections: Vec<Section>,
}

#[derive(Deserialize)]
struct User {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct Pictures {
    #[serde(default, rename = "640wx640h")]
    large: Option<String>,
    #[serde(default)]
    extra_large: Option<String>,
    #[serde(default)]
    medium: Option<String>,
}

#[derive(Deserialize)]
struct Section {
    #[serde(default)]
    start_time: Option<f64>,
    #[serde(default)]
    track: Option<SectionTrack>,
    #[serde(default)]
    chapter: Option<String>,
}

#[derive(Deserialize)]
struct SectionTrack {
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: Option<User>,
}

pub struct Mixcloud;

const HOSTS: &[&str] = &["mixcloud.com", "mxcdn.net"];

fn picture(p: &Option<Pictures>) -> Option<String> {
    p.as_ref().and_then(|p| p.large.clone().or_else(|| p.extra_large.clone()).or_else(|| p.medium.clone()))
}

/// Keys look like `/user/slug/`; reject anything else before building a URL.
fn valid_key(key: &str) -> bool {
    key.starts_with('/')
        && key.ends_with('/')
        && key.len() < 300
        && !key.contains("..")
        && key.chars().all(|c| c.is_ascii_alphanumeric() || "/-_%.".contains(c))
}

fn to_details(c: Cloudcast) -> MixDetails {
    let url = (!c.url.is_empty()).then(|| c.url.clone());
    let provenance = Provenance::Online { provider: PROVIDER.into(), url: url.clone() };
    let mut entries: Vec<SetlistEntry> = c
        .sections
        .iter()
        .filter_map(|s| {
            let (artist, title) = match (&s.track, &s.chapter) {
                (Some(t), _) => (t.artist.as_ref().map(|a| a.name.clone()).unwrap_or_default(), t.name.clone()),
                (None, Some(ch)) => (String::new(), ch.clone()),
                _ => return None,
            };
            let mut e = SetlistEntry::new(artist, title, s.start_time, provenance.clone());
            e.timing = Timing::Approximate;
            e.verification = Verification::Suggested;
            e.source_url = url.clone();
            Some(e)
        })
        .collect();
    if entries.is_empty()
        && let Some(desc) = &c.description
    {
        let parsed = tracklist_text::parse(desc, url.clone());
        let timed = parsed.setlist.entries.iter().filter(|e| e.start.is_some()).count();
        if parsed.setlist.entries.len() >= 3 && timed >= 2 {
            entries = parsed
                .setlist
                .entries
                .into_iter()
                .map(|mut e| {
                    e.provenance = Provenance::Online { provider: format!("{PROVIDER} (description)"), url: url.clone() };
                    e
                })
                .collect();
        }
    }
    let mix = MixInfo {
        title: Some(c.name.clone()).filter(|s| !s.is_empty()),
        dj: c.user.as_ref().map(|u| u.name.clone()).filter(|s| !s.is_empty()),
        event: None,
        date: c.created_time.as_deref().map(|t| t.chars().take(10).collect()),
        url: url.clone(),
    };
    MixDetails {
        setlist: (!entries.is_empty()).then(|| Setlist { mix: mix.clone(), entries }),
        picture: picture(&c.pictures),
        description: c.description.filter(|d| !d.trim().is_empty()),
        mix,
    }
}

impl Mixcloud {
    /// Search and rank by similarity to `query` and (optionally) the file duration.
    pub fn search_ranked(&self, http: &Http, query: &str, duration: Option<f64>) -> Result<Vec<MixMatch>, OnlineError> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("https://api.mixcloud.com/search/?q={}&type=cloudcast&limit=20", utf8_percent_encode(q, NON_ALPHANUMERIC));
        let resp: SearchResponse = http.get_json(PROVIDER, &url, Duration::from_secs(3 * 86_400), &[])?;
        let mut out: Vec<MixMatch> = resp
            .data
            .into_iter()
            .filter(|c| valid_key(&c.key))
            .map(|c| MixMatch {
                score: rank_score(q, &format!("{} {}", c.user.as_ref().map(|u| u.name.as_str()).unwrap_or(""), c.name), duration, c.audio_length),
                picture: picture(&c.pictures),
                dj: c.user.map(|u| u.name).filter(|s| !s.is_empty()),
                created: c.created_time,
                duration: c.audio_length,
                key: c.key,
                url: c.url,
                name: c.name,
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(out)
    }
}

impl MixSource for Mixcloud {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    fn search(&self, http: &Http, query: &str) -> Result<Vec<MixMatch>, OnlineError> {
        self.search_ranked(http, query, None)
    }

    fn details(&self, http: &Http, m: &MixMatch) -> Result<MixDetails, OnlineError> {
        if !valid_key(&m.key) {
            return Err(OnlineError::Parse("invalid Mixcloud key".into()));
        }
        let c: Cloudcast = http.get_json(PROVIDER, &format!("https://api.mixcloud.com{}", m.key), Duration::from_secs(7 * 86_400), &[])?;
        Ok(to_details(c))
    }

    fn allowed_hosts(&self) -> &'static [&'static str] {
        HOSTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_validation() {
        assert!(valid_key("/user/my-mix-2019/"));
        assert!(valid_key("/anjunadeep/the-anjunadeep-edition-208-with-ben-b%C3%B6hmer/"));
        assert!(!valid_key("/../etc/"));
        assert!(!valid_key("https://evil/"));
        assert!(!valid_key("/a b/"));
    }

    #[test]
    fn sections_become_setlist() {
        let json = r#"{"key":"/u/m/","url":"https://www.mixcloud.com/u/m/","name":"Mix","user":{"name":"DJ"},
            "created_time":"2019-08-07T10:00:00Z","sections":[
              {"start_time":0,"track":{"name":"One","artist":{"name":"A"}},"section_type":"track"},
              {"start_time":416,"track":{"name":"Two","artist":{"name":"B"}},"section_type":"track"}]}"#;
        let d = to_details(serde_json::from_str(json).unwrap());
        let s = d.setlist.unwrap();
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[1].start, Some(416.0));
        assert_eq!(s.entries[1].artist, "B");
        assert_eq!(d.mix.dj.as_deref(), Some("DJ"));
        assert_eq!(d.mix.date.as_deref(), Some("2019-08-07"));
    }

    #[test]
    fn description_tracklist_fallback() {
        let json = r#"{"key":"/u/m/","url":"https://www.mixcloud.com/u/m/","name":"Mix","sections":[],
            "description":"Thanks for listening!\n00:00 A - One\n05:10 B - Two (Remix)\n11:00 C - Three"}"#;
        let d = to_details(serde_json::from_str(json).unwrap());
        let s = d.setlist.unwrap();
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[1].version.as_deref(), Some("Remix"));
        assert!(matches!(&s.entries[0].provenance, Provenance::Online { provider, .. } if provider.contains("description")));

        let json = r#"{"key":"/u/m/","name":"Mix","description":"Just a promo text. Follow me!"}"#;
        assert!(to_details(serde_json::from_str(json).unwrap()).setlist.is_none());
    }
}
