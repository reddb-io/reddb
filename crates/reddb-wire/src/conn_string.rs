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
    /// A DoS guardrail in [`ConnStringLimits`] was tripped.
    /// `message` carries the limit name + the offending value so
    /// downstream wrappers can surface the structured detail.
    LimitExceeded,
}

impl ParseErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ParseErrorKind::Empty => "EMPTY",
            ParseErrorKind::InvalidUri => "INVALID_URI",
            ParseErrorKind::UnsupportedScheme => "UNSUPPORTED_SCHEME",
            ParseErrorKind::LimitExceeded => "LIMIT_EXCEEDED",
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

/// DoS guardrails applied by [`parse`] before any URI work happens.
///
/// The connection-string parser is the only entry point an attacker
/// can reach BEFORE auth, so every limit here is enforced eagerly
/// and surfaces as a structured [`ParseErrorKind::LimitExceeded`]
/// error rather than a panic, hang, or unbounded allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnStringLimits {
    /// Maximum length of the input URI in bytes. Default `8 KiB`.
    pub max_uri_bytes: usize,
    /// Maximum number of `key=value` query parameters. Default `32`.
    pub max_query_params: usize,
    /// Maximum number of comma-separated cluster hosts allowed in a
    /// `red://`/`reds://`/`grpc://` cluster URI. Default `64`.
    pub max_cluster_hosts: usize,
}

impl Default for ConnStringLimits {
    fn default() -> Self {
        Self {
            max_uri_bytes: 8 * 1024,
            max_query_params: 32,
            max_cluster_hosts: 64,
        }
    }
}

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
    /// Single remote endpoint over `grpc://` or `grpcs://`. Stored
    /// as a normalised `http://host:port` string because tonic's
    /// `Endpoint` consumes that form.
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
    /// `red://host:port` (plain TCP) or `reds://host:port` (TLS).
    /// RedWire binary frame protocol per ADR 0001. The connector
    /// speaks framed binary directly; it does NOT route through
    /// tonic.
    RedWire {
        host: String,
        port: u16,
        tls: bool,
    },
}

/// Parse a connection URI into a [`ConnectionTarget`] under the
/// default DoS limits.
///
/// Pure function, no side effects. Behaviour matches
/// `drivers/rust/src/connect.rs::parse` 1:1 with two additions:
///   - Mixed-case schemes (e.g. `Red://`, `REDS://`) are normalised
///     to lowercase before dispatch.
///   - Inputs exceeding [`ConnStringLimits`] return a structured
///     [`ParseErrorKind::LimitExceeded`] error instead of being
///     processed.
pub fn parse(uri: &str) -> Result<ConnectionTarget, ParseError> {
    parse_with_limits(uri, ConnStringLimits::default())
}

