//! Is there a newer release? Asks a web address and compares versions.
//!
//! The address (the *source*) is the program's to keep, in its settings, so
//! it can be pointed somewhere else if the place it was published at goes
//! away. It answers with JSON in one of two shapes:
//!
//! - GitHub's "latest release" (`https://api.github.com/repos/<owner>/<repo>/releases/latest`):
//!   `tag_name` is the version, `html_url` the page to download it from.
//! - A file of its own, on any server: `{"version": "1.5.0", "url": "https://example.org/download"}`.
//!   `url` is optional.
//!
//! The page is fetched with `curl`, which macOS ships and most Linux
//! installs have, so this crate needs no TLS stack. Nothing here contacts
//! anyone by itself: the program calls [`check`] when the user asks to.
//! [`check`] blocks for up to [`TIMEOUT_SECS`] seconds, so call it from a
//! worker thread.

use std::fmt;
use std::io::Read;
use std::process::{Command, Stdio};

use serde_json::Value;

mod version;

pub use version::Version;

/// How long the whole request may take.
pub const TIMEOUT_SECS: u32 = 15;
/// Longer answers are refused; a release document is a few kilobytes.
const MAX_BYTES: u64 = 1 << 20;

/// What the source says about the newest release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The running version is the newest or newer (a development build).
    UpToDate { latest: String },
    Available(Release),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The version as published, without a leading `v`.
    pub version: String,
    /// Where to get it: always an `http` or `https` address, so a program
    /// can open it without looking.
    pub page: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The source isn't an `http(s)` address.
    Source(String),
    /// `curl` is not installed.
    NoCurl,
    /// The request failed: no network, a server error, a timeout. Carries curl's message.
    Fetch(String),
    /// The answer isn't a release document, or its version can't be read.
    Reply(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Source(source) => write!(f, "\"{source}\" is not a web address"),
            Error::NoCurl => f.write_str("curl is not installed"),
            Error::Fetch(why) => f.write_str(why),
            Error::Reply(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// Asks `source` for the newest release and compares it with `current`.
/// `program` names the caller (`"name/1.2.3"`) in the request.
pub fn check(source: &str, current: &str, program: &str) -> Result<Status, Error> {
    let source = source.trim();
    if !is_web_address(source) {
        return Err(Error::Source(source.to_owned()));
    }
    parse(&fetch(source, program)?, current)
}

/// Reads the source's answer. Separate from [`check`] so it can be tested
/// without a network.
pub fn parse(body: &str, current: &str) -> Result<Status, Error> {
    let reply = |why: &str| Error::Reply(why.to_owned());
    let json: Value = serde_json::from_str(body).map_err(|_| reply("The answer is not JSON"))?;
    let text = |keys: &[&str]| keys.iter().find_map(|k| json.get(k)?.as_str()).map(str::trim).filter(|t| !t.is_empty());
    let published = text(&["tag_name", "version"]).ok_or_else(|| reply("The answer names no version"))?;
    let latest = Version::parse(published).ok_or_else(|| Error::Reply(format!("\"{published}\" is not a version")))?;
    let running = Version::parse(current).ok_or_else(|| Error::Reply(format!("The running version \"{current}\" is not a version")))?;
    let version = published.trim_start_matches(['v', 'V']).to_owned();
    if latest > running {
        let page = text(&["html_url", "url"]).filter(|p| is_web_address(p)).map(str::to_owned);
        Ok(Status::Available(Release { version, page }))
    } else {
        Ok(Status::UpToDate { latest: version })
    }
}

fn is_web_address(text: &str) -> bool {
    let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && !text.contains(char::is_whitespace)
}

/// The body at `url`, by running curl. Redirects are followed, but only to
/// `https`.
fn fetch(url: &str, program: &str) -> Result<String, Error> {
    let mut child = Command::new("curl")
        .args(["--silent", "--show-error", "--fail", "--location"])
        .args(["--proto", "=http,https", "--proto-redir", "=https"])
        .args(["--connect-timeout", "8", "--max-time", &TIMEOUT_SECS.to_string()])
        .args(["--header", "Accept: application/vnd.github+json", "--user-agent", program, "--url", url])
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
    Ok(String::from_utf8_lossy(&body).into_owned())
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

    const GITHUB: &str = r#"{"tag_name": "v1.5.0", "name": "Nullamp 1.5.0", "html_url": "https://github.com/o/r/releases/tag/v1.5.0", "assets": []}"#;

    #[test]
    fn a_newer_github_release_is_available() {
        let release = Release { version: "1.5.0".into(), page: Some("https://github.com/o/r/releases/tag/v1.5.0".into()) };
        assert_eq!(parse(GITHUB, "1.4.1"), Ok(Status::Available(release)));
    }

    #[test]
    fn the_same_or_a_newer_running_version_is_up_to_date() {
        assert_eq!(parse(GITHUB, "1.5.0"), Ok(Status::UpToDate { latest: "1.5.0".into() }));
        assert_eq!(parse(GITHUB, "1.6.0-dev"), Ok(Status::UpToDate { latest: "1.5.0".into() }));
    }

    #[test]
    fn a_plain_file_works_too() {
        let body = r#"{"version": "2.0", "url": "https://example.org/get"}"#;
        let release = Release { version: "2.0".into(), page: Some("https://example.org/get".into()) };
        assert_eq!(parse(body, "1.9.9"), Ok(Status::Available(release)));
        assert_eq!(parse(r#"{"version": "2.0"}"#, "1.0"), Ok(Status::Available(Release { version: "2.0".into(), page: None })));
    }

    #[test]
    fn a_page_that_is_not_a_web_address_is_dropped() {
        let body = r#"{"version": "2.0", "url": "file:///Applications/Evil.app"}"#;
        assert_eq!(parse(body, "1.0"), Ok(Status::Available(Release { version: "2.0".into(), page: None })));
    }

    #[test]
    fn answers_that_are_not_release_documents_are_errors() {
        assert!(matches!(parse("<html>", "1.0"), Err(Error::Reply(_))));
        assert!(matches!(parse("{}", "1.0"), Err(Error::Reply(_))));
        assert!(matches!(parse(r#"{"tag_name": "nightly"}"#, "1.0"), Err(Error::Reply(_))));
        assert!(matches!(parse(GITHUB, "unknown"), Err(Error::Reply(_))));
    }

    #[test]
    fn only_web_addresses_are_asked() {
        for source in ["", "file:///etc/passwd", "ftp://example.org/x", "-o /tmp/x", "https://exa mple.org"] {
            assert_eq!(check(source, "1.0", "test/1.0"), Err(Error::Source(source.to_owned())));
        }
        assert!(is_web_address("HTTPS://example.org/latest.json"));
    }

    /// Needs the network and a published release.
    #[test]
    #[ignore = "asks GitHub; run with --ignored"]
    fn github_answers() {
        let status = check("https://api.github.com/repos/jbrunemann/nullamp/releases/latest", "0.0.1", "update-check-test/0").unwrap();
        assert!(matches!(status, Status::Available(Release { page: Some(_), .. })), "{status:?}");
        let error = check("https://api.github.com/repos/jbrunemann/no-such-repo/releases/latest", "0.0.1", "update-check-test/0").unwrap_err();
        assert!(matches!(&error, Error::Fetch(m) if m.contains("404")), "{error:?}");
    }

    #[test]
    fn curl_messages_lose_their_prefix() {
        assert_eq!(curl_message("curl: (22) The requested URL returned error: 403\n"), "The requested URL returned error: 403");
        assert_eq!(curl_message("curl: (6) Could not resolve host: example.org"), "Could not resolve host: example.org");
        assert_eq!(curl_message(""), "The request failed");
    }
}
