//! The web requests of the online features (update check, cover lookup, Shazam, 1001Tracklists).
//!
//! Each used to run `curl` itself. They share this crate now so there is one place that knows how: [`send`] runs
//! `curl`, which macOS ships and most Linux installs have, so the program needs no TLS stack. Android has no curl;
//! with the `rustls` feature (the app turns it on there) the same request is made in-process with `ureq`.
//!
//! [`send`] blocks for up to [`Request::timeout_secs`], so call it from a worker thread.

#[cfg(not(target_os = "android"))]
mod curl;
// Only Android calls it outside the tests.
#[cfg(feature = "rustls")]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod native;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("curl is not installed")]
    NoCurl,
    /// No network, a server error, a timeout. Carries curl's message, or what ureq says.
    #[error("{0}")]
    Fetch(String),
    /// The answer is longer than [`Request::max_bytes`].
    #[error("The answer is too long")]
    TooLong,
}

/// What to send with the request.
#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    /// A GET.
    None,
    /// A POST of a URL-encoded form.
    Form(&'a [(&'a str, &'a str)]),
    /// A POST of these bytes; say what they are in the `Content-Type` header.
    Raw(&'a [u8]),
}

#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub url: &'a str,
    pub user_agent: &'a str,
    /// Extra headers, name and value.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Body<'a>,
    /// Follow redirects, but only to `https`.
    pub follow: bool,
    /// Also talk to plain `http` addresses, for a source the user pointed somewhere else.
    pub allow_http: bool,
    /// Longer answers are refused with [`Error::TooLong`].
    pub max_bytes: u64,
    pub timeout_secs: u32,
}

impl<'a> Request<'a> {
    /// A GET of `url` over `https`, following redirects, with a 1 MB limit and 15 seconds.
    pub fn get(url: &'a str, user_agent: &'a str) -> Self {
        Request { url, user_agent, headers: &[], body: Body::None, follow: true, allow_http: false, max_bytes: 1 << 20, timeout_secs: 15 }
    }

    pub fn headers(mut self, headers: &'a [(&'a str, &'a str)]) -> Self {
        self.headers = headers;
        self
    }

    pub fn body(mut self, body: Body<'a>) -> Self {
        self.body = body;
        self
    }

    pub fn follow(mut self, follow: bool) -> Self {
        self.follow = follow;
        self
    }

    pub fn allow_http(mut self) -> Self {
        self.allow_http = true;
        self
    }

    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn timeout_secs(mut self, secs: u32) -> Self {
        self.timeout_secs = secs;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    /// The response itself when the status is a success or a redirect, else the error curl's `--fail` raised.
    pub fn ok(self) -> Result<Response, Error> {
        if self.status >= 400 { Err(Error::Fetch(format!("The requested URL returned error: {}", self.status))) } else { Ok(self) }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Make the request. Any HTTP status is an answer, so callers that want curl's `--fail` use [`Response::ok`].
pub fn send(request: &Request) -> Result<Response, Error> {
    #[cfg(target_os = "android")]
    return native::send(request);
    #[cfg(not(target_os = "android"))]
    return curl::send(request);
}

#[cfg(all(target_os = "android", not(feature = "rustls")))]
compile_error!("Android has no curl: build nullamp-http with the `rustls` feature");

#[cfg(test)]
mod tests;
