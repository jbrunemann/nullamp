//! Finds the tracklist of a DJ mix on 1001Tracklists.
//!
//! The site has no API, so this reads its pages the way a browser does: the search page is a
//! form post, a tracklist is an HTML page. The search needs nothing, but a tracklist page is
//! refused (HTTP 401) unless the request carries the `guid` cookie the site gives a browser. The
//! user copies it from their own browser into the settings; nothing here makes one up, and
//! nothing is requested until the user asks for a search or picks a result. The page layout is
//! not documented and can change, in which case [`parse_hits`] and [`parse_tracklist`] come up
//! empty or fail with [`Error::Reply`].
//!
//! Pages are fetched with `curl`, like the update check, so this crate needs no TLS stack.
//! [`search`] and [`fetch`] block for up to [`TIMEOUT_SECS`] seconds: call them from a worker thread.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::Regex;

/// How long one request may take.
pub const TIMEOUT_SECS: u32 = 20;
/// A tracklist page is about 200 KB; anything much longer is not one.
const MAX_BYTES: u64 = 8 << 20;
const SITE: &str = "https://www.1001tracklists.com";
/// The site turns away clients that do not look like a browser.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// The cookie is empty or holds characters a cookie value cannot.
    #[error("The guid in Settings is empty or not valid")]
    Guid,
    /// The address is not a tracklist page of the site, so the cookie is not sent to it.
    #[error("\"{0}\" is not a 1001Tracklists tracklist address")]
    Address(String),
    /// No network, a server error, a timeout. Carries curl's message.
    #[error("{0}")]
    Fetch(String),
    /// The site refused the request. For a tracklist page this is what a missing or expired guid looks like.
    #[error("1001Tracklists refused the request (HTTP {0}); the guid in Settings may be wrong or expired")]
    Refused(u16),
    /// The answer is not the page that was asked for.
    #[error("{0}")]
    Reply(String),
}

/// One result of a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    /// The tracklist page, an absolute address.
    pub url: String,
    /// "2020-07-10"
    pub date: Option<String>,
    pub tracks: Option<u32>,
    /// The site's own wording, "59m" or "1h 12m".
    pub duration: Option<String>,
    /// How well it fits what was asked for, set by [`rank`]; 0 before.
    pub score: i32,
    /// Set by [`rank`] for a hit that is convincing, as opposed to the best of a poor lot: most of
    /// the words of the query are in its title, it is of the year asked for (when one was) and
    /// about as long as the file (when both are known).
    pub likely: bool,
}

impl Hit {
    /// The year of the tracklist's date.
    pub fn year(&self) -> Option<u16> {
        self.date.as_deref()?.get(..4)?.parse().ok()
    }

    /// Length in seconds, read from the site's "1h 12m".
    pub fn seconds(&self) -> Option<f64> {
        let mut total = 0.0;
        let mut any = false;
        for part in self.duration.as_deref()?.split_whitespace() {
            let (number, unit) = part.split_at(part.find(|c: char| !c.is_ascii_digit())?);
            let n: f64 = number.parse().ok()?;
            total += n * match unit {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                _ => return None,
            };
            any = true;
        }
        any.then_some(total)
    }
}

/// What the mix being looked for is known to be.
#[derive(Clone, Debug, Default)]
pub struct Wanted<'a> {
    /// The words searched for.
    pub query: &'a str,
    /// The year of the mix, from the file's date tag.
    pub year: Option<u16>,
    /// The length of the file in seconds.
    pub seconds: Option<f64>,
}

/// The share of the query's words a title must have to be a [`Hit::likely`] match.
const LIKELY_WORDS: f64 = 0.75;
/// Words that say little about which mix it is.
const FILLER: [&str; 11] = ["live", "at", "the", "a", "and", "set", "mix", "dj", "in", "of", "on"];

