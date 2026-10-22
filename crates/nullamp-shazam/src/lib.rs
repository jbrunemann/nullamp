//! Names the tracks of a DJ mix with Shazam, given where each one starts.
//!
//! A few short clips from the middle of each track (away from the blends at both ends) are
//! turned into signatures in the format the Shazam apps send and posted to the endpoint they
//! use. Only the signature leaves the machine, not the audio. Neither the format nor the
//! endpoint is documented (both are known from projects such as SongRec and shazamio), and
//! using them is against Shazam's terms, so they can change or be blocked at any time; the
//! user chose this knowing that, since the official route (ShazamKit) needs a paid developer
//! account.
//!
//! Single clips are often wrong: with five clips per track of a test mix, each track got one
//! or two answers naming some other song, each such song only once. [`agree`] therefore names
//! a track only when several clips name the same song at positions that move with the mix.
//!
//! Requests go through `curl`, like the update check and the artwork lookup. [`recognise`]
//! blocks for up to [`TIMEOUT_SECS`] seconds, so call it from a worker thread, and leave
//! [`PAUSE`] between requests: 2 seconds got "too many requests" (HTTP 429), 5 did not.

mod signature;

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::Value;

pub use signature::encode as signature;

/// Signatures are made from mono audio at this rate.
pub const RATE: u32 = 16000;
/// Length of one clip; the apps send about this much per request.
pub const CLIP_SECONDS: f64 = 12.0;
/// Clips per track. Five gave every track of the test mix two or more agreeing answers.
pub const CLIPS_PER_TRACK: usize = 5;
/// Clips that must agree before a song names a track.
pub const MIN_SUPPORT: usize = 2;
/// Time between requests.
pub const PAUSE: Duration = Duration::from_secs(5);
/// After "too many requests", wait this long before asking again.
pub const BACK_OFF: Duration = Duration::from_secs(60);
/// How long one request may take.
pub const TIMEOUT_SECS: u32 = 20;
/// Answers are tens of kilobytes.
const MAX_REPLY_BYTES: u64 = 4 << 20;

/// A song Shazam named for one clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Song {
    pub artist: String,
    /// The version is part of the title, as in "Sunset on Ibiza (Blr Extended Remix)".
    pub title: String,
    /// Shazam's id of the song; two answers name the same song when their keys are equal.
    pub key: String,
    /// The song's page on shazam.com.
    pub url: Option<String>,
    /// Where in the song the clip starts, in seconds.
    pub offset: f64,
    /// Tempo and pitch of the clip against the song, as fractions (0.025 is 2.5% faster).
    pub time_skew: f64,
    pub frequency_skew: f64,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// HTTP 429. Wait [`BACK_OFF`] before the next request.
    #[error("Shazam refuses more requests for now")]
    TooMany,
    /// No network, a server error, a timeout. Carries curl's message or the status.
    #[error("{0}")]
    Fetch(String),
    /// The answer is not what Shazam sends.
    #[error("{0}")]
    Reply(String),
}

/// Ask Shazam about one clip: mono samples at [`RATE`], about [`CLIP_SECONDS`] long.
/// `Ok(None)` means Shazam answered but knows no song for it.
pub fn recognise(clip: &[f32]) -> Result<Option<Song>, Error> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let uri = format!("data:audio/vnd.shazam.sig;base64,{}", base64::engine::general_purpose::STANDARD.encode(signature(clip)));
    let body = serde_json::json!({
        "geolocation": { "altitude": 300, "latitude": 45, "longitude": 2 },
        "signature": { "samplems": clip.len() as u64 * 1000 / u64::from(RATE), "timestamp": now, "uri": uri },
        "timestamp": now,
        "timezone": "Europe/Amsterdam",
    });
    let url = format!(
        "https://amp.shazam.com/discovery/v5/en/US/android/-/tag/{}/{}?sync=true&webv3=true&sampling=true&connected=&shazamapiversion=v3&sharehub=true&hubv5minorversion=v5.1&hidelb=true&video=v3",
        uuid().to_uppercase(),
        uuid()
    );
    let reply = post(&url, &body.to_string())?;
    parse(&reply)
}

