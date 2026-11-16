//! A small blocking HTTP client (ureq + rustls/ring).
//!
//! `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` are honoured through ureq's defaults; `socks5://` proxies
//! work too. Nothing here is trusted: every byte that matters is verified later against the pinned signing key.

use crate::error::{Error, Result};
use std::io::Read;
use std::time::Duration;

/// Only requests to this prefix carry `GITHUB_TOKEN` (never an asset host, never a mirror).
const GITHUB_API_PREFIX: &str = "https://api.github.com/";

/// A response body that is still being read.
pub(crate) struct Download {
    pub reader: Box<dyn Read>,
    /// From `Content-Length`; `None` for chunked or close-delimited replies.
    pub content_length: Option<u64>,
}

pub(crate) struct Http {
    agent: ureq::Agent,
    github_token: Option<String>,
}

impl Http {
    /// `timeout` bounds connecting and every single read, so a stalled transfer fails while a long, steady one
    /// (a 120 MB archive) does not. Zero disables it.
    pub fn new(timeout: Duration) -> Self {
        let t = (!timeout.is_zero()).then_some(timeout);
        let config = ureq::Agent::config_builder()
            .user_agent(concat!(
                "citro-install/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/afftroen/Citroenition)"
            ))
            // Plain bytes only: the digest is over exactly what the server stores, not over a transfer encoding.
            .accept_encoding("identity")
            .timeout_resolve(t)
            .timeout_connect(t)
            .timeout_send_request(t)
            .timeout_recv_response(t)
            .timeout_per_read(t)
            .max_redirects(8)
            .build();
        let github_token = std::env::var("GITHUB_TOKEN").ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        Self { agent: ureq::Agent::new_with_config(config), github_token }
    }

    fn call(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut req = self.agent.get(url);
        if url.starts_with(GITHUB_API_PREFIX) {
            req = req.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(token) = &self.github_token {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
        }
        req.call().map_err(|e| map_error(url, e))
    }

    /// The whole body, refusing more than `limit` bytes.
    pub fn get_bytes(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut response = self.call(url)?;
        response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| map_error(url, e))
    }

    /// The body as a stream (unbounded: the caller counts).
    pub fn open(&self, url: &str) -> Result<Download> {
        let response = self.call(url)?;
        let content_length = response.body().content_length();
        Ok(Download { reader: Box::new(response.into_body().into_reader()), content_length })
    }
}

/// `https://user:pass@host/x` -> `https://host/x`: error messages and logs must not carry credentials.
pub(crate) fn display_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if let Some(at) = rest[..authority_end].rfind('@') {
            return format!("{scheme}://{}", &rest[at + 1..]);
        }
    }
    url.to_owned()
}

fn map_error(url: &str, e: ureq::Error) -> Error {
    let url = display_url(url);
    match e {
        ureq::Error::StatusCode(status) => Error::HttpStatus { url, status },
        ureq::Error::BodyExceedsLimit(n) => {
            Error::BadResponse { url, message: format!("the body is larger than the {n} bytes expected here") }
        }
        other => Error::Network { url, message: other.to_string() },
    }
}

/// A read error from a body stream, as ours.
pub(crate) fn read_error(url: &str, e: &std::io::Error) -> Error {
    Error::Network { url: display_url(url), message: e.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_reach_error_messages() {
        assert_eq!(display_url("https://u:p%40ss@mirror.example/a/b?x=1"), "https://mirror.example/a/b?x=1");
        assert_eq!(display_url("http://tok@127.0.0.1:8765"), "http://127.0.0.1:8765");
        assert_eq!(display_url("https://example.com/a@b"), "https://example.com/a@b");
        assert_eq!(display_url("not a url"), "not a url");
    }
}