/// Same as [`parse`] but with caller-supplied DoS guardrails.
/// Useful for tests that need tighter limits or for callers (a
/// future admin tool, an offline validator) that need to relax the
/// defaults.
pub fn parse_with_limits(
    uri: &str,
    limits: ConnStringLimits,
) -> Result<ConnectionTarget, ParseError> {
    if uri.is_empty() {
        return Err(ParseError::new(
            ParseErrorKind::Empty,
            "empty connection string",
        ));
    }

    if uri.len() > limits.max_uri_bytes {
        return Err(ParseError::new(
            ParseErrorKind::LimitExceeded,
            format!(
                "max_uri_bytes exceeded: limit={} actual={}",
                limits.max_uri_bytes,
                uri.len(),
            ),
        ));
    }

    // Lowercase the scheme so `Red://Host`, `REDS://Host`, etc.
    // dispatch identically to the canonical lowercase forms. The
    // host and path retain original casing — host is downcased by
    // `url::Url` for IDN per spec, path stays verbatim.
    let normalised = normalise_scheme(uri);
    let uri = normalised.as_str();

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

    if let Some(cluster) = try_parse_grpc_cluster(uri, &limits)? {
        return Ok(cluster);
    }

    let parsed = Url::parse(uri)
        .map_err(|e| ParseError::new(ParseErrorKind::InvalidUri, format!("{e}: {uri}")))?;

    enforce_query_param_limit(&parsed, &limits)?;

    match parsed.scheme() {
        "red" | "reds" => {
            let host = parsed.host_str().ok_or_else(|| {
                ParseError::new(ParseErrorKind::InvalidUri, "red:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(DEFAULT_PORT_RED);
            Ok(ConnectionTarget::RedWire {
                host: host.to_string(),
                port,
                tls: parsed.scheme() == "reds",
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

/// Lowercase only the scheme portion (everything before the first
/// `:`), leaving host/path/query untouched. Returns the original
/// string when no scheme separator is present so the downstream
/// `Url::parse` path produces the canonical "missing scheme" error
/// instead of being masked here.
fn normalise_scheme(uri: &str) -> String {
    match uri.find(':') {
        Some(i) => {
            let scheme = &uri[..i];
            // Only ASCII alphanumerics + `+ . -` are valid scheme
            // bytes per RFC 3986. If the prefix violates that we
            // leave it alone so `Url::parse` can produce the
            // structured error.
            if scheme.is_empty()
                || !scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'.' || b == b'-')
            {
                return uri.to_string();
            }
            let mut out = String::with_capacity(uri.len());
            out.push_str(&scheme.to_ascii_lowercase());
            out.push_str(&uri[i..]);
            out
        }
        None => uri.to_string(),
    }
}

fn enforce_query_param_limit(url: &Url, limits: &ConnStringLimits) -> Result<(), ParseError> {
    let Some(q) = url.query() else {
        return Ok(());
    };
    if q.is_empty() {
        return Ok(());
    }
    let count = q.split('&').count();
    if count > limits.max_query_params {
        return Err(ParseError::new(
            ParseErrorKind::LimitExceeded,
            format!(
                "max_query_params exceeded: limit={} actual={}",
                limits.max_query_params, count,
            ),
        ));
    }
    Ok(())
}

/// Try to parse a multi-host gRPC URI. `Ok(None)` means "this is a
/// single-host URI — fall through to the standard parser".
fn try_parse_grpc_cluster(
    uri: &str,
    limits: &ConnStringLimits,
) -> Result<Option<ConnectionTarget>, ParseError> {
    let (rest, default_port) = if let Some(r) = uri.strip_prefix("grpc://") {
        (r, DEFAULT_PORT_GRPC)
    } else if let Some(r) = uri.strip_prefix("grpcs://") {
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

    let raw_count = host_part.split(',').count();
    if raw_count > limits.max_cluster_hosts {
        return Err(ParseError::new(
            ParseErrorKind::LimitExceeded,
            format!(
                "max_cluster_hosts exceeded: limit={} actual={}",
                limits.max_cluster_hosts, raw_count,
            ),
        ));
    }

    let mut endpoints: Vec<String> = Vec::with_capacity(raw_count);
    for raw in host_part.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ParseError::new(
                ParseErrorKind::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        // Bracketed IPv6 literal: `[::1]:5050` or `[::1]`.
        let (host, port) = if let Some(after_bracket) = raw.strip_prefix('[') {
            let end = after_bracket.find(']').ok_or_else(|| {
                ParseError::new(
                    ParseErrorKind::InvalidUri,
                    format!("unterminated IPv6 bracket in cluster URI: {raw}"),
                )
            })?;
            let host = &after_bracket[..end];
            let tail = &after_bracket[end + 1..];
            let port = if tail.is_empty() {
                default_port
            } else if let Some(p) = tail.strip_prefix(':') {
                p.parse::<u16>().map_err(|_| {
                    ParseError::new(
                        ParseErrorKind::InvalidUri,
                        format!("invalid port in cluster URI: {raw}"),
                    )
                })?
            } else {
                return Err(ParseError::new(
                    ParseErrorKind::InvalidUri,
                    format!("trailing junk after IPv6 bracket in cluster URI: {raw}"),
                ));
            };
            (format!("[{host}]"), port)
        } else {
            match raw.rsplit_once(':') {
                Some((h, p)) => {
                    let port: u16 = p.parse().map_err(|_| {
                        ParseError::new(
                            ParseErrorKind::InvalidUri,
                            format!("invalid port in cluster URI: {raw}"),
                        )
                    })?;
                    (h.to_string(), port)
                }
                None => (raw.to_string(), default_port),
            }
        };
        if host.is_empty() || host == "[]" {
            return Err(ParseError::new(
                ParseErrorKind::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        endpoints.push(format!("http://{host}:{port}"));
    }

    if let Some(q) = query_part {
        let qcount = if q.is_empty() {
            0
        } else {
            q.split('&').count()
        };
        if qcount > limits.max_query_params {
            return Err(ParseError::new(
                ParseErrorKind::LimitExceeded,
                format!(
                    "max_query_params exceeded: limit={} actual={}",
                    limits.max_query_params, qcount,
                ),
            ));
        }
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
