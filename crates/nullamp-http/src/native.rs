//! The request in-process with ureq and rustls, with the web's root certificates bundled: Android has no curl.

use std::io::Read;

use super::{Body, Error, Request, Response};

pub fn send(request: &Request) -> Result<Response, Error> {
    let fail = |e: ureq::Error| Error::Fetch(message_of(&e));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // Every status is an answer; the caller decides.
        .http_status_as_error(false)
        .https_only(!request.allow_http)
        .max_redirects(if request.follow { 10 } else { 0 })
        .max_redirects_will_error(false)
        .user_agent(request.user_agent)
        .timeout_connect(Some(std::time::Duration::from_secs(8)))
        .timeout_global(Some(std::time::Duration::from_secs(request.timeout_secs.into())))
        .build()
        .into();
    let mut response = match request.body {
        Body::None => {
            let mut call = agent.get(request.url);
            for (name, value) in request.headers {
                call = call.header(*name, *value);
            }
            call.call()
        }
        Body::Form(fields) => {
            let mut call = agent.post(request.url);
            for (name, value) in request.headers {
                call = call.header(*name, *value);
            }
            call.send_form(fields.iter().copied())
        }
        Body::Raw(bytes) => {
            let mut call = agent.post(request.url);
            for (name, value) in request.headers {
                call = call.header(*name, *value);
            }
            call.send(bytes)
        }
    }
    .map_err(fail)?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response.body_mut().as_reader().take(request.max_bytes + 1).read_to_end(&mut body).map_err(|e| Error::Fetch(e.to_string()))?;
    if body.len() as u64 > request.max_bytes {
        return Err(Error::TooLong);
    }
    Ok(Response { status, body })
}

/// Like curl's, the message without the name of the library.
fn message_of(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "Timeout was reached".to_owned(),
        other => other.to_string(),
    }
}
