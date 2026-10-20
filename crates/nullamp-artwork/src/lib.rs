//! The cover of one track of a mix, found by its artist and title.
//!
//! Deezer's search is asked first and iTunes Search after it; both are free and need no
//! key. Deezer found the right release for every track of a test mix and has 1000 pixel
//! covers. It often names only the first artist of a collaboration, so a result is
//! accepted when one of its artists is one of ours. iTunes is limited to about 20
//! requests a minute, which is why it only fills in.
//!
//! Requests go through `curl`, like the update check, so this crate needs no TLS stack.
//! [`find`] and [`download`] block for up to [`TIMEOUT_SECS`] seconds per request, so
//! call them from a worker thread.

use std::io::Read;
use std::process::{Command, Stdio};

use serde_json::Value;

/// How long one request may take.
pub const TIMEOUT_SECS: u32 = 15;
/// Search answers are a few kilobytes.
const MAX_REPLY_BYTES: u64 = 1 << 20;
/// Covers are a few hundred kilobytes.
const MAX_IMAGE_BYTES: u64 = 8 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Deezer,
    Itunes,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Deezer => "Deezer",
            Source::Itunes => "iTunes",
        }
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub artist: String,
    pub title: String,
    pub release: String,
    /// The release is a compilation by various artists, not the track's own release.
    pub compilation: bool,
    pub image_url: String,
}

/// The accepted result and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub candidate: Candidate,
    pub source: Source,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// No network, a server error, a timeout. Carries curl's message.
    #[error("{0}")]
    Fetch(String),
    /// The answer is not what the service sends.
    #[error("{0}")]
    Reply(String),
}

