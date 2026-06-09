//! HTTP/HTTPS REST client.
//!
//! Talks to `red`'s HTTP listener (`POST /query` with a JSON body).
//! Bearer auth via the `Authorization` header. HTTPS uses rustls
//! through ureq's `rustls` feature; certificate validation is on by
//! default, no implicit "skip verify".
//!
//! ureq is synchronous; the bin offloads each call onto
//! `tokio::task::spawn_blocking` so the current-thread runtime
//! stays responsive.

use std::fmt;

use reddb_wire::auth::{bearer_authorization_value, AUTHORIZATION_HEADER};

#[derive(Debug, Clone)]
pub enum Auth {
    Anonymous,
    Bearer(String),
}

#[derive(Debug)]
pub enum HttpError {
    Network(String),
    Http { status: u16, body: String },
    Decode(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "network: {m}"),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Decode(m) => write!(f, "decode: {m}"),
        }
    }
}

impl std::error::Error for HttpError {}

type Result<T> = std::result::Result<T, HttpError>;

/// Single-shot query: POST `<base_url>/query` with a JSON body and
/// return the response body as a string. The caller decides what to
/// do with the response shape (typically pretty-print it).
///
/// Synchronous: callers from an async context should wrap in
/// `tokio::task::spawn_blocking`.
pub fn query_one_shot(base_url: &str, sql: &str, auth: &Auth) -> Result<String> {
    let url = format!("{}/query", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "query": sql }).to_string();
    let mut req = ureq::post(&url).header("content-type", "application/json");
    if let Auth::Bearer(token) = auth {
        req = req.header(AUTHORIZATION_HEADER, &bearer_authorization_value(token));
    }
    let mut resp = req
        .send(body.as_bytes())
        .map_err(|e| HttpError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body_text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| HttpError::Decode(e.to_string()))?;
    if status >= 400 {
        return Err(HttpError::Http {
            status,
            body: body_text,
        });
    }
    Ok(body_text)
}
