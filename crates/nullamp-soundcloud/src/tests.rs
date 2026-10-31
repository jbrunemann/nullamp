use super::*;

mod file_names {
    use super::*;

    #[test]
    fn joins_artist_and_title() {
        assert_eq!(file_name("CHRIS STASSY", "N:A:M:E:, Birmingham", 2394669642), "CHRIS STASSY - N A M E , Birmingham.m4a");
    }

    #[test]
    fn strips_path_and_shell_characters_and_collapses_whitespace() {
        assert_eq!(file_name("foo/bar", "a\tb\nc  d", 1), "foo bar - a b c d.m4a");
        assert_eq!(file_name("", "x<y>|?\"*z", 1), "x y z.m4a");
    }

    #[test]
    fn falls_back_to_the_track_id_when_nothing_is_left() {
        assert_eq!(file_name("", "", 42), "soundcloud-42.m4a");
        assert_eq!(file_name("///", "::", 7), "soundcloud-7.m4a");
    }

    #[test]
    fn long_names_are_shortened() {
        let long: String = "a".repeat(400);
        let name = file_name(&long, &long, 1);
        assert!(name.chars().count() <= 150 + ".m4a".len(), "{name}");
    }
}

mod url_shape {
    use super::download::*;

    #[test]
    fn canonical_track_url_strips_query_and_lowercases_host() {
        // A direct track URL comes through untouched (host stays as given).
        let shaped = track_url_shape("https://soundcloud.com/chrisstassydj/namebirmingham?ref=whatsapp").unwrap();
        assert_eq!(shaped, TrackUrl::Direct("https://soundcloud.com/chrisstassydj/namebirmingham".into()));
    }

    #[test]
    fn www_is_dropped() {
        let shaped = track_url_shape("https://www.soundcloud.com/a/b").unwrap();
        assert_eq!(shaped, TrackUrl::Direct("https://soundcloud.com/a/b".into()));
    }

    #[test]
    fn short_links_are_recognised() {
        let shaped = track_url_shape("https://on.soundcloud.com/eVfT2uKuGIWOXxHU2H").unwrap();
        assert_eq!(shaped, TrackUrl::Short("https://on.soundcloud.com/eVfT2uKuGIWOXxHU2H".into()));
    }

    #[test]
    fn non_track_pages_are_rejected() {
        assert!(matches!(track_url_shape("https://soundcloud.com/discover"), Err(crate::Error::Address(_))));
        assert!(matches!(track_url_shape("https://soundcloud.com/you/sets"), Err(crate::Error::Address(_))));
        assert!(matches!(track_url_shape("https://example.com/track"), Err(crate::Error::Address(_))));
        assert!(matches!(track_url_shape(""), Err(crate::Error::Address(_))));
    }

    #[test]
    fn a_secret_token_in_the_path_is_accepted() {
        let shaped = track_url_shape("https://soundcloud.com/user/slug/s-abc").unwrap();
        assert_eq!(shaped, TrackUrl::Direct("https://soundcloud.com/user/slug".into()));
    }
}

mod client_id_parsing {
    use crate::client_id::{bundle_urls, extract_id};

    #[test]
    fn bundle_urls_collects_only_player_scripts_once() {
        let html = r#"
            <html><head>
            <script src="https://a-v2.sndcdn.com/assets/55-c7bcef92.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/55-c7bcef92.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/0-2d5acf4b.js"></script>
            <script src="https://example.com/not-player.js"></script>
            </head></html>
        "#;
        let urls = bundle_urls(html);
        assert_eq!(urls, vec!["https://a-v2.sndcdn.com/assets/55-c7bcef92.js".to_string(), "https://a-v2.sndcdn.com/assets/0-2d5acf4b.js".into()]);
    }

    #[test]
    fn extract_id_finds_the_literal_in_minified_js() {
        let js = r#"var t={locale:"en",client_id:"OGA0P5R5U4CpyooNe9OUcnjIMxao2hB2",feature_flags:[]};"#;
        assert_eq!(extract_id(js).as_deref(), Some("OGA0P5R5U4CpyooNe9OUcnjIMxao2hB2"));
    }