/// Puts the hits in the order they fit `want`, best first. The year counts most: a tracklist
/// of the year the file's tag names is almost always the one, one a year off (a mix tagged
/// with the year it was released, played the year before) a little. Then come the words of the
/// query found in the title, then a length near the file's. Hits that fit equally keep the
/// site's order.
pub fn rank(hits: &mut [Hit], want: &Wanted) {
    let wanted_words: Vec<String> = words(want.query).filter(|w| !FILLER.contains(&w.as_str()) && !is_year(w)).collect();
    for hit in hits.iter_mut() {
        let mut score = 0.0;
        let mut likely = true;
        if let Some(wanted) = want.year {
            match hit.year().map(|year| wanted.abs_diff(year)) {
                Some(0) => score += 60.0,
                Some(1) => {
                    score += 15.0;
                    likely = false;
                }
                _ => likely = false,
            }
        }
        if wanted_words.is_empty() {
            likely = false;
        } else {
            let title: Vec<String> = words(&hit.title).collect();
            let share = wanted_words.iter().filter(|w| title.contains(w)).count() as f64 / wanted_words.len() as f64;
            score += 60.0 * share;
            likely &= share >= LIKELY_WORDS;
        }
        if let (Some(wanted), Some(length)) = (want.seconds, hit.seconds()) {
            let off = (length - wanted).abs() / wanted.max(1.0);
            score += if off <= 0.1 { 20.0 } else if off <= 0.25 { 8.0 } else { 0.0 };
            likely &= off <= 0.25;
        }
        hit.score = score.round() as i32;
        hit.likely = likely;
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.score));
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

fn is_year(word: &str) -> bool {
    word.len() == 4 && word.chars().all(|c| c.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// Seconds into the mix, when the page gives a time.
    pub start: Option<f64>,
    /// "Artist - Title (Remix)", or "ID - ID" for a track nobody named.
    pub name: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tracklist {
    /// The set's name without the DJ and the date: "MiMo Weekly Podcast 044, Greece".
    pub title: String,
    pub dj: Option<String>,
    pub date: Option<String>,
    pub url: String,
    pub tracks: Vec<Track>,
}

impl Tracklist {
    /// The tracks as lines of a pasted list (`H:MM:SS Artist - Title [LABEL]`), which
    /// `nullamp_core::tracklist_text::parse` splits into artist, title and version.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for t in &self.tracks {
            if let Some(start) = t.start {
                let s = start.max(0.0) as u64;
                let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
                out.push_str(&if h > 0 { format!("{h}:{m:02}:{s:02} ") } else { format!("{m}:{s:02} ") });
            }
            out.push_str(&t.name);
            if let Some(label) = &t.label {
                out.push_str(&format!(" [{label}]"));
            }
            out.push('\n');
        }
        out
    }
}

/// Searches the site's tracklists for `query`. Needs no cookie.
pub fn search(query: &str) -> Result<Vec<Hit>, Error> {
    let query = query.trim();
    let form = [("main_search", query), ("search_selection", "9")];
    let (status, body) = request(&format!("{SITE}/search/result.php"), None, Some(&form))?;
    if status != 200 {
        return Err(Error::Refused(status));
    }
    if !body.contains("Tracklists search result") {
        return Err(Error::Reply("The answer is not a search result".into()));
    }
    Ok(parse_hits(&body))
}

/// Reads the tracklist page at `url` (a [`Hit::url`]) with the user's `guid` cookie.
pub fn fetch(guid: &str, url: &str) -> Result<Tracklist, Error> {
    let guid = guid.trim();
    if guid.is_empty() || !guid.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(Error::Guid);
    }
    let url = url.trim();
    if !url.starts_with(&format!("{SITE}/tracklist/")) || url.contains(char::is_whitespace) {
        return Err(Error::Address(url.to_owned()));
    }
    let (status, body) = request(url, Some(guid), None)?;
    if status != 200 {
        return Err(Error::Refused(status));
    }
    parse_tracklist(&body, url)
}

static HIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<div class="bItm action oItm""#).unwrap());
static HIT_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<a href="(/tracklist/[^"]+)"[^>]*>([^<]*)</a>"#).unwrap());
static HIT_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"title="tracklist date"[^>]*><i[^>]*></i>(\d{4}-\d{2}-\d{2})"#).unwrap());
static HIT_TRACKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"title="IDed tracks / total tracks".*?/(\d+)</div>"#).unwrap());
static HIT_TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"title="play time"[^>]*><i[^>]*></i>([^<]*)"#).unwrap());

