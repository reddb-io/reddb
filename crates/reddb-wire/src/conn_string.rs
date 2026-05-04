//! Connection-string parser shared across `reddb`, `reddb-client`,
//! `red_client`, and every language driver.
//!
//! Pure function over a string; no I/O, no allocation beyond what the
//! returned [`ConnectionTarget`] needs. The grammar is defined by
//! `docs/clients/connection-strings.md`; this module is the canonical
//! parser and is the single source of truth consumed by the rest of
//! the workspace.
//!
//! The parser ports the logic that previously lived in
//! `drivers/rust/src/connect.rs` (which keeps a thin re-export layer
//! for backwards compatibility while drivers migrate over). Cluster
//! URIs (`grpc://primary,replica:port`), default ports per scheme,
//! and the `?route=primary` override behave identically to the
//! original.

use std::path::PathBuf;

use url::Url;

/// Stable error code for parser failures.
///
/// Mirrors the `ErrorCode` shape used by the language drivers so that
/// downstream wrappers can map 1:1 without information loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// The input was empty.
    Empty,
    /// `url::Url` rejected the string, or a transport-specific
    /// invariant (missing host, empty cluster entry, bad port…) was
    /// violated.
    InvalidUri,
    /// The scheme is not in the documented vocabulary.
    UnsupportedScheme,
}

impl ParseErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ParseErrorKind::Empty => "EMPTY",
            ParseErrorKind::InvalidUri => "INVALID_URI",
            ParseErrorKind::UnsupportedScheme => "UNSUPPORTED_SCHEME",
        }
    }
}

/// Error returned by [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub message: String,
}

impl ParseError {
    pub fn new(kind: ParseErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for ParseError {}

/// Default port per documented scheme. Centralised so other crates
/// (the connector, server-side dispatch) can stay consistent.
pub const DEFAULT_PORT_RED: u16 = 5050;
pub const DEFAULT_PORT_GRPC: u16 = 5055;

/// Normalised target produced by [`parse`].
///
/// Variants intentionally mirror the existing `drivers/rust` `Target`
/// shape so the future consolidation slice is a re-export, not a
/// behaviour change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
    /// `memory://` — ephemeral, in-memory backend.
    Memory,
    /// `file:///abs/path` — embedded engine on disk.
    File { path: PathBuf },
    /// Single remote endpoint over `red://`, `reds://`, `grpc://`, or
    /// `grpcs://`. Stored as a normalised `http://host:port` string
    /// because tonic's `Endpoint` consumes that form.
    Grpc { endpoint: String },
    /// Multi-host gRPC URI: primary + read replicas. Writes hit the
    /// primary; reads round-robin across replicas unless
    /// `force_primary` is set.
    GrpcCluster {
        primary: String,
        replicas: Vec<String>,
        force_primary: bool,
    },
    /// `http://host:port` / `https://host:port` — REST endpoint.
    Http { base_url: String },
}

/// Parse a connection URI into a [`ConnectionTarget`].
///
/// Pure function, no side effects. Behaviour matches
/// `drivers/rust/src/connect.rs::parse` 1:1.
pub fn parse(uri: &str) -> Result<ConnectionTarget, ParseError> {
    if uri.is_empty() {
        return Err(ParseError::new(
            ParseErrorKind::Empty,
            "empty connection string",
        ));
    }

    if uri == "memory://" || uri == "memory:" {
        return Ok(ConnectionTarget::Memory);
    }

    if let Some(rest) = uri.strip_prefix("file://") {
        if rest.is_empty() {
            return Err(ParseError::new(
                ParseErrorKind::InvalidUri,
                "file:// URI is missing a path",
            ));
        }
        return Ok(ConnectionTarget::File {
            path: PathBuf::from(rest),
        });
    }

    if let Some(cluster) = try_parse_grpc_cluster(uri)? {
        return Ok(cluster);
    }

    let parsed = Url::parse(uri)
        .map_err(|e| ParseError::new(ParseErrorKind::InvalidUri, format!("{e}: {uri}")))?;

    match parsed.scheme() {
        "red" | "reds" => {
            let host = parsed.host_str().ok_or_else(|| {
                ParseError::new(ParseErrorKind::InvalidUri, "red:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(DEFAULT_PORT_RED);
            Ok(ConnectionTarget::Grpc {
                endpoint: format!("http://{host}:{port}"),
            })
        }
        "grpc" | "grpcs" => {
            let host = parsed.host_str().ok_or_else(|| {
                ParseError::new(ParseErrorKind::InvalidUri, "grpc:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(DEFAULT_PORT_GRPC);
            Ok(ConnectionTarget::Grpc {
                endpoint: format!("http://{host}:{port}"),
            })
        }
        "http" | "https" => {
            let host = parsed.host_str().ok_or_else(|| {
                ParseError::new(
                    ParseErrorKind::InvalidUri,
                    "http(s):// URI is missing a host",
                )
            })?;
            let scheme = parsed.scheme();
            let port = parsed
                .port()
                .unwrap_or(if scheme == "https" { 443 } else { 80 });
            Ok(ConnectionTarget::Http {
                base_url: format!("{scheme}://{host}:{port}"),
            })
        }
        other => Err(ParseError::new(
            ParseErrorKind::UnsupportedScheme,
            format!("unsupported scheme: {other}"),
        )),
    }
}

/// Try to parse a multi-host gRPC URI. `Ok(None)` means "this is a
/// single-host URI — fall through to the standard parser".
fn try_parse_grpc_cluster(uri: &str) -> Result<Option<ConnectionTarget>, ParseError> {
    let (rest, default_port) = if let Some(r) = uri.strip_prefix("grpc://") {
        (r, DEFAULT_PORT_GRPC)
    } else if let Some(r) = uri
        .strip_prefix("red://")
        .or_else(|| uri.strip_prefix("reds://"))
    {
        (r, DEFAULT_PORT_RED)
    } else {
        return Ok(None);
    };

    let (host_part, query_part) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };

    if !host_part.contains(',') {
        return Ok(None);
    }

    let mut endpoints: Vec<String> = Vec::new();
    for raw in host_part.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ParseError::new(
                ParseErrorKind::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| {
                    ParseError::new(
                        ParseErrorKind::InvalidUri,
                        format!("invalid port in cluster URI: {raw}"),
                    )
                })?;
                (h, port)
            }
            None => (raw, default_port),
        };
        if host.is_empty() {
            return Err(ParseError::new(
                ParseErrorKind::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        endpoints.push(format!("http://{host}:{port}"));
    }

    let force_primary = query_part
        .map(|q| {
            q.split('&').any(|kv| {
                let mut parts = kv.splitn(2, '=');
                let k = parts.next().unwrap_or("");
                let v = parts.next().unwrap_or("");
                k.eq_ignore_ascii_case("route") && v.eq_ignore_ascii_case("primary")
            })
        })
        .unwrap_or(false);

    let mut iter = endpoints.into_iter();
    let primary = iter.next().expect("split on ',' yields at least one entry");
    let replicas: Vec<String> = iter.collect();

    Ok(Some(ConnectionTarget::GrpcCluster {
        primary,
        replicas,
        force_primary,
    }))
}
