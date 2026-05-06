//! Canonical `Topology` payload — shared by both transports.
//!
//! ADR 0008 (`docs/adr/0008-topology-advertisement-security.md`)
//! settled the security model and the schema-evolution rule:
//!
//!   * New optional fields land under a versioned envelope.
//!   * Old parsers ignore unknown fields cleanly. Unknown version
//!     tags are dropped (no panic), the consumer falls back to
//!     URI-only routing.
//!   * Schema-version bumps are reserved for changes a naive
//!     optional field cannot express (a removed field, a renegotiated
//!     meaning, a framing change). They are not the default move.
//!
//! Wire encoding (single shape across RedWire HelloAck + gRPC
//! `Topology` RPC):
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ u8   version_tag        currently 0x01                     │
//! │ u32  body_length (LE)   bytes that follow                  │
//! │ ... body ...            version-specific payload encoding  │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! The header (tag + length) is fixed across versions so a parser
//! that does not recognise the tag can still skip the entire blob
//! cleanly.
//!
//! Body for `0x01`: a flat little-endian struct dump where every
//! string is `u32 len` + utf-8 bytes:
//!
//! ```text
//! u64  epoch
//! str  primary.addr
//! str  primary.region
//! u32  replicas.len
//! foreach replica:
//!   str  addr
//!   str  region
//!   u8   healthy   (0 / 1)
//!   u32  lag_ms
//!   u64  last_applied_lsn
//! ```
//!
//! No serde dependency: a hex dump stays readable, no extra crate
//! pulled into `reddb-wire`, and the format matches the rest of the
//! RedWire codec discipline.

/// Wire version tag for the initial schema.
///
/// Bumping this is reserved for genuinely breaking changes (removed
/// field, renegotiated meaning, framing change). Additive evolution
/// (new optional fields) keeps this byte stable — see ADR 0008 §4.
pub const TOPOLOGY_WIRE_VERSION_V1: u8 = 0x01;

/// Highest tag the current parser understands. A `Topology` blob
/// stamped with anything else is ignored cleanly so an older client
/// can keep its URI-only routing fallback.
pub const MAX_KNOWN_TOPOLOGY_VERSION: u8 = TOPOLOGY_WIRE_VERSION_V1;

/// Header size (version tag + u32 body length).
pub const TOPOLOGY_HEADER_SIZE: usize = 1 + 4;

/// Canonical topology payload — the same shape both transports
/// carry. New optional fields go in here as `Option<…>` /
/// `Vec<…>` and ride the existing version tag; only a genuinely
/// breaking change earns a new tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub epoch: u64,
    pub primary: Endpoint,
    pub replicas: Vec<ReplicaInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: String,
    pub region: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaInfo {
    pub addr: String,
    pub region: String,
    pub healthy: bool,
    pub lag_ms: u32,
    pub last_applied_lsn: u64,
}

/// Decode-side errors. Distinct from "unknown version tag", which
/// is reported as `Ok(None)` on `decode_topology` so the consumer
/// can fall back to URI-only routing without branching on an
/// error variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyError {
    Truncated,
    BodyLengthMismatch { declared: u32, available: usize },
    InvalidUtf8,
    StringTooLong { declared: u32, remaining: usize },
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "topology blob truncated (< 5-byte header)"),
            Self::BodyLengthMismatch {
                declared,
                available,
            } => write!(
                f,
                "topology body length mismatch: declared {declared}, available {available}"
            ),
            Self::InvalidUtf8 => write!(f, "topology string field is not valid UTF-8"),
            Self::StringTooLong {
                declared,
                remaining,
            } => write!(
                f,
                "topology string length {declared} exceeds remaining body bytes {remaining}"
            ),
        }
    }
}

impl std::error::Error for TopologyError {}

