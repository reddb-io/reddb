//! URN codec for ASK source references (issue #394).
//!
//! Deep module: pure bidirectional codec between a typed [`Urn`]
//! value and its wire form.
//!
//! Wire grammar (per ADR 0013):
//!
//! ```text
//! urn        = "reddb:" collection "/" id [ "#" suffix ]
//! collection = pct-encoded utf-8
//! id         = pct-encoded utf-8
//! suffix     = pct-encoded utf-8           ; kind-specific:
//!                                          ;   VectorHit  → score literal
//!                                          ;   GraphEdge  → edge id
//!                                          ;   Document   → fragment label
//! ```
//!
//! Percent-encoding covers `/`, `#`, `%`, control bytes (`< 0x20`,
//! `0x7F`), space, and all bytes ≥ `0x80` so the wire form stays
//! ASCII and decoding can reconstruct UTF-8 byte-for-byte.
//!
//! No I/O. Round-tripping `decode(encode(u))` is the property the
//! unit tests pin.

use std::fmt;

const SCHEME: &str = "reddb:";

/// What kind of source the URN points at. Suffix payload (when
/// present) lives inside the variant.
#[derive(Debug, Clone, PartialEq)]
pub enum UrnKind {
    Row,
    KvEntry,
    GraphNode,
    VectorHit { score: f32 },
    Document { fragment: String },
    GraphEdge { edge_id: String },
}

impl UrnKind {
    fn suffix(&self) -> Option<String> {
        match self {
            UrnKind::Row | UrnKind::KvEntry | UrnKind::GraphNode => None,
            UrnKind::VectorHit { score } => Some(format_score(*score)),
            UrnKind::Document { fragment } => Some(fragment.clone()),
            UrnKind::GraphEdge { edge_id } => Some(edge_id.clone()),
        }
    }

    pub fn token(&self) -> &'static str {
        match self {
            UrnKind::Row => "row",
            UrnKind::KvEntry => "kv",
            UrnKind::GraphNode => "graph_node",
            UrnKind::VectorHit { .. } => "vector_hit",
            UrnKind::Document { .. } => "document",
            UrnKind::GraphEdge { .. } => "graph_edge",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Urn {
    pub collection: String,
    pub id: String,
    pub kind: UrnKind,
}

impl Urn {
    pub fn row(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::Row,
        }
    }
    pub fn vector_hit(collection: impl Into<String>, id: impl Into<String>, score: f32) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::VectorHit { score },
        }
    }
    pub fn document(
        collection: impl Into<String>,
        id: impl Into<String>,
        fragment: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::Document {
                fragment: fragment.into(),
            },
        }
    }
    pub fn graph_node(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::GraphNode,
        }
    }
    pub fn graph_edge(
        collection: impl Into<String>,
        id: impl Into<String>,
        edge_id: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::GraphEdge {
                edge_id: edge_id.into(),
            },
        }
    }
    pub fn kv(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            id: id.into(),
            kind: UrnKind::KvEntry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrnError {
    MissingScheme,
    MissingId,
    InvalidPercent,
    InvalidScore,
}

impl fmt::Display for UrnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrnError::MissingScheme => write!(f, "URN missing reddb: scheme"),
            UrnError::MissingId => write!(f, "URN missing /id segment"),
            UrnError::InvalidPercent => write!(f, "URN has invalid percent-encoding"),
            UrnError::InvalidScore => write!(f, "URN vector_hit suffix is not a score"),
        }
    }
}

impl std::error::Error for UrnError {}

pub fn encode(urn: &Urn) -> String {
    let mut s = String::with_capacity(SCHEME.len() + urn.collection.len() + urn.id.len() + 8);
    s.push_str(SCHEME);
    pct_encode_into(&urn.collection, &mut s);
    s.push('/');
    pct_encode_into(&urn.id, &mut s);
    if let Some(suffix) = urn.kind.suffix() {
        s.push('#');
        pct_encode_into(&suffix, &mut s);
    }
    s
}

/// Hint passed to [`decode`] so the codec stays pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindHint {
    Row,
    KvEntry,
    GraphNode,
    VectorHit,
    Document,
    GraphEdge,
}

