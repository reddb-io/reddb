//! Adversarial proptest corpus for `ConnStringSanitizer` /
//! `Tainted<String>::escape_for` (issue #179, ADR 0010).
//!
//! For every `Boundary` and every adversarial byte (CR, LF, NUL, HT,
//! DEL, ESC, BEL, double-quote, semicolon, backslash, percent), an
//! arbitrary string sandwich (`prefix + payload + suffix`) must
//! survive a round-trip through `escape_for(boundary)` without
//! smuggling a delimiter into the boundary's parser. The contract per
//! boundary:
//!
//! - `HttpHeader` / `GrpcMetadata`: output must contain none of CR,
//!   LF, NUL, or HT (the four bytes that terminate a header line).
//! - `LogField`: output must contain no byte < 0x20 and no 0x7F. CR,
//!   LF, NUL, and the ESC sequence Babeld-style log shippers split on
//!   are all escaped (visible-but-inert).
//! - `AuditField` / `JsonValue`: pass-through; the contract is that
//!   the boundary tag tracks the value so a typed setter on the
//!   downstream lane can refuse a mismatched escape.

use proptest::prelude::*;
use reddb_wire::{Boundary, ConnStringSanitizer, Tainted, TaintedTarget};

// 1 KiB strings keep the corpus tractable and stay well under the
// 8 KiB MAX_ESCAPED_LEN guard.
const MAX_BYTES: usize = 1024;

fn adversarial_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(b'\r'),
        Just(b'\n'),
        Just(0x00),
        Just(b'\t'),
        Just(0x07), // BEL
        Just(0x1B), // ESC
        Just(0x7F), // DEL
        Just(b'"'),
        Just(b';'),
        Just(b'\\'),
        Just(b'%'),
        Just(b','),
        any::<u8>(), // full 0..=255 to fuzz the boundary
    ]
}

