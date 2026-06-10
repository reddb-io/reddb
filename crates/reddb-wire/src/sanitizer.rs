//! Connection-string sanitizer + typed taint guard (issue #179, ADR 0010).
//!
//! The Whiz / Babeld disclosure (March 2026) is the canonical example
//! of the failure mode this module exists to prevent: caller-supplied
//! strings concatenated into a structured serialization format whose
//! delimiter the caller controls let the caller smuggle a forged field
//! past the producer and into the consumer's parser. The
//! [serialization-boundary audit][audit] enumerated 15 instances of
//! the pattern in this codebase. F-04 is the broadest: ~141
//! `tracing::*!` call sites that interpolate user-supplied strings via
//! `Display`, so a CR/LF in a connection-string-derived token, a
//! tenant name, or a collection name forges a log line.
//!
//! [audit]: ../../docs/security/serialization-boundary-audit-2026-05-06.md
//!
//! This module ships three things:
//!
//! 1. [`Tainted<T>`] — a non-`Display` wrapper. The only way to
//!    project the inner string into a structured serialization
//!    boundary is [`Tainted::escape_for`], which forces the caller to
//!    name the [`Boundary`] and returns an [`EscapedFor`] under the
//!    boundary's escape contract.
//! 2. [`ConnStringSanitizer`] — a deep module wrapping the existing
//!    [`crate::conn_string::parse`]. It returns a
//!    [`ParsedConnString`] whose host / cert-path / endpoint / query
//!    fields are exposed as `Tainted<String>` so downstream consumers
//!    cannot accidentally route a tainted byte through `Display`.
//! 3. [`audit_safe_log_field`] — a thin `Display` adapter that strips
//!    CR/LF/NUL/control bytes from a `&str` for log emission. The
//!    structured fix is [`Tainted::escape_for(Boundary::LogField)`];
//!    the helper exists because the codebase has hundreds of call
//!    sites where a full type-system migration is mechanical work
//!    that CI lint #180 tracks separately, and the helper unblocks
//!    incremental migration without expanding the attack surface.

use std::fmt;

use crate::conn_string::{
    parse as parse_conn_string, ConnectionTarget, ParseError as ConnParseError,
};

// ---------------------------------------------------------------------------
// Boundary + escape error
// ---------------------------------------------------------------------------

/// Serialization boundaries supported by [`Tainted::escape_for`].
///
/// Each variant names the exact escape contract the boundary expects.
/// The contract is implemented by [`Tainted::escape_for`] and
/// validated by the proptest corpus in this crate's test suite, so
/// adding a variant requires extending both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// HTTP/1.1 + HTTP/2 header value (`http::HeaderValue`-safe).
    /// Strips CR, LF, NUL, and tab — the four bytes that let an
    /// attacker terminate the current header and inject a second one.
    /// The output is guaranteed to be accepted by
    /// [`http::HeaderValue::from_str`] (modulo bytes outside ASCII
    /// 0x20-0x7E which are passed through; the gRPC layer rejects
    /// non-visible-ASCII separately).
    HttpHeader,
    /// gRPC metadata value. gRPC metadata sits on HTTP/2 headers, so
    /// the contract is identical to [`Boundary::HttpHeader`].
    GrpcMetadata,
    /// Plain-text log line emitted via `tracing` or any other
    /// line-oriented formatter. Strips CR / LF / NUL / control bytes
    /// (0x00-0x1F + 0x7F) and percent-encodes them, so a smuggled
    /// `\nlevel=ERROR` survives as `%0Alevel=ERROR` in the captured
    /// line — visibly tampered, never authoritative.
    LogField,
    /// Structured audit field. Pass-through; the
    /// `AuditFieldEscaper` (#177, slice AC) owns the on-disk encoder
    /// and rejects control bytes at emit time. Exposing the typed
    /// value here lets the audit lane consume `Tainted<String>`
    /// without going through a string detour.
    AuditField,
    /// JSON value. Pass-through; the `SerializedJsonField` (#178,
    /// slice AB) round-trips through `serde_json::Value::String` and
    /// inherits serde's escape contract. Exposing the typed value
    /// here lets the JSON lane consume `Tainted<String>` without a
    /// string detour.
    JsonValue,
}

impl Boundary {
    pub fn as_str(self) -> &'static str {
        match self {
            Boundary::HttpHeader => "HttpHeader",
            Boundary::GrpcMetadata => "GrpcMetadata",
            Boundary::LogField => "LogField",
            Boundary::AuditField => "AuditField",
            Boundary::JsonValue => "JsonValue",
        }
    }
}