/// The body of the answer. No `--fail`: the status comes after the body, so 429 can be told apart.
fn post(url: &str, body: &str) -> Result<String, Error> {
    let mut child = Command::new("curl")
        .args(["--silent", "--show-error", "--proto", "=https", "--connect-timeout", "8", "--max-time", &TIMEOUT_SECS.to_string()])
        .args(["--write-out", "\n%{http_code}"])
        .args(["--header", "Content-Type: application/json", "--header", "Content-Language: en_US"])
        .args(["--user-agent", "Dalvik/2.1.0 (Linux; U; Android 10; Pixel 3 Build/QP1A.190711.020)"])
        .args(["--data-binary", "@-", "--url", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::NoCurl } else { Error::Fetch(e.to_string()) })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body.as_bytes()).map_err(|e| Error::Fetch(e.to_string()))?;
    }
    let mut out = Vec::new();
    let read = child.stdout.take().map(|o| o.take(MAX_REPLY_BYTES + 1).read_to_end(&mut out));
    let mut message = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut message);
    }
    let status = child.wait().map_err(|e| Error::Fetch(e.to_string()))?;
    if !status.success() {
        return Err(Error::Fetch(message.lines().last().unwrap_or("curl failed").trim_start_matches(|c: char| c != ')').trim_start_matches(") ").to_owned()));
    }
    if let Some(Err(e)) = read {
        return Err(Error::Fetch(e.to_string()));
    }
    if out.len() as u64 > MAX_REPLY_BYTES {
        return Err(Error::Reply("The answer is too long".into()));
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    let (reply, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code.trim() {
        "200" => Ok(reply.to_owned()),
        "429" => Err(Error::TooMany),
        code => Err(Error::Fetch(format!("Shazam answered HTTP {code}"))),
    }
}

fn parse(reply: &str) -> Result<Option<Song>, Error> {
    let reply: Value = serde_json::from_str(reply).map_err(|e| Error::Reply(format!("Unreadable answer from Shazam: {e}")))?;
    let Some(track) = reply.get("track") else { return Ok(None) };
    let text = |key: &str| track.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    let first = reply.get("matches").and_then(|m| m.get(0));
    let number = |key: &str| first.and_then(|m| m.get(key)).and_then(Value::as_f64).unwrap_or(0.0);
    let song = Song {
        artist: text("subtitle"),
        title: text("title"),
        key: text("key"),
        url: Some(text("url")).filter(|u| u.starts_with("https://")),
        offset: number("offset"),
        time_skew: number("timeskew"),
        frequency_skew: number("frequencyskew"),
    };
    Ok(Some(song).filter(|s| !s.title.is_empty() && !s.key.is_empty()))
}

/// A random version 4 UUID, as the apps put in the address.
fn uuid() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Where to cut `count` clips from a track that plays from `start` to `end` (seconds in
/// the mix): spread over its middle 60%, since the first and last fifth are usually blended
/// with the neighbours.
pub fn clip_starts(start: f64, end: f64, count: usize) -> Vec<f64> {
    let span = (end - start - CLIP_SECONDS).max(0.0);
    (0..count).map(|i| start + span * (0.2 + 0.6 * (i as f64 + 0.5) / count as f64)).collect()
}

/// What the clips of one track say together.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// `support` clips agree on `song`.
    Named { song: Song, support: usize },
    /// No song has enough agreeing clips; these are the songs that were named, each once.
    Unsure { candidates: Vec<Song> },
}

/// Combine the answers for the clips of one track, each with the clip's start in the mix.
/// Clips agree when they name the same song at positions that fit their distance in the mix:
/// a chance match names a song at an arbitrary position, a real one moves through it with
/// the mix. DJs change the tempo by a few percent, which stretches distances; 10% covers it.
pub fn agree(answers: &[(f64, Song)]) -> Verdict {
    let fits = |(at_a, a): &(f64, Song), (at_b, b): &(f64, Song)| {
        let (in_mix, in_song) = (at_b - at_a, b.offset - a.offset);
        a.key == b.key && (in_song - in_mix).abs() <= 3.0 + 0.1 * in_mix.abs()
    };
    let mut ranked: Vec<(&Song, usize)> = answers.iter().map(|a| (&a.1, answers.iter().filter(|b| fits(a, b)).count())).collect();
    ranked.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if let Some(&(best, support)) = ranked.first() {
        // Two different songs with equal support leave the track undecided.
        let tied = ranked.iter().any(|(song, n)| *n == support && song.key != best.key);
        if support >= MIN_SUPPORT && !tied {
            return Verdict::Named { song: best.clone(), support };
        }
    }
    let mut candidates: Vec<Song> = Vec::new();
    for (_, song) in answers {
        if !candidates.iter().any(|c| c.key == song.key) {
            candidates.push(song.clone());
        }
    }
    Verdict::Unsure { candidates }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(at: f64, key: &str, offset: f64) -> (f64, Song) {
        (at, Song { artist: String::new(), title: key.into(), key: key.into(), url: None, offset, time_skew: 0.0, frequency_skew: 0.0 })
    }

    fn named(verdict: &Verdict) -> Option<(&str, usize)> {
        match verdict {
            Verdict::Named { song, support } => Some((song.key.as_str(), *support)),
            Verdict::Unsure { .. } => None,
        }
    }

    // Answers from a run on the test mix (clip start in the mix, position in the song).
    #[test]
    fn clips_agree_only_where_the_song_moves_with_the_mix() {
        // The third clip lands in a repeated part of the song, the fourth names another song.
        let troopa = [answer(38.0, "troopa", 36.0), answer(54.0, "troopa", 52.0), answer(69.0, "troopa", 68.0), answer(85.0, "mwaki", 65.0), answer(96.0, "troopa", 39.0)];
        assert_eq!(named(&agree(&troopa)), Some(("troopa", 3)));
        // Three different songs, one clip each.
        let gone = [answer(649.0, "the man", 487.0), answer(716.0, "gone", 235.0), answer(783.0, "devotion", 331.0)];
        let Verdict::Unsure { candidates } = agree(&gone) else { panic!("named a song") };
        assert_eq!(candidates.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["the man", "gone", "devotion"]);
        // The same song twice, at positions the mix cannot have played them in.
        assert_eq!(named(&agree(&[answer(100.0, "x", 300.0), answer(160.0, "x", 20.0)])), None);
        // Two songs, two agreeing clips each.
        assert_eq!(named(&agree(&[answer(0.0, "a", 10.0), answer(30.0, "a", 40.0), answer(60.0, "b", 5.0), answer(90.0, "b", 35.0)])), None);
        assert_eq!(agree(&[]), Verdict::Unsure { candidates: Vec::new() });
    }

    #[test]
    fn clips_come_from_the_middle_of_the_track() {
        let starts = clip_starts(100.0, 400.0, 5);
        assert_eq!(starts.len(), 5);
        assert!(starts[0] > 100.0 + 0.2 * 288.0 && starts[4] + CLIP_SECONDS < 400.0 - 0.2 * 288.0 + CLIP_SECONDS);
        assert!(starts.windows(2).all(|w| w[1] > w[0]));
        // A track shorter than a clip still gets clips, all at its start.
        assert!(clip_starts(10.0, 15.0, 2).iter().all(|&s| s == 10.0));
    }

    #[test]
    fn reads_an_answer() {
        let reply = r#"{"matches":[{"id":"1","offset":163.5,"timeskew":0.025,"frequencyskew":-0.0001}],
            "track":{"key":"5933917","title":"Troopa (feat. Yung Felix)","subtitle":"Donnie","url":"https://www.shazam.com/track/5933917/troopa"}}"#;
        let song = parse(reply).unwrap().unwrap();
        assert_eq!((song.artist.as_str(), song.title.as_str(), song.key.as_str()), ("Donnie", "Troopa (feat. Yung Felix)", "5933917"));
        assert_eq!(song.url.as_deref(), Some("https://www.shazam.com/track/5933917/troopa"));
        assert_eq!((song.offset, song.time_skew), (163.5, 0.025));
        assert_eq!(parse(r#"{"matches":[],"retryms":4000}"#), Ok(None));
        assert!(matches!(parse("<html>"), Err(Error::Reply(_))));
    }
}