pub fn decode(s: &str, hint: KindHint) -> Result<Urn, UrnError> {
    let rest = s.strip_prefix(SCHEME).ok_or(UrnError::MissingScheme)?;
    let (head, suffix) = match rest.split_once('#') {
        Some((h, s)) => (h, Some(pct_decode(s)?)),
        None => (rest, None),
    };
    let (collection, id) = head.split_once('/').ok_or(UrnError::MissingId)?;
    if id.is_empty() {
        return Err(UrnError::MissingId);
    }
    let collection = pct_decode(collection)?;
    let id = pct_decode(id)?;
    let kind = match (hint, suffix) {
        (KindHint::Row, None) => UrnKind::Row,
        (KindHint::KvEntry, None) => UrnKind::KvEntry,
        (KindHint::GraphNode, None) => UrnKind::GraphNode,
        (KindHint::VectorHit, Some(sx)) => {
            let score: f32 = sx.parse().map_err(|_| UrnError::InvalidScore)?;
            UrnKind::VectorHit { score }
        }
        (KindHint::Document, Some(sx)) => UrnKind::Document { fragment: sx },
        (KindHint::GraphEdge, Some(sx)) => UrnKind::GraphEdge { edge_id: sx },
        _ => return Err(UrnError::MissingId),
    };
    Ok(Urn {
        collection,
        id,
        kind,
    })
}

fn pct_encode_into(input: &str, out: &mut String) {
    for &b in input.as_bytes() {
        if needs_pct(b) {
            out.push('%');
            out.push(hex_high(b));
            out.push(hex_low(b));
        } else {
            out.push(b as char);
        }
    }
}

fn pct_decode(input: &str) -> Result<String, UrnError> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(UrnError::InvalidPercent);
            }
            let hi = hex_value(bytes[i + 1]).ok_or(UrnError::InvalidPercent)?;
            let lo = hex_value(bytes[i + 2]).ok_or(UrnError::InvalidPercent)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| UrnError::InvalidPercent)
}

fn needs_pct(b: u8) -> bool {
    b == b'%' || b == b'/' || b == b'#' || b == b' ' || !(0x20..0x7F).contains(&b)
}

fn hex_high(b: u8) -> char {
    let h = b >> 4;
    if h < 10 {
        (b'0' + h) as char
    } else {
        (b'A' + h - 10) as char
    }
}

