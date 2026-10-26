use super::*;

fn page(json: &str) -> String {
    format!(r#"<html><head></head><body><script id="__NEXT_DATA__" type="application/json">{json}</script></body></html>"#)
}

fn entity(tracks: &str) -> String {
    page(&format!(r#"{{"props":{{"pageProps":{{"state":{{"data":{{"entity":{{"type":"playlist","name":"Late night","id":"37i9dQZF1DXcBWIGoYBM5M","trackList":[{tracks}]}}}}}}}}}}}}"#))
}

const ID: &str = "37i9dQZF1DXcBWIGoYBM5M";

#[test]
fn ids_come_out_of_links_uris_and_bare_ids() {
    for text in [
        format!("https://open.spotify.com/playlist/{ID}"),
        format!("https://open.spotify.com/playlist/{ID}?si=abc123"),
        format!("  https://open.spotify.com/intl-de/playlist/{ID}?si=x#top\n"),
        format!("https://open.spotify.com/embed/playlist/{ID}"),
        format!("spotify:playlist:{ID}"),
        ID.to_owned(),
    ] {
        assert_eq!(playlist_id(&text).as_deref(), Some(ID), "{text}");
    }
}

#[test]
fn other_links_are_no_playlists() {
    for text in [
        "",
        "https://open.spotify.com/album/4aawyAB9vmqN3uQ7FjRGTy",
        "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT",
        "https://example.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
        "http://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
        "https://open.spotify.com/playlist/short",
        "spotify:album:4aawyAB9vmqN3uQ7FjRGTy",
        "37i9dQZF1DXcBWIGoYBM5",
    ] {
        assert_eq!(playlist_id(text), None, "{text}");
    }
}

#[test]
fn tracks_are_read_with_artists_and_seconds() {
    let html = entity(
        r#"{"uri":"spotify:track:a","title":"Housewife","subtitle":"Tate McRae","duration":208810,"entityType":"track"},
           {"uri":"spotify:track:b","title":"Under Pressure","subtitle":"Queen, David Bowie","duration":248000,"entityType":"track"}"#,
    );
    let list = parse(&html).unwrap();
    assert_eq!((list.name.as_str(), list.id.as_str(), list.cut), ("Late night", ID, false));
    assert_eq!(list.tracks[0], Track { artist: "Tate McRae".into(), title: "Housewife".into(), duration: Some(209.0), uri: "spotify:track:a".into() });
    assert_eq!((list.tracks[1].artist.as_str(), list.tracks[1].duration), ("Queen, David Bowie", Some(248.0)));
}

#[test]
fn episodes_and_nameless_rows_are_left_out_and_missing_facts_are_kept_empty() {
    let html = entity(
        r#"{"uri":"spotify:episode:e","title":"A podcast","subtitle":"Host","duration":1000,"entityType":"episode"},
           {"uri":"spotify:track:x","title":"","subtitle":"Nobody","entityType":"track"},
           {"uri":"spotify:track:y","title":"Just a title","subtitle":"","duration":0}"#,
    );
    let list = parse(&html).unwrap();
    assert_eq!(list.tracks, vec![Track { artist: String::new(), title: "Just a title".into(), duration: None, uri: "spotify:track:y".into() }]);
}

#[test]
fn a_full_page_may_be_cut_short() {
    let row = |i: usize| format!(r#"{{"uri":"spotify:track:{i}","title":"T{i}","subtitle":"A","duration":1000,"entityType":"track"}}"#);
    let rows = |n: usize| (0..n).map(row).collect::<Vec<_>>().join(",");
    assert!(!parse(&entity(&rows(PAGE - 1))).unwrap().cut);
    let list = parse(&entity(&rows(PAGE))).unwrap();
    assert!(list.cut);
    assert_eq!(list.tracks.len(), PAGE);
}

#[test]
fn a_playlist_that_is_not_public_is_told_apart_from_a_broken_page() {
    let missing = page(r#"{"props":{"pageProps":{"status":404,"title":"","description":""}}}"#);
    assert!(matches!(parse(&missing), Err(Error::NotPublic)));
    assert!(matches!(parse("<html>nothing</html>"), Err(Error::Reply(_))));
    assert!(matches!(parse(&page("{not json")), Err(Error::Reply(_))));
    assert!(matches!(parse(&page(r#"{"props":{"pageProps":{"state":{"data":{"entity":{"name":"x"}}}}}}"#)), Err(Error::Reply(_))));
}

#[test]
fn a_link_is_checked_before_anything_is_requested() {
    assert!(matches!(fetch("https://example.com/"), Err(Error::Address(_))));
}
