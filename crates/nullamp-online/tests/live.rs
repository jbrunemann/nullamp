//! Live checks against the real services. Not run by default (network, rate limits):
//! cargo test -p nullamp-online --test live -- --ignored

use nullamp_online::*;

#[test]
#[ignore]
fn musicbrainz_and_cover_art() {
    let http = Http::new(None);
    let found = MusicBrainz.search(&http, "deadmau5", "Strobe").expect("search");
    assert!(!found.is_empty());
    let rec = found.iter().find(|r| r.artist.to_lowercase().contains("deadmau5")).expect("artist match");
    println!("{} - {} ({:?}) releases: {}", rec.artist, rec.title, rec.length, rec.releases.len());
    let mut images = Vec::new();
    for rel in rec.releases.iter().take(3) {
        images = CoverArtArchive.images_for_release(&http, &rel.id).expect("cover art listing");
        if !images.is_empty() {
            break;
        }
    }
    let img = images.first().expect("some release has artwork");
    let bytes = http.get_image(&img.thumbnail, CoverArtArchive.allowed_hosts()).expect("thumbnail");
    println!("thumbnail {} bytes from {}", bytes.len(), img.thumbnail);
    assert!(bytes.len() > 1000);
}

#[test]
#[ignore]
fn mixcloud_search_and_details() {
    let http = Http::new(None);
    let found = Mixcloud.search_ranked(&http, "Solomun Boiler Room Tulum", Some(7286.0)).expect("search");
    let top = found.first().expect("results");
    println!("top: {} by {:?} score {:.2}", top.name, top.dj, top.score);
    let d = Mixcloud.details(&http, top).expect("details");
    println!("details: {:?} picture {:?} setlist entries {}", d.mix, d.picture, d.setlist.map(|s| s.entries.len()).unwrap_or(0));
    if let Some(p) = &d.picture {
        let bytes = http.get_image(p, Mixcloud.allowed_hosts()).expect("picture");
        assert!(bytes.len() > 1000);
    }
}