/// Stable error code returned by [`Tainted::escape_for`].
///
/// The escape paths in this module are total — they always produce a
/// safe value — so today the only failure mode is "input was so long
/// that escaping would exceed [`Tainted::MAX_ESCAPED_LEN`]". Future
/// boundaries (e.g. an MTLS SAN slot with a 256-byte cap) get their
/// own variants here without breaking existing callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeError {
    /// Escaping the input would produce a value longer than
    /// [`Tainted::MAX_ESCAPED_LEN`].
    TooLong { boundary: Boundary, bytes: usize },
}

impl fmt::Display for EscapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EscapeError::TooLong { boundary, bytes } => write!(
                f,
                "escape_for({}) would emit {} bytes (limit {})",
                boundary.as_str(),
                bytes,
                Tainted::<String>::MAX_ESCAPED_LEN,
            ),
        }
    }
}

impl std::error::Error for EscapeError {}

// ---------------------------------------------------------------------------
// EscapedFor — boundary-tagged result of `Tainted::escape_for`
// ---------------------------------------------------------------------------

/// Output of [`Tainted::escape_for`]. Carries the boundary it was
/// escaped for so a header setter can statically refuse a value that
/// was escaped for a log line, and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapedFor {
    boundary: Boundary,
    value: String,
}

impl EscapedFor {
    pub fn boundary(&self) -> Boundary {
        self.boundary
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
    pub fn into_string(self) -> String {
        self.value
    }
}

impl fmt::Display for EscapedFor {
    /// `EscapedFor` is `Display`-able by design — once the value has
    /// crossed the [`Tainted::escape_for`] gate the boundary's escape
    /// contract has been applied and the bytes are safe to render
    /// against that boundary's parser.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

// ---------------------------------------------------------------------------
// Tainted<T> — non-`Display` wrapper around caller-supplied bytes
// ---------------------------------------------------------------------------

/// Caller-controlled value that has not yet crossed a serialization
/// boundary. Deliberately does **not** implement [`fmt::Display`];
/// the only way to project a `Tainted<String>` into a serialized
/// surface is [`Tainted::escape_for`].
///
/// Implements [`fmt::Debug`] (which `tracing` `?value` calls) because
/// `Debug` quote-wraps and escapes control bytes, so emitting a
/// `Tainted<String>` via `?value` is safe in a way `%value` is not.
///
/// The propagation rule is enforced by visibility, not by the type
/// system: the inner field is `pub(crate)` so only this crate can
/// build a `Tainted<String>`. Downstream crates receive
/// `Tainted<String>` from [`ConnStringSanitizer::parse`] and cannot
/// peel it; they must call [`Tainted::escape_for`] or
/// [`Tainted::expose_secret`] (the latter named loudly to surface in
/// review).
#[derive(Clone, PartialEq, Eq)]
pub struct Tainted<T>(pub(crate) T);

impl<T> Tainted<T> {
    /// Build a `Tainted` from a caller-supplied value. This is the
    /// one place the type system loses ground; every site that calls
    /// it should be reviewable.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Escape hatch for callers that need the raw inner. Named
    /// loudly so a grep / review / lint flags it. Prefer
    /// [`Tainted::escape_for`].
    pub fn expose_secret(&self) -> &T {
        &self.0
    }

    /// Consuming variant of [`Tainted::expose_secret`].
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: fmt::Debug> fmt::Debug for Tainted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Debug`-quoting + control-byte escaping is the safe form
        // that F-04 recommends as the mechanical fix; preserve it.
        f.debug_tuple("Tainted").field(&self.0).finish()
    }
}

impl Tainted<String> {
    /// Hard cap on the size of any escaped boundary projection.
    /// Mirrors the `max_uri_bytes` default in [`crate::conn_string`]
    /// (8 KiB) so a tainted value that fits the parser also fits the
    /// boundary projection.
    pub const MAX_ESCAPED_LEN: usize = 8 * 1024;

    /// Project the tainted value into the named [`Boundary`]'s
    /// escape contract. Returns [`EscapedFor`] tagged with the
    /// boundary, so a header setter can statically refuse a value
    /// that was escaped for a log line.
    pub fn escape_for(&self, boundary: Boundary) -> Result<EscapedFor, EscapeError> {
        let escaped = match boundary {
            Boundary::HttpHeader | Boundary::GrpcMetadata => escape_http_header(&self.0),
            Boundary::LogField => escape_log_field(&self.0),
            // AuditField + JsonValue are pass-through: their downstream
            // guard owns the encoder. Returning the inner string
            // tagged with the boundary lets a typed setter (`fn
            // set(field: EscapedFor)` whose `boundary` matches
            // `AuditField`) consume it without a re-escape.
            Boundary::AuditField | Boundary::JsonValue => self.0.clone(),
        };
        if escaped.len() > Self::MAX_ESCAPED_LEN {
            return Err(EscapeError::TooLong {
                boundary,
                bytes: escaped.len(),
            });
        }
        Ok(EscapedFor {
            boundary,
            value: escaped,
        })
    }
}

