//! The request by running `curl`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use super::{Body, Error, Request, Response};

pub fn send(request: &Request) -> Result<Response, Error> {
    send_with("curl", request)
}

/// `send` with the program named, so a test can point it at a program that is not there.
pub(crate) fn send_with(program: &str, request: &Request) -> Result<Response, Error> {
    let mut command = Command::new(program);
    command.args(["--silent", "--show-error"]);
    command.args(["--proto", if request.allow_http { "=http,https" } else { "=https" }]);
    if request.follow {
        command.args(["--location", "--proto-redir", "=https"]);
    }
    command
        .args(["--connect-timeout", "8", "--max-time", &request.timeout_secs.to_string()])
        .args(["--user-agent", request.user_agent])
        // The status follows the body, on a line of its own.
        .args(["--write-out", "\n%{http_code}"]);
    for (name, value) in request.headers {
        command.args(["--header", &format!("{name}: {value}")]);
    }
    match request.body {
        Body::None => {}
        Body::Form(fields) => {
            for (name, value) in fields {
                command.args(["--data-urlencode", &format!("{name}={value}")]);
            }
        }
        Body::Raw(_) => {
            command.args(["--data-binary", "@-"]);
        }
    }
    let raw = matches!(request.body, Body::Raw(_));
    let mut child = command
        .args(["--url", request.url])
        .stdin(if raw { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Error::NoCurl } else { Error::Fetch(e.to_string()) })?;
    if let (Body::Raw(bytes), Some(mut stdin)) = (request.body, child.stdin.take()) {
        stdin.write_all(bytes).map_err(|e| Error::Fetch(e.to_string()))?;
    }
    // Room for the status line after a body of the longest allowed size.
    let limit = request.max_bytes + 16;
    let mut out = Vec::new();
    let read = child.stdout.take().map(|o| o.take(limit + 1).read_to_end(&mut out));
    if out.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::TooLong);
    }
    let mut message = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut message);
    }
    let exit = child.wait().map_err(|e| Error::Fetch(e.to_string()))?;
    if !exit.success() {
        return Err(Error::Fetch(message_of(&message)));
    }
    if let Some(Err(e)) = read {
        return Err(Error::Fetch(e.to_string()));
    }
    let at = out.iter().rposition(|b| *b == b'\n').ok_or_else(|| Error::Fetch("The answer has no status".into()))?;
    let status = std::str::from_utf8(&out[at + 1..]).ok().and_then(|s| s.trim().parse().ok()).ok_or_else(|| Error::Fetch("The answer has no status".into()))?;
    out.truncate(at);
    if out.len() as u64 > request.max_bytes {
        return Err(Error::TooLong);
    }
    Ok(Response { status, body: out })
}

/// curl's last line without its `curl: (22) ` prefix.
pub(crate) fn message_of(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The request failed").trim();
    let line = line.strip_prefix("curl:").map_or(line, str::trim_start);
    let line = line.strip_prefix('(').and_then(|rest| rest.split_once(')')).map_or(line, |(_, rest)| rest.trim_start());
    line.to_owned()
}