/// Look `artist` and `title` up, Deezer first. `Ok(None)` means the services answered but
/// had nothing close enough; an error means none of them could be asked.
pub fn find(artist: &str, title: &str, program: &str) -> Result<Option<Found>, Error> {
    let mut failure = None;
    let mut answered = false;
    for source in [Source::Deezer, Source::Itunes] {
        for query in queries(artist, title) {
            let reply = fetch(&search_url(source, &query), program, MAX_REPLY_BYTES).and_then(|body| parse(source, &String::from_utf8_lossy(&body)));
            match reply {
                Ok(candidates) => {
                    answered = true;
                    if let Some(c) = best(artist, title, &candidates) {
                        return Ok(Some(Found { candidate: c.clone(), source }));
                    }
                }
                // The other service may still answer.
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
    }
    match failure {
        Some(e) if !answered => Err(e),
        _ => Ok(None),
    }
}

/// The image at `url`.
pub fn download(url: &str, program: &str) -> Result<Vec<u8>, Error> {
    if !url.starts_with("https://") || url.contains(char::is_whitespace) {
        return Err(Error::Reply(format!("\"{url}\" is not an https address")));
    }
    fetch(url, program, MAX_IMAGE_BYTES)
}

/// What to search for: the title as given, without suffixes that only say it is the
/// usual version, then the bare title when that differs.
fn queries(artist: &str, title: &str) -> Vec<String> {
    let (base, versions) = split_title(title);
    let kept: Vec<&str> = versions.iter().map(String::as_str).filter(|v| !is_plain_version(v)).collect();
    let full = if kept.is_empty() { base.clone() } else { format!("{base} {}", kept.join(" ")) };
    let mut out = vec![format!("{} {full}", artist.trim())];
    if full != base {
        out.push(format!("{} {base}", artist.trim()));
    }
    out
}

fn search_url(source: Source, query: &str) -> String {
    match source {
        Source::Deezer => format!("https://api.deezer.com/search?limit=15&q={}", encode(query)),
        Source::Itunes => format!("https://itunes.apple.com/search?media=music&entity=song&limit=15&term={}", encode(query)),
    }
}

/// Percent-encodes everything but the unreserved characters.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The results in an answer from `source`. Results without a cover are left out.
pub fn parse(source: Source, body: &str) -> Result<Vec<Candidate>, Error> {
    let json: Value = serde_json::from_str(body).map_err(|_| Error::Reply(format!("{} did not answer with JSON", source.name())))?;
    if let Some(e) = json.get("error") {
        let why = e.get("message").and_then(Value::as_str).unwrap_or("an error");
        return Err(Error::Reply(format!("{} answered: {why}", source.name())));
    }
    let text = |v: &Value, path: &[&str]| path.iter().try_fold(v, |v, k| v.get(k)).and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    let list = json.get(if source == Source::Deezer { "data" } else { "results" }).and_then(Value::as_array);
    let candidates = list.into_iter().flatten().filter_map(|r| {
        let c = match source {
            Source::Deezer => Candidate {
                artist: text(r, &["artist", "name"]),
                title: text(r, &["title"]),
                release: text(r, &["album", "title"]),
                compilation: false,
                image_url: text(r, &["album", "cover_xl"]),
            },
            Source::Itunes => Candidate {
                artist: text(r, &["artistName"]),
                title: text(r, &["trackName"]),
                release: text(r, &["collectionName"]),
                compilation: text(r, &["collectionArtistName"]).eq_ignore_ascii_case("various artists"),
                // The 100 pixel address names its size; the same image comes in other sizes.
                image_url: text(r, &["artworkUrl100"]).replace("100x100bb", "600x600bb"),
            },
        };
        // Deezer's address for a release without a cover has no image id.
        let has_image = c.image_url.starts_with("https://") && !c.image_url.contains("/cover//");
        (has_image && !c.title.is_empty()).then_some(c)
    });
    Ok(candidates.collect())
}

/// The result most like `artist` and `title`, if any is close enough: one of its artists
/// must be one of ours and its title, without the version, must be ours. Among those an
/// exact version wins, and DJ-mix and various-artists releases lose.
pub fn best<'a>(artist: &str, title: &str, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    let ours = artists(artist);
    let (base, all_versions) = split_title(title);
    let versions = meaningful(&all_versions);
    let mut best: Option<(i32, &Candidate)> = None;
    for c in candidates {
        let theirs = artists(&c.artist);
        if !theirs.iter().any(|a| ours.contains(a)) {
            continue;
        }
        let (their_base, their_versions) = split_title(&c.title);
        if their_base != base {
            continue;
        }
        // "Boiling Point (Edit)" and "Boiling Point" can be separate releases with their own covers.
        let mut score = if sorted(&their_versions) == sorted(&all_versions) { 1 } else { 0 };
        let their_versions = meaningful(&their_versions);
        score += if their_versions == versions { 2 } else { 0 };
        if their_versions.iter().any(|v| v == "mixed") || normalize(&c.release).contains("mixed") {
            score -= 2;
        }
        if c.compilation {
            score -= 1;
        }
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Lowercase words of letters and digits. Apostrophes join rather than split, so
/// "Keepin' On" and "Keepin On" agree.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !matches!(ch, '\'' | '’' | '`') && !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

/// The single names in a credit such as "A & B feat. C".
fn artists(credit: &str) -> Vec<String> {
    let mut text = format!(" {} ", credit.to_lowercase());
    for sep in [" & ", ",", " feat. ", " feat ", " ft. ", " ft ", " featuring ", " vs. ", " vs ", " x ", " and ", " with ", " + ", "/"] {
        text = text.replace(sep, "\u{1}");
    }
    text.split('\u{1}').map(normalize).filter(|a| !a.is_empty()).collect()
}

/// The title without what is in brackets or after " - ", and those parts.
fn split_title(title: &str) -> (String, Vec<String>) {
    let mut base = String::new();
    let mut versions = Vec::new();
    let mut part = String::new();
    let mut depth = 0;
    for ch in title.chars() {
        match ch {
            '(' | '[' => {
                if depth == 0 {
                    base.push(' ');
                } else {
                    part.push(ch);
                }
                depth += 1;
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    versions.push(normalize(&part));
                    part.clear();
                } else {
                    part.push(ch);
                }
            }
            _ if depth > 0 => part.push(ch),
            _ => base.push(ch),
        }
    }
    if let Some((head, tail)) = base.split_once(" - ") {
        versions.push(normalize(tail));
        base = head.to_owned();
    }
    // "Title feat. Someone" names a guest, not another title.
    let mut base = normalize(&base);
    for guest in [" feat ", " ft ", " featuring "] {
        if let Some(at) = base.find(guest) {
            base.truncate(at);
        }
    }
    versions.retain(|v| !v.is_empty());
    (base, versions)
}

/// Versions that only say "the usual one", which releases name inconsistently.
fn is_plain_version(version: &str) -> bool {
    matches!(version, "original mix" | "original" | "extended mix" | "extended" | "edit" | "radio edit" | "extended version" | "album version")
        || version.starts_with("feat ")
        || version.starts_with("ft ")
        || version.starts_with("featuring ")
}

fn meaningful(versions: &[String]) -> Vec<String> {
    sorted(&versions.iter().filter(|v| !is_plain_version(v)).cloned().collect::<Vec<_>>())
}

fn sorted(versions: &[String]) -> Vec<String> {
    let mut out = versions.to_vec();
    out.sort();
    out
}

/// The body at `url`, by running curl. Redirects are followed, but only to `https`.
fn fetch(url: &str, program: &str, max_bytes: u64) -> Result<Vec<u8>, Error> {
    let mut child = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--connect-timeout", "8", "--max-time", &TIMEOUT_SECS.to_string()])
        .args(["--user-agent", program, "--url", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::NoCurl } else { Error::Fetch(e.to_string()) })?;
    let mut body = Vec::new();
    let read = child.stdout.take().map(|out| out.take(max_bytes + 1).read_to_end(&mut body));
    if body.len() as u64 > max_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Reply("The answer is too long".to_owned()));
    }
    let mut message = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut message);
    }
    let status = child.wait().map_err(|e| Error::Fetch(e.to_string()))?;
    if !status.success() {
        return Err(Error::Fetch(curl_message(&message)));
    }
    if let Some(Err(e)) = read {
        return Err(Error::Fetch(e.to_string()));
    }
    Ok(body)
}