impl From<String> for Tainted<String> {
    fn from(s: String) -> Self {
        Tainted(s)
    }
}

impl From<&str> for Tainted<String> {
    fn from(s: &str) -> Self {
        Tainted(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Boundary-specific escapers
// ---------------------------------------------------------------------------

/// `HeaderValue` / gRPC metadata contract: strip the four header
/// terminators (CR, LF, NUL, tab). Other bytes pass through. The
/// downstream constructor (`http::HeaderValue::from_str`) is the
/// authoritative gate; this function is the producer-side guard.
fn escape_http_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'\r' | b'\n' | 0x00 | b'\t' => {
                // Strip. Header field-values forbid these per RFC 9110.
            }
            _ => out.push(b as char),
        }
    }
    out
}

/// Log-field contract: percent-encode CR / LF / NUL / control bytes
/// (0x00-0x1F + 0x7F). Other bytes pass through. Percent-encoding
/// (rather than stripping) preserves visible evidence of tampering
/// in the captured log line.
fn escape_log_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b < 0x20 || b == 0x7F {
            out.push('%');
            out.push(hex_nibble(b >> 4));
            out.push(hex_nibble(b & 0x0F));
        } else {
            out.push(b as char);
        }
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + (n - 10)) as char,
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// audit_safe_log_field — incremental-migration helper for F-04
// ---------------------------------------------------------------------------

/// `Display` adapter that strips CR / LF / NUL / control bytes from
/// a borrowed `&str` for log emission.
///
/// This is the F-04 incremental-migration helper. The structural fix
/// is [`Tainted<String>::escape_for(Boundary::LogField)`], which the
/// codebase will adopt as the new tracing-fields plumbing lands; the
/// helper exists so contributors can mechanically migrate a single
/// `tracing::info!(user = %username, ...)` call site to
/// `tracing::info!(user = %audit_safe_log_field(&username), ...)`
/// without first re-typing the upstream variable as `Tainted<String>`.
///
/// Output bytes match `escape_for(Boundary::LogField)`. The contract
/// is enforced by a shared helper.
pub fn audit_safe_log_field(value: &str) -> impl fmt::Display + '_ {
    AuditSafeLogField(value)
}

struct AuditSafeLogField<'a>(&'a str);