fn adversarial_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(adversarial_byte(), 0..MAX_BYTES).prop_map(|bytes| {
        // Convert to UTF-8 by treating each byte as Latin-1; the
        // escape paths are byte-oriented anyway, so the "shape" of
        // adversarial inputs is preserved without needing valid
        // UTF-8 only.
        bytes.iter().map(|&b| b as char).collect::<String>()
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    /// HTTP header / gRPC metadata: the four header terminators must
    /// never survive escape_for, regardless of input.
    #[test]
    fn http_header_strips_terminators(payload in adversarial_string()) {
        let t = Tainted::<String>::from(payload.as_str());
        let h = t.escape_for(Boundary::HttpHeader).unwrap();
        let s = h.as_str();
        prop_assert!(!s.contains('\r'), "CR survived: {s:?}");
        prop_assert!(!s.contains('\n'), "LF survived: {s:?}");
        prop_assert!(!s.contains('\0'), "NUL survived: {s:?}");
        prop_assert!(!s.contains('\t'), "HT survived: {s:?}");
        prop_assert_eq!(h.boundary(), Boundary::HttpHeader);
    }

    #[test]
    fn grpc_metadata_strips_terminators(payload in adversarial_string()) {
        let t = Tainted::<String>::from(payload.as_str());
        let g = t.escape_for(Boundary::GrpcMetadata).unwrap();
        let s = g.as_str();
        prop_assert!(!s.contains('\r'));
        prop_assert!(!s.contains('\n'));
        prop_assert!(!s.contains('\0'));
        prop_assert!(!s.contains('\t'));
    }

    /// Log field: no byte below 0x20 and no 0x7F may survive. This
    /// is the F-04 contract — Filebeat-style log shippers split on
    /// 0x0A; if any control byte rides through we have lost.
    #[test]
    fn log_field_escapes_all_control_bytes(payload in adversarial_string()) {
        let t = Tainted::<String>::from(payload.as_str());
        let l = t.escape_for(Boundary::LogField).unwrap();
        let s = l.as_str();
        for b in s.bytes() {
            prop_assert!(b >= 0x20 && b != 0x7F,
                "control byte 0x{:02X} survived in {s:?}", b);
        }
    }

    /// Pass-through boundaries: the byte-for-byte payload survives,
    /// only the boundary tag changes. The downstream guard owns the
    /// encoder, so corruption-class properties move to that lane's
    /// test suite.
    #[test]
    fn audit_field_round_trips(payload in adversarial_string()) {
        let t = Tainted::<String>::from(payload.as_str());
        let a = t.escape_for(Boundary::AuditField).unwrap();
        prop_assert_eq!(a.as_str(), payload.as_str());
        prop_assert_eq!(a.boundary(), Boundary::AuditField);
    }

    #[test]
    fn json_value_round_trips(payload in adversarial_string()) {
        let t = Tainted::<String>::from(payload.as_str());
        let j = t.escape_for(Boundary::JsonValue).unwrap();
        prop_assert_eq!(j.as_str(), payload.as_str());
        prop_assert_eq!(j.boundary(), Boundary::JsonValue);
    }

    /// Conn-string parser surface: a parsed grpc cluster URI carries
    /// host bytes that may have come from a hostile DNS / config
    /// source. Once they're inside `Tainted<String>`, every
    /// downstream emission must route through `escape_for` to be
    /// safe. The property here checks the typed-target accessor
    /// preserves the bytes the parser produced (no silent
    /// corruption) while still routing through the escape contract.
    #[test]
    fn parsed_grpc_cluster_typed_view(
        host_a in "[a-z][a-z0-9-]{0,30}",
        host_b in "[a-z][a-z0-9-]{0,30}",
        port_a in 1u16..=65535,
        port_b in 1u16..=65535,
    ) {
        let uri = format!("grpc://{host_a}:{port_a},{host_b}:{port_b}");
        let parsed = ConnStringSanitizer::parse(&uri).unwrap();
        match parsed.target() {
            TaintedTarget::GrpcCluster { primary, replicas, .. } => {
                let p = primary.expose_secret().to_string();
                prop_assert!(p.contains(&host_a));
                prop_assert!(p.contains(&port_a.to_string()));
                let h = primary.escape_for(Boundary::HttpHeader).unwrap();
                prop_assert!(!h.as_str().contains('\n'));
                prop_assert_eq!(replicas.len(), 1);
            }
            other => prop_assert!(false, "unexpected variant {other:?}"),
        }
    }
}

/// F-04 sub-test: prove the LogField escape neutralises the
/// Babeld-style CRLF injection at the format-string level — the same
/// path `tracing::info!("{}", escaped)` walks when it renders the
/// value into a log line.
///
/// `EscapedFor: Display`, so this is exactly what tracing's
/// `format_args!`-based macro will emit; bringing the
/// `tracing-subscriber` collector into the wire crate just to
/// re-verify a pure formatting contract would only add a runtime dep
/// for no extra coverage.
#[test]
fn f04_log_field_capture_no_crlf_injection() {
    let evil = "alice\nlevel=ERROR cluster_breach=true target=\"reddb::secrets\"";
    let escaped = Tainted::<String>::from(evil)
        .escape_for(Boundary::LogField)
        .unwrap();
    let formatted = format!("{}", escaped);
    assert!(!formatted.contains('\n'), "LF in {formatted:?}");
    assert!(!formatted.contains('\r'), "CR in {formatted:?}");
    // The smuggled `level=ERROR` payload survives only as a visibly
    // tampered substring — the LF that would have terminated the
    // prior field is now `%0A`, so a downstream log shipper sees one
    // line, not two.
    assert!(formatted.contains("%0A"));
    assert!(formatted.contains("level=ERROR"));

    // Same property via the `audit_safe_log_field` helper, which is
    // the path migrating callers will use until upstream variables
    // are re-typed as `Tainted<String>`.
    let helper = format!("{}", reddb_wire::audit_safe_log_field(evil));
    assert!(!helper.contains('\n'));
    assert!(!helper.contains('\r'));
    assert!(helper.contains("%0A"));
}