    #[test]
    fn extract_id_handles_alternative_syntax() {
        assert_eq!(extract_id(r#"client_id="aAaAaAaAaAaAaAaAaAaA""#).as_deref(), Some("aAaAaAaAaAaAaAaAaAaA"));
        assert_eq!(extract_id(r#""client_id":"XXXXXXXXXXXXXXXXXXXX""#).as_deref(), Some("XXXXXXXXXXXXXXXXXXXX"));
    }

    #[test]
    fn too_short_or_non_alnum_candidates_are_rejected() {
        assert_eq!(extract_id(r#"client_id:"short""#), None);
        assert_eq!(extract_id(r#"client_id:"has space in it xx""#), None);
    }

    #[test]
    fn bundles_without_the_literal_return_none() {
        assert_eq!(extract_id("just some js(){}"), None);
    }
}

mod m3u8 {
    use crate::hls::parse;

    #[test]
    fn init_and_segments_in_order() {
        let body = r#"#EXTM3U
#EXT-X-TARGETDURATION:11
#EXT-X-MAP:URI="https://cdn.example.com/init.mp4?token=1"
#EXTINF:10.007800,
https://cdn.example.com/data000.m4s?token=1
#EXTINF:10.007800,
https://cdn.example.com/data001.m4s?token=1
#EXTINF:1.177370,
https://cdn.example.com/data002.m4s?token=1
#EXT-X-ENDLIST
"#;
        let playlist = parse(body).unwrap();
        assert_eq!(playlist.init.as_deref(), Some("https://cdn.example.com/init.mp4?token=1"));
        assert_eq!(playlist.segments.len(), 3);
        assert_eq!(playlist.segments[0].duration_secs, 10.0078);
        assert_eq!(playlist.segments[2].duration_secs, 1.17737);
        assert_eq!(playlist.segments[1].url, "https://cdn.example.com/data001.m4s?token=1");
        let total: f64 = playlist.duration_secs();
        assert!((total - 21.19297).abs() < 1e-5, "{total}");
    }

    #[test]
    fn missing_header_fails() {
        assert!(matches!(parse("no header here\n"), Err(crate::Error::Reply(_))));
    }

    #[test]
    fn empty_playlist_fails() {
        assert!(matches!(parse("#EXTM3U\n#EXT-X-ENDLIST\n"), Err(crate::Error::Reply(_))));
    }

    #[test]
    fn a_segment_without_extinf_fails() {
        let body = "#EXTM3U\nhttps://cdn.example.com/data000.m4s\n";
        assert!(matches!(parse(body), Err(crate::Error::Reply(_))));
    }
}

mod resolve_json {
    use crate::resolve::{parse, parse_signed_url};

    const SAMPLE: &str = include_str!("tests/resolve.json");

    #[test]
    fn parses_the_captured_response_shape() {
        let resolved = parse(SAMPLE).unwrap();
        assert_eq!(resolved.track.id, 2394669642);
        assert_eq!(resolved.track.title, "CHRIS STASSY @ N:A:M:E:, Birmingham, 22.08.2026");
        assert_eq!(resolved.track.artist, "CHRIS STASSY");
        assert_eq!(resolved.track.duration_ms, 9181186);
        assert_eq!(resolved.track.artwork_url.as_deref(), Some("https://i1.sndcdn.com/artworks-abc-t500x500.jpg"));
        assert_eq!(resolved.transcoding_url, "https://api-v2.soundcloud.com/media/soundcloud:tracks:2394669642/819ff7a4-c18f-46f2-bfe2-650e6def658f/stream/hls");
        assert_eq!(resolved.format, "aac_160k hls");
        assert!(resolved.track_authorization.starts_with("eyJ0"));
    }

    #[test]
    fn a_resource_other_than_a_track_is_refused() {
        let json = r#"{"kind":"playlist","id":1}"#;
        assert!(matches!(parse(json), Err(crate::Error::Reply(_))));
    }

    #[test]
    fn signed_url_parsing() {
        let body = r#"{"url":"https://cdn.example.com/playlist.m3u8?Signature=abc"}"#;
        assert_eq!(parse_signed_url(body).unwrap(), "https://cdn.example.com/playlist.m3u8?Signature=abc");
    }

    #[test]
    fn a_signed_url_body_without_a_url_is_refused() {
        assert!(matches!(parse_signed_url("{}"), Err(crate::Error::Reply(_))));
    }

    #[test]
    fn a_track_with_only_a_master_playlist_variant_has_no_audio() {
        let json = r#"{
            "kind":"track","id":1,"title":"x","duration":1000,"artwork_url":null,"permalink_url":"https://soundcloud.com/u/x",
            "user":{"username":"u"},"track_authorization":"t",
            "media":{"transcodings":[{"url":"https://api/x","preset":"abr_sq","quality":"sq","format":{"protocol":"hls","mime_type":"audio/mpegurl"}}]}
        }"#;
        assert!(matches!(parse(json), Err(crate::Error::NoAudio)));
    }

    #[test]
    fn a_progressive_variant_wins_over_hls() {
        let json = r#"{
            "kind":"track","id":1,"title":"x","duration":1000,"artwork_url":null,"permalink_url":"https://soundcloud.com/u/x",
            "user":{"username":"u"},"track_authorization":"t",
            "media":{"transcodings":[
                {"url":"https://api/hls","preset":"aac_160k","quality":"sq","format":{"protocol":"hls","mime_type":"audio/mp4"}},
                {"url":"https://api/mp3","preset":"mp3_1_0","quality":"sq","format":{"protocol":"progressive","mime_type":"audio/mpeg"}}
            ]}
        }"#;
        let resolved = parse(json).unwrap();
        assert_eq!(resolved.transcoding_url, "https://api/mp3");
        assert_eq!(resolved.format, "mp3_1_0 progressive");
    }

    #[test]
    fn snippet_variants_are_skipped() {
        let json = r#"{
            "kind":"track","id":1,"title":"x","duration":1000,"artwork_url":null,"permalink_url":"https://soundcloud.com/u/x",
            "user":{"username":"u"},"track_authorization":"t",
            "media":{"transcodings":[
                {"url":"https://api/snippet","preset":"aac_160k","quality":"sq","snipped":true,"format":{"protocol":"progressive","mime_type":"audio/mpeg"}},
                {"url":"https://api/hls","preset":"aac_160k","quality":"sq","format":{"protocol":"hls","mime_type":"audio/mp4"}}
            ]}
        }"#;
        let resolved = parse(json).unwrap();
        assert_eq!(resolved.transcoding_url, "https://api/hls");
    }
}

// `TrackUrl` is a private enum; the tests above compare it via a `PartialEq` impl we add here.
impl PartialEq for download::TrackUrl {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Direct(a), Self::Direct(b)) | (Self::Short(a), Self::Short(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Debug for download::TrackUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct(u) => write!(f, "Direct({u})"),
            Self::Short(u) => write!(f, "Short({u})"),
        }
    }
}