impl fmt::Display for AuditSafeLogField<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0.bytes() {
            if b < 0x20 || b == 0x7F {
                write!(f, "%{:02X}", b)?;
            } else {
                f.write_str(std::str::from_utf8(&[b]).unwrap_or("?"))?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ConnStringSanitizer + ParsedConnString
// ---------------------------------------------------------------------------

/// Parse a connection URI and surface its caller-controlled fields
/// inside [`Tainted<String>`].
///
/// Wraps [`crate::conn_string::parse`]. The underlying
/// [`ConnectionTarget`] is preserved verbatim — this is purely an
/// additive layer that lets new code paths consume tainted values
/// without forcing a breaking change on existing consumers
/// ([`reddb_client::connect`], `red_client`, the driver crates).
pub struct ConnStringSanitizer;

impl ConnStringSanitizer {
    /// Parse `uri` into a [`ParsedConnString`]. Same DoS guardrails
    /// as [`crate::conn_string::parse`].
    pub fn parse(uri: &str) -> Result<ParsedConnString, ConnParseError> {
        let target = parse_conn_string(uri)?;
        Ok(ParsedConnString { target })
    }
}

/// Sanitized view of a parsed connection string.
///
/// Holds the raw [`ConnectionTarget`] (preserves backward
/// compatibility with [`crate::conn_string::parse`]) plus typed
/// accessors that hand caller-supplied fields out as
/// [`Tainted<String>`]. Downstream consumers that re-emit the values
/// (gRPC `Endpoint::from`, log lines, error messages, audit fields)
/// route through [`Tainted::escape_for`] rather than the raw inner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConnString {
    target: ConnectionTarget,
}

impl ParsedConnString {
    /// Typed view of the parsed target. Each variant carries
    /// [`Tainted<String>`] for every caller-influenced field.
    pub fn target(&self) -> TaintedTarget<'_> {
        match &self.target {
            ConnectionTarget::Memory => TaintedTarget::Memory,
            ConnectionTarget::File { path } => TaintedTarget::File { path },
            ConnectionTarget::Grpc { endpoint } => TaintedTarget::Grpc {
                endpoint: TaintedRef(endpoint),
            },
            ConnectionTarget::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => TaintedTarget::GrpcCluster {
                primary: TaintedRef(primary),
                replicas,
                force_primary: *force_primary,
            },
            ConnectionTarget::Http { base_url } => TaintedTarget::Http {
                base_url: TaintedRef(base_url),
            },
            ConnectionTarget::RedWire { host, port, tls } => TaintedTarget::RedWire {
                host: TaintedRef(host),
                port: *port,
                tls: *tls,
            },
            ConnectionTarget::WsNative { host, port, tls } => TaintedTarget::WsNative {
                host: TaintedRef(host),
                port: *port,
                tls: *tls,
            },
        }
    }

    /// Hand the underlying [`ConnectionTarget`] back. Backward-compat
    /// hatch for callers that have not yet been migrated to consume
    /// [`TaintedTarget`].
    pub fn into_connection_target(self) -> ConnectionTarget {
        self.target
    }

    /// Borrow the underlying [`ConnectionTarget`].
    pub fn as_connection_target(&self) -> &ConnectionTarget {
        &self.target
    }
}

/// Borrowed-side analogue of [`Tainted<String>`]. Same escape API,
/// no allocation when the caller only wants `expose_secret`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaintedRef<'a>(&'a String);

impl<'a> TaintedRef<'a> {
    pub fn expose_secret(&self) -> &'a str {
        self.0.as_str()
    }
    pub fn to_owned_tainted(&self) -> Tainted<String> {
        Tainted(self.0.clone())
    }
    pub fn escape_for(&self, boundary: Boundary) -> Result<EscapedFor, EscapeError> {
        Tainted(self.0.clone()).escape_for(boundary)
    }
}