fn hex_low(b: u8) -> char {
    let h = b & 0x0F;
    if h < 10 {
        (b'0' + h) as char
    } else {
        (b'A' + h - 10) as char
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

fn format_score(score: f32) -> String {
    let mut s = format!("{:.6}", score);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_round_trip() {
        let u = Urn::row("incidents", "42");
        assert_eq!(encode(&u), "reddb:incidents/42");
        assert_eq!(decode("reddb:incidents/42", KindHint::Row).unwrap(), u);
    }

    #[test]
    fn kv_round_trip() {
        let u = Urn::kv("settings", "ask.cache.enabled");
        assert_eq!(decode(&encode(&u), KindHint::KvEntry).unwrap(), u);
    }

    #[test]
    fn graph_node_round_trip() {
        let u = Urn::graph_node("hosts", "n-7");
        assert_eq!(decode(&encode(&u), KindHint::GraphNode).unwrap(), u);
    }

    #[test]
    fn vector_hit_round_trip() {
        let u = Urn::vector_hit("docs", "doc-9", 0.87125);
        let s = encode(&u);
        let back = decode(&s, KindHint::VectorHit).unwrap();
        assert_eq!(back.collection, "docs");
        assert_eq!(back.id, "doc-9");
        match back.kind {
            UrnKind::VectorHit { score } => assert!((score - 0.87125).abs() < 1e-5),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn vector_hit_score_format_stable() {
        assert_eq!(format_score(0.5), "0.5");
        assert_eq!(format_score(1.0), "1");
        assert_eq!(format_score(0.0), "0");
        assert_eq!(format_score(0.123456), "0.123456");
    }

    #[test]
    fn document_round_trip_with_fragment() {
        let u = Urn::document("manuals", "m-1", "chunk-7");
        assert_eq!(encode(&u), "reddb:manuals/m-1#chunk-7");
        assert_eq!(decode(&encode(&u), KindHint::Document).unwrap(), u);
    }

    #[test]
    fn graph_edge_round_trip() {
        let u = Urn::graph_edge("hosts", "n-1", "e-77");
        assert_eq!(encode(&u), "reddb:hosts/n-1#e-77");
        assert_eq!(decode(&encode(&u), KindHint::GraphEdge).unwrap(), u);
    }

    #[test]
    fn percent_encodes_separators_in_collection() {
        let u = Urn::row("we/ird#name", "id");
        assert_eq!(encode(&u), "reddb:we%2Fird%23name/id");
        assert_eq!(decode(&encode(&u), KindHint::Row).unwrap(), u);
    }

    #[test]
    fn percent_encodes_separators_in_id() {
        let u = Urn::row("col", "a/b#c");
        assert_eq!(encode(&u), "reddb:col/a%2Fb%23c");
        assert_eq!(decode(&encode(&u), KindHint::Row).unwrap(), u);
    }

    #[test]
    fn percent_encodes_space_and_percent() {
        let u = Urn::row("col with space", "100%");
        assert_eq!(encode(&u), "reddb:col%20with%20space/100%25");
        assert_eq!(decode(&encode(&u), KindHint::Row).unwrap(), u);
    }

    #[test]
    fn percent_encodes_control_bytes() {
        let u = Urn::row("col\nname", "id\t");
        let s = encode(&u);
        assert!(s.contains("%0A"));
        assert!(s.contains("%09"));
        assert_eq!(decode(&s, KindHint::Row).unwrap(), u);
    }

    #[test]
    fn utf8_round_trips_via_pct_encoding() {
        let u = Urn::row("日本語", "café");
        let s = encode(&u);
        assert!(s.is_ascii(), "wire URN must be ASCII: {s}");
        assert_eq!(decode(&s, KindHint::Row).unwrap(), u);
    }

    #[test]
    fn fragment_with_special_chars_round_trips() {
        let u = Urn::document("docs", "d-1", "section/2#a b");
        assert_eq!(decode(&encode(&u), KindHint::Document).unwrap(), u);
    }

    #[test]
    fn missing_scheme_rejected() {
        assert_eq!(
            decode("not-a-urn/x", KindHint::Row),
            Err(UrnError::MissingScheme)
        );
    }

    #[test]
    fn missing_id_rejected() {
        assert_eq!(
            decode("reddb:colonly", KindHint::Row),
            Err(UrnError::MissingId)
        );
        assert_eq!(
            decode("reddb:col/", KindHint::Row),
            Err(UrnError::MissingId)
        );
    }

    #[test]
    fn invalid_percent_rejected() {
        assert_eq!(
            decode("reddb:col%2/id", KindHint::Row),
            Err(UrnError::InvalidPercent)
        );
        assert_eq!(
            decode("reddb:col/id%ZZ", KindHint::Row),
            Err(UrnError::InvalidPercent)
        );
    }

    #[test]
    fn vector_hit_invalid_score_rejected() {
        assert_eq!(
            decode("reddb:docs/d-1#nope", KindHint::VectorHit),
            Err(UrnError::InvalidScore)
        );
    }

    #[test]
    fn hint_mismatch_rejected() {
        let s = encode(&Urn::row("col", "id"));
        assert!(decode(&s, KindHint::VectorHit).is_err());
        let s = encode(&Urn::vector_hit("col", "id", 0.5));
        assert!(decode(&s, KindHint::Row).is_err());
    }

    #[test]
    fn token_is_stable() {
        assert_eq!(UrnKind::Row.token(), "row");
        assert_eq!(UrnKind::KvEntry.token(), "kv");
        assert_eq!(UrnKind::GraphNode.token(), "graph_node");
        assert_eq!(UrnKind::VectorHit { score: 0.0 }.token(), "vector_hit");
        assert_eq!(
            UrnKind::Document {
                fragment: "x".into()
            }
            .token(),
            "document"
        );
        assert_eq!(
            UrnKind::GraphEdge {
                edge_id: "e".into()
            }
            .token(),
            "graph_edge"
        );
    }

    /// Pseudo-property test: deterministic byte-pattern matrix
    /// covering separator / pct / space / control / UTF-8 chars
    /// across every kind.
    #[test]
    fn property_round_trip_byte_matrix() {
        let collections = [
            "simple",
            "with/slash",
            "with#hash",
            "with%pct",
            "with space",
            "with\ttab",
            "with\nnewline",
            "café",
            "日本語",
            "mixed/ # %",
        ];
        let ids = ["1", "abc", "uuid-1234", "with/slash", "deep/path#frag"];
        for c in collections {
            for i in ids {
                for hint in [KindHint::Row, KindHint::KvEntry, KindHint::GraphNode] {
                    let u = match hint {
                        KindHint::Row => Urn::row(c, i),
                        KindHint::KvEntry => Urn::kv(c, i),
                        KindHint::GraphNode => Urn::graph_node(c, i),
                        _ => unreachable!(),
                    };
                    let s = encode(&u);
                    assert_eq!(decode(&s, hint).unwrap(), u, "mismatch for {s}");
                }
                let v = Urn::vector_hit(c, i, 0.42);
                let back = decode(&encode(&v), KindHint::VectorHit).unwrap();
                assert_eq!(back.collection, v.collection);
                assert_eq!(back.id, v.id);
                let d = Urn::document(c, i, "frag/with#stuff");
                assert_eq!(decode(&encode(&d), KindHint::Document).unwrap(), d);
                let e = Urn::graph_edge(c, i, "edge%01");
                assert_eq!(decode(&encode(&e), KindHint::GraphEdge).unwrap(), e);
            }
        }
    }
}
