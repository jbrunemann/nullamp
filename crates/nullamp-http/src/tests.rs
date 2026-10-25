use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use super::*;

/// A web server on this machine that answers by path: `/ok`, `/echo` (sends the request back), `/missing` (404),
/// `/moved` (302 to `/ok`) and `/big` (100 bytes).
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(|| answer(stream));
        }
    });
    address
}

fn answer(mut stream: TcpStream) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        request.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&request).into_owned();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            if n == 0 { return } else { continue }
        };
        let length = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
        if body.len() < length && n > 0 {
            continue;
        }
        let path = head.split_whitespace().nth(1).unwrap_or("/");
        let (status, extra, payload) = match path {
            "/ok" => ("200 OK", "", b"hello".to_vec()),
            "/echo" => ("200 OK", "", text.into_bytes()),
            "/missing" => ("404 Not Found", "", b"nope".to_vec()),
            "/moved" => ("302 Found", "Location: /ok\r\n", Vec::new()),
            "/big" => ("200 OK", "", vec![b'x'; 100]),
            _ => ("500 Internal Server Error", "", Vec::new()),
        };
        let _ = write!(stream, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
        let _ = stream.write_all(&payload);
        return;
    }
}

type Send = fn(&Request) -> Result<Response, Error>;

/// The ways to make a request on this machine.
fn backends() -> Vec<(&'static str, Send)> {
    #[cfg_attr(not(feature = "rustls"), allow(unused_mut))]
    let mut all: Vec<(&'static str, Send)> = vec![("curl", curl::send)];
    #[cfg(feature = "rustls")]
    all.push(("ureq", native::send));
    all
}

#[test]
fn a_get_returns_status_and_body() {
    let base = serve();
    for (name, send) in backends() {
        let r = send(&Request::get(&format!("{base}/ok"), "test/1").allow_http()).unwrap();
        assert_eq!((r.status, r.text()), (200, "hello".to_owned()), "{name}");
    }
}

#[test]
fn an_error_status_is_an_answer_until_ok_is_asked() {
    let base = serve();
    for (name, send) in backends() {
        let r = send(&Request::get(&format!("{base}/missing"), "test/1").allow_http()).unwrap();
        assert_eq!((r.status, r.text()), (404, "nope".to_owned()), "{name}");
        assert_eq!(r.ok(), Err(Error::Fetch("The requested URL returned error: 404".into())), "{name}");
    }
}

#[test]
fn headers_user_agent_and_form_are_sent() {
    let base = serve();
    for (name, send) in backends() {
        let fields = [("q", "a b&c"), ("n", "1")];
        let r = send(&Request::get(&format!("{base}/echo"), "test/1").allow_http().headers(&[("Cookie", "guid=abc"), ("X-One", "1")]).body(Body::Form(&fields))).unwrap();
        let echoed = r.text().to_ascii_lowercase();
        assert!(echoed.starts_with("post /echo"), "{name}: {echoed}");
        assert!(echoed.contains("user-agent: test/1"), "{name}");
        assert!(echoed.contains("cookie: guid=abc"), "{name}");
        assert!(echoed.contains("x-one: 1"), "{name}");
        assert!(echoed.ends_with("q=a%20b%26c&n=1") || echoed.ends_with("q=a+b%26c&n=1"), "{name}: {echoed}");
    }
}

#[test]
fn a_raw_body_is_posted_with_its_content_type() {
    let base = serve();
    for (name, send) in backends() {
        let r = send(&Request::get(&format!("{base}/echo"), "test/1").allow_http().headers(&[("Content-Type", "application/json")]).body(Body::Raw(br#"{"a":1}"#))).unwrap();
        let echoed = r.text().to_ascii_lowercase();
        assert!(echoed.starts_with("post /echo"), "{name}");
        assert!(echoed.contains("content-type: application/json"), "{name}");
        assert!(echoed.ends_with(r#"{"a":1}"#), "{name}: {echoed}");
    }
}

#[test]
fn a_redirect_can_be_left_alone() {
    let base = serve();
    for (name, send) in backends() {
        let r = send(&Request::get(&format!("{base}/moved"), "test/1").allow_http().follow(false)).unwrap();
        assert_eq!(r.status, 302, "{name}");
    }
}

#[test]
fn a_long_answer_is_refused() {
    let base = serve();
    for (name, send) in backends() {
        let r = send(&Request::get(&format!("{base}/big"), "test/1").allow_http().max_bytes(10));
        assert_eq!(r, Err(Error::TooLong), "{name}");
        let r = send(&Request::get(&format!("{base}/big"), "test/1").allow_http().max_bytes(100)).unwrap();
        assert_eq!(r.body.len(), 100, "{name}");
    }
}

#[test]
fn plain_http_needs_to_be_allowed() {
    let base = serve();
    for (name, send) in backends() {
        assert!(send(&Request::get(&format!("{base}/ok"), "test/1")).is_err(), "{name}");
    }
}

#[test]
fn a_missing_curl_is_named() {
    let r = curl::send_with("curl-that-is-not-installed", &Request::get("https://example.org", "test/1"));
    assert_eq!(r, Err(Error::NoCurl));
}

#[test]
fn curls_message_is_kept_short() {
    assert_eq!(curl::message_of("curl: (6) Could not resolve host: example.org"), "Could not resolve host: example.org");
    assert_eq!(curl::message_of("curl: (22) The requested URL returned error: 404\n"), "The requested URL returned error: 404");
    assert_eq!(curl::message_of(""), "The request failed");
}

/// Needs the internet: a real https answer, which proves the bundled root certificates and the TLS handshake.
#[cfg(feature = "rustls")]
#[test]
#[ignore]
fn ureq_reaches_a_real_https_server() {
    let r = native::send(&Request::get("https://api.deezer.com/search?q=daft%20punk&limit=1", "nullamp-test/1")).unwrap().ok().unwrap();
    assert!(r.text().contains("\"data\""), "{}", r.text());
}
