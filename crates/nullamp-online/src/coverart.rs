//! Cover Art Archive. <https://musicbrainz.org/doc/Cover_Art_Archive/API>

use std::time::Duration;

use serde::Deserialize;

use crate::{ArtworkSource, Http, OnlineError};

#[derive(Clone, Debug, PartialEq)]
pub struct CoverImage {
    pub full: String,
    pub thumbnail: String,
    pub front: bool,
    pub types: Vec<String>,
    /// Page to credit and link to.
    pub source_page: String,
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    release: String,
}

#[derive(Deserialize)]
struct Image {
    image: String,
    #[serde(default)]
    front: bool,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    thumbnails: Thumbs,
}

#[derive(Deserialize, Default)]
struct Thumbs {
    #[serde(rename = "250")]
    small: Option<String>,
    #[serde(rename = "500")]
    medium: Option<String>,
}

pub struct CoverArtArchive;

/// Cover Art Archive serves files from archive.org after a redirect.
const HOSTS: &[&str] = &["coverartarchive.org", "archive.org"];

fn https(url: &str) -> String {
    url.strip_prefix("http://").map(|r| format!("https://{r}")).unwrap_or_else(|| url.to_owned())
}

fn convert(listing: Listing) -> Vec<CoverImage> {
    let page = https(&listing.release);
    let mut images: Vec<CoverImage> = listing
        .images
        .into_iter()
        .map(|i| CoverImage {
            full: https(i.thumbnails.medium.as_deref().unwrap_or(&i.image)),
            thumbnail: https(i.thumbnails.small.as_deref().unwrap_or(&i.image)),
            front: i.front,
            types: i.types,
            source_page: page.clone(),
        })
        .collect();
    images.sort_by_key(|i| !i.front);
    images
}

impl ArtworkSource for CoverArtArchive {
    fn name(&self) -> &'static str {
        "Cover Art Archive"
    }

    fn images_for_release(&self, http: &Http, release_id: &str) -> Result<Vec<CoverImage>, OnlineError> {
        if !release_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(OnlineError::Parse("invalid release id".into()));
        }
        let url = format!("https://coverartarchive.org/release/{release_id}");
        match http.get_json::<Listing>("Cover Art Archive", &url, Duration::from_secs(30 * 86_400), HOSTS) {
            Ok(listing) => Ok(convert(listing)),
            // 404: the release simply has no artwork.
            Err(OnlineError::Http { status: 404, .. }) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn allowed_hosts(&self) -> &'static [&'static str] {
        HOSTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listing_front_first() {
        let json = r#"{"release":"http://musicbrainz.org/release/x","images":[
            {"image":"http://coverartarchive.org/release/x/2.jpg","front":false,"types":["Back"],"thumbnails":{}},
            {"image":"http://coverartarchive.org/release/x/1.png","front":true,"types":["Front"],
             "thumbnails":{"250":"http://coverartarchive.org/release/x/1-250.jpg","500":"https://coverartarchive.org/release/x/1-500.jpg"}}]}"#;
        let images = convert(serde_json::from_str(json).unwrap());
        assert!(images[0].front);
        assert_eq!(images[0].thumbnail, "https://coverartarchive.org/release/x/1-250.jpg");
        assert_eq!(images[0].full, "https://coverartarchive.org/release/x/1-500.jpg");
        assert_eq!(images[1].full, "https://coverartarchive.org/release/x/2.jpg");
        assert_eq!(images[0].source_page, "https://musicbrainz.org/release/x");
    }
}