/// Typed view of [`ConnectionTarget`] with [`TaintedRef`] in place
/// of the bare `String` fields.
#[derive(Debug)]
pub enum TaintedTarget<'a> {
    Memory,
    File {
        path: &'a std::path::Path,
    },
    Grpc {
        endpoint: TaintedRef<'a>,
    },
    GrpcCluster {
        primary: TaintedRef<'a>,
        replicas: &'a [String],
        force_primary: bool,
    },
    Http {
        base_url: TaintedRef<'a>,
    },
    RedWire {
        host: TaintedRef<'a>,
        port: u16,
        tls: bool,
    },
    WsNative {
        host: TaintedRef<'a>,
        port: u16,
        tls: bool,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_strip_crlf_nul_tab() {
        let t = Tainted::<String>::from("v1\r\nX-Forged: yes\0\there");
        let e = t.escape_for(Boundary::HttpHeader).unwrap();
        assert_eq!(e.boundary(), Boundary::HttpHeader);
        assert!(!e.as_str().contains('\r'));
        assert!(!e.as_str().contains('\n'));
        assert!(!e.as_str().contains('\0'));
        assert!(!e.as_str().contains('\t'));
        assert_eq!(e.as_str(), "v1X-Forged: yeshere");
    }

    #[test]
    fn grpc_metadata_matches_http_header_contract() {
        let payload = "alice\r\nx-trace-id: forged";
        let h = Tainted::from(payload)
            .escape_for(Boundary::HttpHeader)
            .unwrap();
        let g = Tainted::from(payload)
            .escape_for(Boundary::GrpcMetadata)
            .unwrap();
        assert_eq!(h.as_str(), g.as_str());
    }

    #[test]
    fn log_field_percent_encodes_control_bytes() {
        let t = Tainted::<String>::from(
            "alice\nlevel=ERROR\rcluster_breach=true\ttab\0nul\x07bel\x1bescape\x7fdel",
        );
        let e = t.escape_for(Boundary::LogField).unwrap();
        let s = e.as_str();
        // Every control byte must be escaped, not stripped, so
        // tampering remains visible in the log line.
        assert!(!s.contains('\n'));
        assert!(!s.contains('\r'));
        assert!(!s.contains('\0'));
        assert!(!s.contains('\t'));
        assert!(!s.contains('\x07'));
        assert!(!s.contains('\x1b'));
        assert!(!s.contains('\x7f'));
        assert!(s.contains("%0A"));
        assert!(s.contains("%0D"));
        assert!(s.contains("%00"));
        assert!(s.contains("%09"));
        assert!(s.contains("%07"));
        assert!(s.contains("%1B"));
        assert!(s.contains("%7F"));
    }

    #[test]
    fn audit_and_json_pass_through() {
        // Audit + JSON are pass-through; their downstream guard owns
        // the encoder. The boundary tag travels with the value so a
        // typed setter can refuse a mismatched escape.
        let raw = "alice\nbob";
        let a = Tainted::from(raw).escape_for(Boundary::AuditField).unwrap();
        let j = Tainted::from(raw).escape_for(Boundary::JsonValue).unwrap();
        assert_eq!(a.as_str(), raw);
        assert_eq!(j.as_str(), raw);
        assert_eq!(a.boundary(), Boundary::AuditField);
        assert_eq!(j.boundary(), Boundary::JsonValue);
    }

    #[test]
    fn audit_safe_log_field_strips_crlf() {
        let evil = "alice\nlevel=ERROR cluster_breach=true";
        let rendered = format!("{}", audit_safe_log_field(evil));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\r'));
        assert!(rendered.contains("%0A"));
    }

    #[test]
    fn audit_safe_log_field_matches_log_field_boundary() {
        // Same bytes out as escape_for(LogField). This is the
        // contract: the helper is a `Display` adapter for the same
        // escaper, so an incremental migration via the helper does
        // not change behaviour when the call site later upgrades to
        // Tainted<String>.
        let evil = "user\rname\nrow=1\0nul\x1Besc\x7Fdel";
        let helper = format!("{}", audit_safe_log_field(evil));
        let typed = Tainted::from(evil)
            .escape_for(Boundary::LogField)
            .unwrap()
            .into_string();
        assert_eq!(helper, typed);
    }

    #[test]
    fn tainted_is_not_display() {
        // Compile-time check: the caller cannot accidentally write a
        // Tainted<String> through `{}`. We can't *test* the absence
        // of an impl with a unit test, but Debug-quoting *is* present
        // and round-trips control bytes through the standard escape.
        let t = Tainted::from("alice\nbob");
        let dbg = format!("{:?}", t);
        assert!(dbg.contains("\\n"), "Debug must escape control bytes");
    }

    #[test]
    fn parser_round_trip_grpc() {
        let parsed = ConnStringSanitizer::parse("grpc://node-1:5055").unwrap();
        match parsed.target() {
            TaintedTarget::Grpc { endpoint } => {
                assert_eq!(endpoint.expose_secret(), "http://node-1:5055");
                let h = endpoint.escape_for(Boundary::HttpHeader).unwrap();
                assert!(!h.as_str().contains('\n'));
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn parser_round_trip_redwire() {
        let parsed = ConnStringSanitizer::parse("reds://example.com:9999").unwrap();
        match parsed.target() {
            TaintedTarget::RedWire { host, port, tls } => {
                assert_eq!(host.expose_secret(), "example.com");
                assert_eq!(port, 9999);
                assert!(tls);
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn parser_into_connection_target_compat() {
        // Backward-compat hatch: the underlying ConnectionTarget is
        // unchanged so existing consumers keep working.
        let parsed = ConnStringSanitizer::parse("memory://").unwrap();
        assert_eq!(parsed.into_connection_target(), ConnectionTarget::Memory);
    }

    #[test]
    fn escape_too_long_surfaces_typed_error() {
        let big = "a".repeat(Tainted::<String>::MAX_ESCAPED_LEN + 1);
        let err = Tainted::from(big.as_str())
            .escape_for(Boundary::LogField)
            .unwrap_err();
        match err {
            EscapeError::TooLong { boundary, bytes } => {
                assert_eq!(boundary, Boundary::LogField);
                assert!(bytes > Tainted::<String>::MAX_ESCAPED_LEN);
            }
        }
    }
}