/// The results of a search page, best match first (the site's order).
pub fn parse_hits(html: &str) -> Vec<Hit> {
    let starts: Vec<usize> = HIT.find_iter(html).map(|m| m.start()).collect();
    let mut hits = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let block = &html[start..starts.get(i + 1).copied().unwrap_or(html.len())];
        let Some(link) = HIT_LINK.captures(block) else { continue };
        let title = decode(&link[2]);
        if title.is_empty() {
            continue;
        }
        hits.push(Hit {
            title,
            url: format!("{SITE}{}", &link[1]),
            date: HIT_DATE.captures(block).map(|c| c[1].to_owned()),
            tracks: HIT_TRACKS.captures(block).and_then(|c| c[1].parse().ok()),
            duration: HIT_TIME.captures(block).map(|c| decode(&c[1])).filter(|d| !d.is_empty()),
            score: 0,
            likely: false,
        });
    }
    hits
}

static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<div id="tlp_\d+" class="[^"]*\btlpItem\b"#).unwrap());
static CUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"_cue_seconds"[^>]*value="([^"]*)""#).unwrap());
static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"itemprop="name" content="([^"]*)""#).unwrap());
static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"class="trackLabel[^"]*">([^<]*)"#).unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<title>([^<]*)").unwrap());
static TRAILING_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*(\d{4}-\d{2}-\d{2})\s*$").unwrap());

/// The tracks and the name of a tracklist page. `url` is where it came from.
pub fn parse_tracklist(html: &str, url: &str) -> Result<Tracklist, Error> {
    let starts: Vec<usize> = ROW.find_iter(html).map(|m| m.start()).collect();
    if starts.is_empty() {
        return Err(Error::Reply("The page lists no tracks".into()));
    }
    let mut tracks = Vec::new();
    let rows: Vec<&str> = starts.iter().enumerate().map(|(i, &start)| &html[start..starts.get(i + 1).copied().unwrap_or(html.len())]).collect();
    let cues: Vec<Option<f64>> = rows.iter().map(|row| CUE.captures(row).and_then(|c| c[1].trim().parse::<f64>().ok()).filter(|s| *s >= 0.0)).collect();
    // The site writes 0 for a track nobody timed, so only the first track really starts at 0, and only
    // when the list has any times at all.
    let timed = cues.iter().any(|c| c.is_some_and(|s| s > 0.0));
    for (i, row) in rows.iter().enumerate() {
        let name = NAME.captures(row).map(|c| decode(&c[1])).filter(|n| !n.is_empty()).unwrap_or_else(|| "ID - ID".into());
        tracks.push(Track {
            start: cues[i].filter(|s| *s > 0.0 || (i == 0 && timed)),
            name,
            label: LABEL.captures(row).map(|c| decode(&c[1])).filter(|l| !l.is_empty()),
        });
    }
    let page_title = TITLE.captures(html).map(|c| decode(&c[1])).unwrap_or_default();
    let (rest, date) = match TRAILING_DATE.captures(&page_title) {
        Some(c) => (page_title[..c.get(0).map_or(0, |m| m.start())].to_owned(), Some(c[1].to_owned())),
        None => (page_title, None),
    };
    let (dj, title) = match rest.split_once(" @ ") {
        Some((dj, event)) => (Some(dj.trim().to_owned()), event.trim().to_owned()),
        None => (None, rest.trim().to_owned()),
    };
    Ok(Tracklist { title, dj: dj.filter(|d| !d.is_empty()), date, url: url.to_owned(), tracks })
}

/// Replaces the HTML entities a page uses in text and attributes.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                _ => {
                    let number = entity.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((ch, end + 1))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// The status and body of `url`, by running curl: a form post when `form` is given. Redirects
/// are followed, but only to `https`.
fn request(url: &str, guid: Option<&str>, form: Option<&[(&str, &str)]>) -> Result<(u16, String), Error> {
    let mut command = Command::new("curl");
    command
        .args(["--silent", "--show-error", "--location"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--connect-timeout", "8", "--max-time", &TIMEOUT_SECS.to_string()])
        .args(["--user-agent", USER_AGENT, "--header", "Accept: text/html,application/xhtml+xml", "--header", "Accept-Language: en-US,en;q=0.9"])
        .args(["--write-out", "\n%{http_code}"]);
    if let Some(guid) = guid {
        command.args(["--header", &format!("Cookie: guid={guid}")]);
    }
    for (name, value) in form.unwrap_or_default() {
        command.args(["--data-urlencode", &format!("{name}={value}")]);
    }
    let mut child = command
        .args(["--url", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::NoCurl } else { Error::Fetch(e.to_string()) })?;
    let mut body = Vec::new();
    let read = child.stdout.take().map(|out| out.take(MAX_BYTES + 1).read_to_end(&mut body));
    if body.len() as u64 > MAX_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Reply("The answer is too long".into()));
    }
    let mut message = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut message);
    }
    let exit = child.wait().map_err(|e| Error::Fetch(e.to_string()))?;
    if !exit.success() {
        return Err(Error::Fetch(curl_message(&message)));
    }
    if let Some(Err(e)) = read {
        return Err(Error::Fetch(e.to_string()));
    }
    let body = String::from_utf8_lossy(&body);
    let (page, status) = body.rsplit_once('\n').ok_or_else(|| Error::Reply("The answer has no status".into()))?;
    let status = status.trim().parse().map_err(|_| Error::Reply("The answer has no status".into()))?;
    Ok((status, page.to_owned()))
}