/// curl's last line without its `curl: (22) ` prefix.
fn curl_message(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The request failed").trim();
    let line = line.strip_prefix("curl:").map_or(line, str::trim_start);
    let line = line.strip_prefix('(').and_then(|rest| rest.split_once(')')).map_or(line, |(_, rest)| rest.trim_start());
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(artist: &str, title: &str, release: &str) -> Candidate {
        Candidate { artist: artist.into(), title: title.into(), release: release.into(), compilation: false, image_url: format!("https://img/{release}") }
    }

    #[test]
    fn normalizes_case_punctuation_and_apostrophes() {
        assert_eq!(normalize("Keepin' On"), "keepin on");
        assert_eq!(normalize("Need U, Want U"), "need u want u");
        assert_eq!(normalize("  Ben van  Kuringen "), "ben van kuringen");
        assert_eq!(normalize("Kölsch"), "kölsch");
    }

    #[test]
    fn splits_credits_into_artists() {
        assert_eq!(artists("CHRIS STASSY & Toman"), ["chris stassy", "toman"]);
        assert_eq!(artists("A feat. B, C x D"), ["a", "b", "c", "d"]);
        assert_eq!(artists("Rico Scott & Leigh Hunt"), ["rico scott", "leigh hunt"]);
    }

    #[test]
    fn splits_titles_into_base_and_versions() {
        assert_eq!(split_title("Follow Me (Benny S Remix)"), ("follow me".into(), vec!["benny s remix".into()]));
        assert_eq!(split_title("Freak in Me - Extended Mix"), ("freak in me".into(), vec!["extended mix".into()]));
        assert_eq!(split_title("Song feat. Guest [Dub]"), ("song".into(), vec!["dub".into()]));
        assert_eq!(split_title("Node 03"), ("node 03".into(), vec![]));
    }

    #[test]
    fn queries_drop_plain_versions_and_fall_back_to_the_bare_title() {
        assert_eq!(queries("Anil Aras", "Zy Lophone"), ["Anil Aras zy lophone"]);
        assert_eq!(queries("X", "Boiling Point (Edit)"), ["X boiling point"]);
        assert_eq!(queries("Oreason", "Follow Me (Benny S Remix)"), ["Oreason follow me benny s remix", "Oreason follow me"]);
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(encode("Tino & Malachy keepin' on"), "Tino%20%26%20Malachy%20keepin%27%20on");
        assert_eq!(encode("é"), "%C3%A9");
    }

    #[test]
    fn accepts_a_subset_of_the_artists() {
        let c = [candidate("CHRIS STASSY", "Boiling Point (Edit)", "Timewriter EP")];
        assert_eq!(best("CHRIS STASSY & Toman", "Boiling Point (Edit)", &c), Some(&c[0]));
        assert_eq!(best("Toman", "Boiling Point", &c), None, "the one artist named is not ours");
    }

    #[test]
    fn rejects_other_titles_and_other_artists() {
        let c = [candidate("CHRIS STASSY", "Desire", "Desire"), candidate("Shawn Mendes", "Need U, Want U", "x")];
        assert_eq!(best("CHRIS STASSY & Toman", "Boiling Point", &c), None);
        assert_eq!(best("Kolter", "Need U, Want U", &c), None);
    }

    #[test]
    fn prefers_the_same_version_and_the_artists_own_release() {
        let c = [
            candidate("Philip George", "Freak In Me (Mixed)", "Dansu Deep Cuts 2025"),
            candidate("Oreason", "Follow Me", "Follow Me"),
            candidate("Oreason", "Follow Me (Benny S Remix)", "Follow Me E.P"),
            Candidate { compilation: true, ..candidate("Kolter", "Need U, Want U", "House of House") },
            candidate("Kolter", "Need U, Want U", "Need U, Want U"),
            candidate("Philip George", "Freak In Me", "Freak In Me"),
        ];
        assert_eq!(best("Oreason", "Follow Me (Benny S Remix)", &c).unwrap().release, "Follow Me E.P");
        assert_eq!(best("Oreason", "Follow Me", &c).unwrap().release, "Follow Me");
        assert_eq!(best("Kolter", "Need U, Want U", &c).unwrap().release, "Need U, Want U");
        assert_eq!(best("Philip George", "Freak In Me", &c).unwrap().release, "Freak In Me");
        assert_eq!(best("Philip George", "Freak In Me (Original Mix)", &c).unwrap().release, "Freak In Me", "a plain version is no version");
    }

    #[test]
    fn an_exact_plain_version_breaks_a_tie() {
        let c = [candidate("CHRIS STASSY", "Boiling Point", "Single"), candidate("CHRIS STASSY", "Boiling Point (Edit)", "Timewriter EP")];
        assert_eq!(best("CHRIS STASSY & Toman", "Boiling Point (Edit)", &c).unwrap().release, "Timewriter EP");
        assert_eq!(best("CHRIS STASSY & Toman", "Boiling Point", &c).unwrap().release, "Single");
    }

    #[test]
    fn a_different_version_is_still_better_than_nothing() {
        let c = [candidate("Ben Van Kuringen", "Necesito", "Necesito")];
        assert_eq!(best("Ben van Kuringen", "Necesito (Evock Remix)", &c), Some(&c[0]));
    }

    #[test]
    fn parses_deezer() {
        let body = r#"{"data": [
            {"title": "Dexed Tek", "artist": {"name": "Anil Aras"}, "album": {"title": "When Souls Collide EP", "cover_xl": "https://cdn-images.dzcdn.net/images/cover/f9ef/1000x1000.jpg"}},
            {"title": "No Cover", "artist": {"name": "A"}, "album": {"title": "B", "cover_xl": "https://cdn-images.dzcdn.net/images/cover//1000x1000.jpg"}},
            {"title": "Null Cover", "artist": {"name": "A"}, "album": {"title": "B", "cover_xl": null}}
        ], "total": 3}"#;
        let c = parse(Source::Deezer, body).unwrap();
        assert_eq!(c, [Candidate { artist: "Anil Aras".into(), title: "Dexed Tek".into(), release: "When Souls Collide EP".into(), compilation: false, image_url: "https://cdn-images.dzcdn.net/images/cover/f9ef/1000x1000.jpg".into() }]);
        assert!(matches!(parse(Source::Deezer, r#"{"error": {"type": "Exception", "message": "Quota limit exceeded", "code": 4}}"#), Err(Error::Reply(m)) if m.contains("Quota")));
        assert!(matches!(parse(Source::Deezer, "<html>"), Err(Error::Reply(_))));
        assert_eq!(parse(Source::Deezer, r#"{"data": [], "total": 0}"#), Ok(vec![]));
    }

    #[test]
    fn parses_itunes_and_asks_for_a_larger_image() {
        let body = r#"{"resultCount": 1, "results": [{"artistName": "Kolter", "trackName": "Need U, Want U", "collectionName": "House of House", "collectionArtistName": "Various Artists", "artworkUrl100": "https://is1-ssl.mzstatic.com/image/thumb/x/cover.jpg/100x100bb.jpg"}]}"#;
        let c = parse(Source::Itunes, body).unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].compilation);
        assert_eq!(c[0].image_url, "https://is1-ssl.mzstatic.com/image/thumb/x/cover.jpg/600x600bb.jpg");
    }

    #[test]
    fn only_https_images_are_downloaded() {
        for url in ["http://example.org/a.jpg", "file:///etc/passwd", "https://exa mple.org/a.jpg"] {
            assert!(matches!(download(url, "test/0"), Err(Error::Reply(_))), "{url}");
        }
    }

    /// Needs the network.
    #[test]
    #[ignore = "asks Deezer and iTunes; run with --ignored"]
    fn finds_the_tracks_of_a_real_mix() {
        let found = find("CHRIS STASSY & Toman", "Boiling Point (Edit)", "nullamp-artwork-test/0").unwrap().unwrap();
        assert_eq!((found.source, found.candidate.release.as_str()), (Source::Deezer, "Timewriter EP"));
        let found = find("Tino & Malachy", "Keepin' On", "nullamp-artwork-test/0").unwrap().unwrap();
        assert_eq!(found.candidate.release, "Foundations EP");
        assert!(download(&found.candidate.image_url, "nullamp-artwork-test/0").unwrap().len() > 10_000);
        assert_eq!(find("Zzqx Nonexistent Artist", "Qqzx No Such Title", "nullamp-artwork-test/0"), Ok(None));
    }
}