/// Encode `topology` to the canonical version-tagged byte string.
/// Same bytes consumed by both RedWire HelloAck (after base64
/// embedding in the JSON envelope) and the gRPC `TopologyReply`
/// (carried directly as a `bytes` field).
pub fn encode_topology(topology: &Topology) -> Vec<u8> {
    let mut body = Vec::with_capacity(estimate_body_size(topology));
    body.extend_from_slice(&topology.epoch.to_le_bytes());
    write_str(&mut body, &topology.primary.addr);
    write_str(&mut body, &topology.primary.region);
    body.extend_from_slice(&(topology.replicas.len() as u32).to_le_bytes());
    for r in &topology.replicas {
        write_str(&mut body, &r.addr);
        write_str(&mut body, &r.region);
        body.push(if r.healthy { 1 } else { 0 });
        body.extend_from_slice(&r.lag_ms.to_le_bytes());
        body.extend_from_slice(&r.last_applied_lsn.to_le_bytes());
    }

    let mut out = Vec::with_capacity(TOPOLOGY_HEADER_SIZE + body.len());
    out.push(TOPOLOGY_WIRE_VERSION_V1);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Decode a topology blob.
///
/// Returns:
/// * `Ok(Some(Topology))` — recognised version tag, body parsed.
/// * `Ok(None)` — unknown version tag. The consumer is expected to
///   fall back to URI-only routing rather than treat this as an
///   error (ADR 0008 §4: unknown fields are dropped, not rejected).
/// * `Err(TopologyError)` — recognised tag, but the body was
///   structurally malformed (truncated, invalid UTF-8, …).
pub fn decode_topology(bytes: &[u8]) -> Result<Option<Topology>, TopologyError> {
    if bytes.len() < TOPOLOGY_HEADER_SIZE {
        return Err(TopologyError::Truncated);
    }
    let version = bytes[0];
    let declared_len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    let body = &bytes[TOPOLOGY_HEADER_SIZE..];
    if (body.len() as u64) < declared_len as u64 {
        return Err(TopologyError::BodyLengthMismatch {
            declared: declared_len,
            available: body.len(),
        });
    }
    let body = &body[..declared_len as usize];

    if version > MAX_KNOWN_TOPOLOGY_VERSION {
        // Forward-compat: unknown version tag, drop cleanly.
        return Ok(None);
    }

    // version == 0x01
    let mut cur = Cursor::new(body);
    let epoch = cur.read_u64()?;
    let primary_addr = cur.read_str()?;
    let primary_region = cur.read_str()?;
    let replica_count = cur.read_u32()? as usize;
    let mut replicas = Vec::with_capacity(replica_count);
    for _ in 0..replica_count {
        let addr = cur.read_str()?;
        let region = cur.read_str()?;
        let healthy = cur.read_u8()? != 0;
        let lag_ms = cur.read_u32()?;
        let last_applied_lsn = cur.read_u64()?;
        replicas.push(ReplicaInfo {
            addr,
            region,
            healthy,
            lag_ms,
            last_applied_lsn,
        });
    }
    Ok(Some(Topology {
        epoch,
        primary: Endpoint {
            addr: primary_addr,
            region: primary_region,
        },
        replicas,
    }))
}

fn estimate_body_size(t: &Topology) -> usize {
    let endpoint = |e: &Endpoint| 4 + e.addr.len() + 4 + e.region.len();
    let mut n = 8 + endpoint(&t.primary) + 4;
    for r in &t.replicas {
        n += 4 + r.addr.len() + 4 + r.region.len() + 1 + 4 + 8;
    }
    n
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_u8(&mut self) -> Result<u8, TopologyError> {
        if self.remaining() < 1 {
            return Err(TopologyError::Truncated);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32, TopologyError> {
        if self.remaining() < 4 {
            return Err(TopologyError::Truncated);
        }
        let bytes = &self.buf[self.pos..self.pos + 4];
        self.pos += 4;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, TopologyError> {
        if self.remaining() < 8 {
            return Err(TopologyError::Truncated);
        }
        let bytes = &self.buf[self.pos..self.pos + 8];
        self.pos += 8;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_str(&mut self) -> Result<String, TopologyError> {
        let len = self.read_u32()?;
        if (len as usize) > self.remaining() {
            return Err(TopologyError::StringTooLong {
                declared: len,
                remaining: self.remaining(),
            });
        }
        let bytes = &self.buf[self.pos..self.pos + len as usize];
        self.pos += len as usize;
        let s = std::str::from_utf8(bytes)
            .map_err(|_| TopologyError::InvalidUtf8)?
            .to_string();
        Ok(s)
    }
}

// ---------------------------------------------------------------
// HelloAck embedding.
//
// The HelloAck payload is a JSON object. To carry the binary
// topology blob inside it without breaking the existing JSON-only
// parser (which deserialises the whole payload with `serde_json`),
// the canonical bytes are base64-encoded under a new `topology`
// key. Old parsers ignore unknown keys cleanly; new parsers extract
// the field and run `decode_topology` on the decoded bytes.
// ---------------------------------------------------------------

/// Base64-encode the canonical topology bytes for embedding inside
/// a HelloAck JSON payload as a string field.
///
/// The caller (server-side HelloAck builder) is expected to insert
/// the resulting string under the JSON key `"topology"`; the client
/// extracts that key, base64-decodes it, and runs `decode_topology`.
pub fn encode_topology_for_hello_ack(topology: &Topology) -> String {
    base64_encode(&encode_topology(topology))
}

/// Decode the base64 string carried in HelloAck JSON `topology`
/// field back into a `Topology`.
///
/// Mirrors `decode_topology`'s three-state contract:
/// * `Ok(Some(_))` — recognised version tag, body parsed.
/// * `Ok(None)` — base64 decode failed *or* the version tag is
///   unknown. Both cases collapse to "fall back to URI-only
///   routing"; the consumer does not branch on which one it was.
/// * `Err(_)` — recognised version tag with a malformed body.
pub fn decode_topology_from_hello_ack(field: &str) -> Result<Option<Topology>, TopologyError> {
    let Some(bytes) = base64_decode(field) else {
        // Malformed base64 is treated as "unknown encoding, drop
        // cleanly" so the client falls back to URI-only routing —
        // same posture as an unknown version tag (ADR 0008 §4).
        return Ok(None);
    };
    decode_topology(&bytes)
}

const B64_ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let chunks = input.chunks_exact(3);
    let rem = chunks.remainder();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[(n & 0x3F) as usize] as char);
    }
    match rem {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in trimmed.bytes() {
        let v: u32 = match ch {
            b'A'..=b'Z' => (ch - b'A') as u32,
            b'a'..=b'z' => (ch - b'a' + 26) as u32,
            b'0'..=b'9' => (ch - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Topology {
        Topology {
            epoch: 0xDEAD_BEEF_CAFE_BABE,
            primary: Endpoint {
                addr: "primary.example.com:5050".into(),
                region: "us-east-1".into(),
            },
            replicas: vec![
                ReplicaInfo {
                    addr: "replica-a.example.com:5050".into(),
                    region: "us-east-1".into(),
                    healthy: true,
                    lag_ms: 12,
                    last_applied_lsn: 4242,
                },
                ReplicaInfo {
                    addr: "replica-b.example.com:5050".into(),
                    region: "us-west-2".into(),
                    healthy: false,
                    lag_ms: 999,
                    last_applied_lsn: 4100,
                },
            ],
        }
    }

    #[test]
    fn round_trip_v1() {
        let t = fixture();
        let bytes = encode_topology(&t);
        let decoded = decode_topology(&bytes).expect("decode").expect("v1 known");
        assert_eq!(decoded, t);
    }

    #[test]
    fn empty_replicas_round_trip() {
        let t = Topology {
            epoch: 1,
            primary: Endpoint {
                addr: "p:5050".into(),
                region: "r".into(),
            },
            replicas: vec![],
        };
        let bytes = encode_topology(&t);
        let decoded = decode_topology(&bytes).expect("decode").expect("v1");
        assert_eq!(decoded, t);
    }

    #[test]
    fn unknown_version_tag_returns_none() {
        // Forward-compat invariant from ADR 0008 §4: an unknown
        // version tag must be ignored, not rejected. The consumer
        // falls back to URI-only routing.
        let mut bytes = encode_topology(&fixture());
        bytes[0] = 0xFE; // bumped past MAX_KNOWN_TOPOLOGY_VERSION
        let decoded = decode_topology(&bytes).expect("decode");
        assert!(
            decoded.is_none(),
            "unknown version tag must drop cleanly, got {decoded:?}"
        );
    }

    #[test]
    fn truncated_header_errors() {
        assert!(matches!(
            decode_topology(&[0x01, 0x00]),
            Err(TopologyError::Truncated)
        ));
    }

    #[test]
    fn body_length_mismatch_errors() {
        // Declared body length larger than buffer.
        let bytes = vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        assert!(matches!(
            decode_topology(&bytes),
            Err(TopologyError::BodyLengthMismatch { .. })
        ));
    }

    #[test]
    fn version_tag_is_pinned_to_0x01() {
        // Sentinel against an accidental schema bump. ADR 0008 §4
        // reserves the bump for genuinely breaking changes; a PR
        // adding an optional field must NOT touch this value.
        assert_eq!(TOPOLOGY_WIRE_VERSION_V1, 0x01);
    }

    #[test]
    fn hello_ack_round_trip_via_base64() {
        // The HelloAck embedding shape: encode → base64-string →
        // decode. Same canonical bytes both transports carry, just
        // base64-wrapped so the JSON envelope stays valid.
        let t = fixture();
        let field = encode_topology_for_hello_ack(&t);
        let decoded = decode_topology_from_hello_ack(&field)
            .expect("decode")
            .expect("v1 known");
        assert_eq!(decoded, t);
    }

    #[test]
    fn hello_ack_inner_bytes_match_grpc_bytes() {
        // The acceptance criterion (#166 §4): same bytes consumed
        // by both transports. Round-trip via the HelloAck base64
        // wrapper and assert the decoded inner payload is byte-for-
        // byte equivalent to the canonical encoding.
        let t = fixture();
        let canonical = encode_topology(&t);
        let field = encode_topology_for_hello_ack(&t);
        let recovered = base64_decode(&field).expect("base64");
        assert_eq!(recovered, canonical);
    }

    #[test]
    fn hello_ack_unknown_version_tag_drops_cleanly() {
        // A HelloAck whose topology field carries a future version
        // tag must not panic — the client falls back to URI-only
        // routing.
        let mut bytes = encode_topology(&fixture());
        bytes[0] = 0x99;
        let field = base64_encode(&bytes);
        let decoded = decode_topology_from_hello_ack(&field).expect("decode");
        assert!(decoded.is_none());
    }

    #[test]
    fn hello_ack_malformed_base64_drops_cleanly() {
        // A garbled base64 field is treated like an unknown version
        // tag: drop, fall back to URI-only routing, never panic.
        let decoded = decode_topology_from_hello_ack("@not base64@").expect("decode");
        assert!(decoded.is_none());
    }

    #[test]
    fn old_hello_ack_without_topology_field_is_backwards_compat() {
        // Backwards-compat (#166 acceptance criterion §6): an
        // old-style HelloAck JSON payload — no `topology` key —
        // still parses cleanly into a Topology slot of `None` on
        // the consumer side. We model that here by extracting the
        // JSON value the way the client will (look for the key,
        // run our decoder if present), and checking the absence
        // path resolves to "no topology, fall back to URI-only".
        let json = br#"{"version":1,"auth":"bearer","features":3,"server":"reddb/0.2.9"}"#;
        let v: serde_json_check::Value =
            serde_json_check::from_slice(json).expect("valid JSON");
        let topo_field = v.find_string("topology");
        let topology = match topo_field {
            None => None,
            Some(s) => decode_topology_from_hello_ack(&s).expect("decode"),
        };
        assert!(
            topology.is_none(),
            "an old HelloAck without `topology` must produce None"
        );
    }

    /// Tiny JSON probe used only by the backwards-compat test.
    /// `reddb-wire` has no JSON dep — keep this scoped to test
    /// code so we do not pull serde into the production build.
    mod serde_json_check {
        pub enum Value {
            Object(Vec<(String, Value)>),
            String(String),
            Other,
        }

        impl Value {
            pub fn find_string(&self, key: &str) -> Option<String> {
                match self {
                    Value::Object(map) => map.iter().find_map(|(k, v)| {
                        if k == key {
                            if let Value::String(s) = v {
                                Some(s.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }),
                    _ => None,
                }
            }
        }

        pub fn from_slice(bytes: &[u8]) -> Result<Value, &'static str> {
            let s = std::str::from_utf8(bytes).map_err(|_| "utf8")?;
            let mut p = Parser { src: s, pos: 0 };
            p.skip_ws();
            let v = p.parse_value()?;
            Ok(v)
        }

        struct Parser<'a> {
            src: &'a str,
            pos: usize,
        }

        impl<'a> Parser<'a> {
            fn rest(&self) -> &'a str {
                &self.src[self.pos..]
            }
            fn bump(&mut self, n: usize) {
                self.pos += n;
            }
            fn skip_ws(&mut self) {
                while let Some(c) = self.rest().chars().next() {
                    if c.is_whitespace() {
                        self.bump(c.len_utf8());
                    } else {
                        break;
                    }
                }
            }
            fn parse_value(&mut self) -> Result<Value, &'static str> {
                self.skip_ws();
                let head = self.rest().chars().next().ok_or("eof")?;
                match head {
                    '{' => self.parse_object(),
                    '"' => self.parse_string().map(Value::String),
                    _ => {
                        // Skip primitives (numbers, true/false/null,
                        // arrays). The probe only cares about object
                        // keys at the top level, so coarse skipping
                        // is enough for our fixture.
                        self.skip_until_top_level_comma_or_close();
                        Ok(Value::Other)
                    }
                }
            }
            fn skip_until_top_level_comma_or_close(&mut self) {
                let mut depth = 0i32;
                while let Some(c) = self.rest().chars().next() {
                    match c {
                        '"' => {
                            let _ = self.parse_string();
                            continue;
                        }
                        '{' | '[' => {
                            depth += 1;
                            self.bump(1);
                        }
                        '}' | ']' => {
                            if depth == 0 {
                                return;
                            }
                            depth -= 1;
                            self.bump(1);
                        }
                        ',' if depth == 0 => return,
                        _ => self.bump(c.len_utf8()),
                    }
                }
            }
            fn parse_object(&mut self) -> Result<Value, &'static str> {
                self.bump(1); // '{'
                let mut map = Vec::new();
                loop {
                    self.skip_ws();
                    if self.rest().starts_with('}') {
                        self.bump(1);
                        return Ok(Value::Object(map));
                    }
                    let key = self.parse_string()?;
                    self.skip_ws();
                    if !self.rest().starts_with(':') {
                        return Err("expected ':'");
                    }
                    self.bump(1);
                    let val = self.parse_value()?;
                    map.push((key, val));
                    self.skip_ws();
                    match self.rest().chars().next() {
                        Some(',') => {
                            self.bump(1);
                            continue;
                        }
                        Some('}') => {
                            self.bump(1);
                            return Ok(Value::Object(map));
                        }
                        _ => return Err("expected ',' or '}'"),
                    }
                }
            }
            fn parse_string(&mut self) -> Result<String, &'static str> {
                if !self.rest().starts_with('"') {
                    return Err("expected '\"'");
                }
                self.bump(1);
                let start = self.pos;
                while let Some(c) = self.rest().chars().next() {
                    if c == '"' {
                        let s = self.src[start..self.pos].to_string();
                        self.bump(1);
                        return Ok(s);
                    }
                    if c == '\\' {
                        self.bump(c.len_utf8());
                    }
                    self.bump(c.len_utf8());
                }
                Err("unterminated string")
            }
        }
    }

    #[test]
    fn header_layout_first_byte_is_version_then_le_length() {
        // Pinning the layout: byte 0 is the tag, bytes 1..5 are the
        // little-endian body length. A consumer that sees an unknown
        // tag still knows how many bytes to skip — that is the only
        // forward-compat invariant the header has to carry.
        let t = fixture();
        let bytes = encode_topology(&t);
        assert_eq!(bytes[0], TOPOLOGY_WIRE_VERSION_V1);
        let declared = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        assert_eq!(declared as usize, bytes.len() - TOPOLOGY_HEADER_SIZE);
    }
}