fn curl_message(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The request failed").trim();
    let line = line.strip_prefix("curl:").map_or(line, str::trim_start);
    let line = line.strip_prefix('(').and_then(|rest| rest.split_once(')')).map_or(line, |(_, rest)| rest.trim_start());
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = r#"<div class="bItmH">Tracklists search result for "x"</div>
<div class="bItm action oItm" onclick="window.open('/tracklist/2ulfu5h9/a.html', '_self');" data-id="2ulfu5h9"><div class="bCont"><div class="bTitle" onclick="cancelBubble();"><a href="/tracklist/2ulfu5h9/miss-monique-atlas-weekend-virtual-festival-ukraine-2020-07-10.html" class="">Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine</a></div></div><div class="mediaRow iRow"><div class="badge views" title="tracklist views" data-count="23.7k"><i class="fa fa-eye spR"></i></div><div title="tracklist date"><i class="fa fa-calendar fa-20 spR"></i>2020-07-10</div><div title="IDed tracks / total tracks"><i class="fa fa-check fa-20 spR"></i><span class="greenTxt">all</span>/11</div><div title="play time"><i class="fa fa-clock-o fa-20 spR"></i>59m </div></div></div>
<div class="bItm action oItm" onclick="window.open('/tracklist/g99dqn9/b.html', '_self');"><div class="bCont"><div class="bTitle"><a href="/tracklist/g99dqn9/danny-grunow.html" class="">Ballentine&#039;s &amp; Co</a></div></div><div class="mediaRow iRow"></div></div>"#;

    fn row(id: u32, cue: &str, name: &str, label: &str) -> String {
        format!(
            r#"<div id="tlp_{id}" class="tlpTog bItm tlpItem trRow1" data-trno="0"><input id="tlp{id}_cue_seconds" type="hidden" value="{cue}" form=frmEditTracklist><div id="tlp0_content" itemprop="tracks"><meta itemprop="name" content="{name}"><meta itemprop="publisher" content="&lt;span class=&quot;trackLabel noWrap&quot;&gt;X&lt;/span&gt;"><span class="iBlock"><span title="label" class="trackLabel noWrap blueTxt">{label}<a href="/label/1">x</a></span></span></div></div>"#
        )
    }

    fn page() -> String {
        let rows = [row(1, "0", "Hayden James &amp; AR/CO - We Could Be Love (Fideles Remix)", "FUTURE CLASSIC"), row(2, "382", "ID - ID", ""), row(3, "4000", "Cherry - Selene", "ALL DAY I DREAM")];
        format!("<html><head><title>Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine 2020-07-10</title></head><body>{}</body></html>", rows.concat())
    }

    #[test]
    fn search_results_are_read() {
        let hits = parse_hits(SEARCH);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine");
        assert_eq!(hits[0].url, "https://www.1001tracklists.com/tracklist/2ulfu5h9/miss-monique-atlas-weekend-virtual-festival-ukraine-2020-07-10.html");
        assert_eq!(hits[0].date.as_deref(), Some("2020-07-10"));
        assert_eq!(hits[0].tracks, Some(11));
        assert_eq!(hits[0].duration.as_deref(), Some("59m"));
        assert_eq!(hits[1].title, "Ballentine's & Co");
        assert_eq!((hits[1].date.as_deref(), hits[1].tracks, hits[1].duration.as_deref()), (None, None, None));
    }

    fn hit(title: &str, date: &str, duration: &str) -> Hit {
        Hit { title: title.into(), url: String::new(), date: Some(date.into()), tracks: None, duration: Some(duration.into()), score: 0, likely: false }
    }

    #[test]
    fn the_year_of_the_tag_decides_between_mixes_of_one_dj() {
        let mut hits = vec![
            hit("Miss Monique @ MiMo Weekly Podcast 044, Greece", "2024-09-20", "1h 20m"),
            hit("Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine", "2020-07-10", "59m"),
            hit("Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine", "2019-07-12", "1h 5m"),
        ];
        rank(&mut hits, &Wanted { query: "Miss Monique Live Atlas Weekend 2020", year: Some(2020), seconds: Some(3540.0) });
        assert_eq!(hits[0].date.as_deref(), Some("2020-07-10"));
        assert!(hits[0].likely);
        assert_eq!(hits[1].date.as_deref(), Some("2019-07-12"));
        assert!(!hits[1].likely, "a year off is not convincing");
        assert!(!hits[2].likely);
    }

    #[test]
    fn the_year_beats_a_better_worded_title_of_another_year() {
        let mut hits = vec![hit("Miss Monique @ Atlas Weekend 2019", "2019-07-12", "1h"), hit("Miss Monique @ Atlas Weekend, Ukraine", "2020-07-10", "1h")];
        rank(&mut hits, &Wanted { query: "Miss Monique Atlas Weekend Ukraine", year: Some(2020), seconds: None });
        assert_eq!(hits[0].year(), Some(2020));
    }

    #[test]
    fn without_a_year_the_words_and_the_length_decide_and_ties_keep_the_site_order() {
        let mut hits = vec![hit("Someone Else @ Somewhere", "2020-01-01", "1h"), hit("Miss Monique @ Atlas Weekend", "2018-01-01", "3h"), hit("Miss Monique @ Atlas Weekend", "2020-07-10", "59m")];
        rank(&mut hits, &Wanted { query: "Miss Monique Atlas Weekend", year: None, seconds: Some(3540.0) });
        assert_eq!(hits[0].date.as_deref(), Some("2020-07-10"));
        assert!(hits[0].likely);
        assert_eq!(hits[1].date.as_deref(), Some("2018-01-01"));
        let mut tied = vec![hit("A", "2020-01-01", "1h"), hit("B", "2020-01-02", "1h")];
        rank(&mut tied, &Wanted::default());
        assert_eq!(tied[0].title, "A");
    }

    #[test]
    fn a_mix_of_the_same_dj_and_year_is_not_a_likely_match_for_another_name() {
        let mut hits = vec![hit("Miss Monique @ Kiss FM Ukraine", "2020-12-17", "1h"), hit("Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine", "2020-07-10", "59m")];
        rank(&mut hits, &Wanted { query: "Miss Monique Live Atlas Weekend 2020", year: Some(2020), seconds: Some(3540.0) });
        assert_eq!(hits[0].title, "Miss Monique @ Atlas Weekend (Virtual Festival), Ukraine");
        assert!(hits[0].likely && !hits[1].likely);
    }

    #[test]
    fn site_durations_are_read() {
        let seconds = |d: &str| hit("", "2020-01-01", d).seconds();
        assert_eq!((seconds("59m"), seconds("1h"), seconds("2h 1m")), (Some(3540.0), Some(3600.0), Some(7260.0)));
        assert_eq!(seconds("soon"), None);
    }

    #[test]
    fn a_page_without_results_has_no_hits() {
        assert!(parse_hits("<html>nothing</html>").is_empty());
    }

    #[test]
    fn a_tracklist_page_is_read() {
        let list = parse_tracklist(&page(), "https://www.1001tracklists.com/tracklist/x/y.html").unwrap();
        assert_eq!(list.title, "Atlas Weekend (Virtual Festival), Ukraine");
        assert_eq!(list.dj.as_deref(), Some("Miss Monique"));
        assert_eq!(list.date.as_deref(), Some("2020-07-10"));
        assert_eq!(list.tracks.len(), 3);
        assert_eq!(list.tracks[0], Track { start: Some(0.0), name: "Hayden James & AR/CO - We Could Be Love (Fideles Remix)".into(), label: Some("FUTURE CLASSIC".into()) });
        assert_eq!(list.tracks[1].name, "ID - ID");
        assert_eq!(list.tracks[1].label, None);
        assert_eq!(list.tracks[2].start, Some(4000.0));
    }

    #[test]
    fn the_text_is_a_pasteable_list() {
        let list = parse_tracklist(&page(), "u").unwrap();
        assert_eq!(list.text(), "0:00 Hayden James & AR/CO - We Could Be Love (Fideles Remix) [FUTURE CLASSIC]\n6:22 ID - ID\n1:06:40 Cherry - Selene [ALL DAY I DREAM]\n");
    }

    #[test]
    fn a_page_without_tracks_is_not_a_tracklist() {
        assert!(matches!(parse_tracklist("<title>1001Tracklists</title>", "u"), Err(Error::Reply(_))));
    }

    /// 1001Tracklists writes 0 for a track without a time; taking it as a start put 15 of 24 tracks of
    /// one list at 0:00.
    #[test]
    fn tracks_without_a_time_have_none() {
        let rows = [row(1, "0", "A - One", ""), row(2, "300", "B - Two", ""), row(3, "0", "C - Three", ""), row(4, "0", "D - Four", ""), row(5, "900", "E - Five", "")];
        let list = parse_tracklist(&format!("<title>X</title>{}", rows.concat()), "u").unwrap();
        assert_eq!(list.tracks.iter().map(|t| t.start).collect::<Vec<_>>(), [Some(0.0), Some(300.0), None, None, Some(900.0)]);
        let rows = [row(1, "0", "A - One", ""), row(2, "0", "B - Two", "")];
        let list = parse_tracklist(&format!("<title>X</title>{}", rows.concat()), "u").unwrap();
        assert_eq!(list.tracks.iter().map(|t| t.start).collect::<Vec<_>>(), [None, None], "a list without any time has none");
    }

    #[test]
    fn a_title_without_dj_stays_whole() {
        let html = format!("<title>Radio Show 12</title>{}", row(1, "", "A - B", ""));
        let list = parse_tracklist(&html, "u").unwrap();
        assert_eq!((list.title.as_str(), list.dj, list.date), ("Radio Show 12", None, None));
        assert_eq!(list.tracks[0].start, None);
    }

    #[test]
    fn entities_are_decoded() {
        assert_eq!(decode("Ballentine&#039;s &amp; Co &#x41; &unknown; & more"), "Ballentine's & Co A &unknown; & more");
    }

    #[test]
    fn the_cookie_only_goes_to_tracklist_pages() {
        assert!(matches!(fetch("", "https://www.1001tracklists.com/tracklist/x/y.html"), Err(Error::Guid)));
        assert!(matches!(fetch("a b", "https://www.1001tracklists.com/tracklist/x/y.html"), Err(Error::Guid)));
        assert!(matches!(fetch("a;b=c", "https://www.1001tracklists.com/tracklist/x/y.html"), Err(Error::Guid)));
        assert!(matches!(fetch("abc123", "https://example.org/tracklist/x"), Err(Error::Address(_))));
        assert!(matches!(fetch("abc123", "https://www.1001tracklists.com.evil.org/tracklist/x"), Err(Error::Address(_))));
        assert!(matches!(fetch("abc123", "http://www.1001tracklists.com/tracklist/x"), Err(Error::Address(_))));
    }

    #[test]
    fn curl_messages_lose_their_prefix() {
        assert_eq!(curl_message("curl: (6) Could not resolve host: example.org"), "Could not resolve host: example.org");
        assert_eq!(curl_message(""), "The request failed");
    }
}
