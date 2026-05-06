//! `SerializedJsonField` — typed guard for JSON-envelope construction.
//!
//! Issue [#178](https://github.com/reddb-io/reddb/issues/178), enforcing
//! [ADR 0010 §3](../../../docs/adr/0010-serialization-boundary-discipline.md#3-serializedjsonfield--helloack--payloadreply--topology-json).
//!
//! # The boundary
//!
//! Every JSON envelope this server emits — HelloAck (issue #166), gRPC
//! `PayloadReply` (`crates/reddb-server/src/grpc/service_impl.rs`), and
//! HTTP response bodies (`crates/reddb-server/src/server/handlers_*.rs`)
//! — is a structured serialization format whose delimiter (`"`,
//! control bytes, `{`/`}`, `:`) the untrusted caller can attempt to
//! inject. The Whiz / Babeld pattern is `serialize(trusted ++
//! untrusted)` without escape: the producer emits attacker-controlled
//! bytes verbatim and the downstream parser sees a forged field.
//!
//! `SerializedJsonField` is the typed point of crossing for that
//! boundary on the producer side. Caller-influenced data does not get
//! formatted into a JSON envelope as raw bytes; it round-trips through
//! [`crate::serde_json::Value`] first, picking up the canonical
//! RFC-8259-compliant escape contract from
//! [`crate::serde_json::Value::to_string_compact`] (the F-01 hotfix
//! shipped in #181).
//!
//! # Public surface
//!
//! - [`SerializedJsonField::tainted`] — wrap an untrusted, caller-
//!   influenced string. Returns a [`crate::serde_json::Value::String`]
//!   that, when serialized, will have every control byte and JSON
//!   delimiter escaped per RFC 8259 §7. Use this for error messages,
//!   user-supplied identifiers, free-form text, and anything reaching
//!   the envelope from a parser, header, or request body.
//! - [`SerializedJsonField::typed`] — wrap a known-typed value that
//!   implements [`crate::serde_json::JsonEncode`] (the in-house
//!   counterpart to `serde::Serialize`). Returns the value's
//!   canonical [`crate::serde_json::Value`] representation. Use this
//!   for structs and enums whose schema is owned by the server; it
//!   guarantees the round-trip even for nested string fields.
//!
//! Both forms produce a [`crate::serde_json::Value`] that the rest of
//! the envelope assembly (`Map::insert`, `to_string_compact`)
//! consumes uniformly. A caller never hands raw bytes to the JSON
//! emitter; everything goes through `Value`.
//!
//! # F-05 — SQL parser error message routing
//!
//! Audit finding F-05 (see
//! `docs/security/serialization-boundary-audit-2026-05-06.md`)
//! observes that SQL parser errors interpolate user-supplied SQL
//! fragments into their `Display` strings via bare `format!`. When
//! such an error message reaches an HTTP response body via
//! [`crate::server::transport::json_error`], the F-05 fix on the JSON
//! wire side is to route the message through
//! [`SerializedJsonField::tainted`] before embedding it. That fix
//! lands in this slice via the
//! [`crate::server::transport::json_error`] retrofit, which now wraps
//! every error message with the guard regardless of upstream origin.
//! The parser-side F-05 cleanup (avoiding `format!` for the offending
//! fragment in the first place) is a separate concern tracked under
//! Lane AG / issue #184.
//!
//! # Why `tainted` does not return an error
//!
//! Unlike [`crate::server::header_escape_guard::HeaderEscapeGuard`],
//! which rejects CR/LF/NUL/tab outright, `SerializedJsonField` cannot
//! reject anything: every Unicode string is a legal JSON string under
//! RFC 8259 §7 once escaped. The contract is "round-trip", not
//! "validate". The result is always emittable.

use crate::serde_json::{JsonEncode, Value};

/// Typed guard for JSON-envelope field construction. See module docs.
///
/// Zero-sized; the type exists only to namespace the constructors and
/// to make audit grep (`SerializedJsonField::tainted`,
/// `SerializedJsonField::typed`) trivially locatable.
pub struct SerializedJsonField;

impl SerializedJsonField {
    /// Wrap an untrusted, caller-influenced string as a JSON value.
    ///
    /// The returned [`Value::String`] will, on serialization through
    /// [`Value::to_string_compact`], have every control byte
    /// (`U+0000..U+001F`), embedded `"`, `\`, and other JSON
    /// delimiters escaped per RFC 8259 §7. The downstream parser sees
    /// the original bytes verbatim — it does *not* see the bytes as
    /// envelope structure.
    ///
    /// This function is the canonical entry point for caller-
    /// influenced JSON-envelope fields. Examples:
    ///
    /// - Error messages reaching `json_error` (HTTP body)
    /// - SQL parser error fragments (F-05 fix)
    /// - User-supplied identifiers reflected back into a response
    /// - Connection-string fragments arriving via
    ///   [`reddb_wire::Tainted`] after `escape_for(Boundary::JsonValue)`
    pub fn tainted(s: &str) -> Value {
        Value::String(s.to_string())
    }

    /// Wrap a server-owned, typed value as a JSON value.
    ///
    /// Use this for structs and enums whose schema the server owns
    /// (configuration, status snapshots, typed view-models). The
    /// `JsonEncode` impl walks the type and produces the canonical
    /// [`Value`] tree; any nested string fields automatically inherit
    /// the round-trip guarantee.
    ///
    /// Note: `JsonEncode` is this workspace's in-house counterpart to
    /// `serde::Serialize`; the dependency-free split is documented in
    /// [`crate::serde_json`].
    pub fn typed<T: JsonEncode + ?Sized>(value: &T) -> Value {
        value.to_json_value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serde_json::{from_str, Map};

    fn round_trip(input: &str) -> String {
        let mut obj = Map::new();
        obj.insert("field".to_string(), SerializedJsonField::tainted(input));
        let envelope = Value::Object(obj).to_string_compact();
        // Parse back as a JSON object and pull out `field`.
        let parsed: Value = from_str(&envelope).expect("envelope must be valid JSON");
        match parsed {
            Value::Object(map) => match map.get("field").cloned() {
                Some(Value::String(s)) => s,
                other => panic!("field missing or wrong shape: {other:?}"),
            },
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn json_field_tainted_round_trips_quote_smuggling_attempt() {
        // Classic envelope-smuggling payload: caller hopes to terminate
        // the field early and inject a sibling key.
        let payload = r#"val"; "injected": true"#;
        assert_eq!(round_trip(payload), payload);
    }

    #[test]
    fn json_field_tainted_round_trips_crlf_in_value() {
        // CRLF in a JSON value must survive as `\r\n` escape, not as
        // raw bytes that confuse a downstream line-oriented log
        // shipper that re-splits the body.
        let payload = "first line\r\n\"injected_key\": \"x";
        assert_eq!(round_trip(payload), payload);
    }

    #[test]
    fn json_field_tainted_escapes_all_control_bytes() {
        // Every byte 0x00..0x20 must be escaped to a `\uXXXX` form (or
        // a short escape for the standard ones) — never silently
        // dropped, never emitted raw.
        for byte in 0x00u8..0x20 {
            let payload: String = char::from_u32(byte as u32).unwrap().to_string();
            let mut obj = Map::new();
            obj.insert("k".to_string(), SerializedJsonField::tainted(&payload));
            let envelope = Value::Object(obj).to_string_compact();
            // The raw control byte must not appear in the envelope.
            assert!(
                !envelope.as_bytes().contains(&byte) || byte == b'\n' && envelope.contains("\\n"),
                "byte 0x{byte:02x} appeared raw in envelope: {envelope:?}"
            );
            // And the round-trip must yield the original byte.
            assert_eq!(round_trip(&payload), payload);
        }
    }

    #[test]
    fn json_field_tainted_round_trips_existing_escape_sequences() {
        // The caller's literal `\n` (two chars: backslash + n) must
        // survive as the *literal* two-char sequence — the wrapper
        // must not re-interpret it as an actual newline.
        let payload = r#"contains \n and \t as literal chars"#;
        assert_eq!(round_trip(payload), payload);
    }

    #[test]
    fn json_field_tainted_round_trips_deeply_nested_escapes() {
        // Worst-case: caller hands us a string that *itself* looks
        // like a JSON-in-JSON envelope. The wrapper round-trips it as
        // a single string field — the downstream parser sees one
        // string, not a nested object.
        let payload =
            r#"{"outer":"{\"inner\":\"{\\\"deepest\\\":\\\"\\\\\\\"end\\\\\\\"\\\"}\"}"}"#;
        assert_eq!(round_trip(payload), payload);
    }

    #[test]
    fn json_field_tainted_round_trips_when_used_as_object_key() {
        // Object keys are also JSON strings; the same escape contract
        // applies. We test by inserting the tainted string as a key.
        let key = "key\"with\\quotes\nand-newlines";
        let mut obj = Map::new();
        obj.insert(key.to_string(), SerializedJsonField::tainted("v"));
        let envelope = Value::Object(obj).to_string_compact();
        let parsed: Value = from_str(&envelope).expect("envelope must be valid JSON");
        match parsed {
            Value::Object(map) => assert!(
                map.get(key).is_some(),
                "key did not round-trip; map keys: {:?}",
                map.keys().collect::<Vec<_>>()
            ),
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn json_field_tainted_round_trips_unicode_and_emoji() {
        let payload = "café — naïve façade — 日本語 — 🦀";
        assert_eq!(round_trip(payload), payload);
    }

    #[test]
    fn json_field_typed_emits_canonical_representation() {
        // `typed` for known-good values goes through JsonEncode and
        // produces a canonical Value tree.
        let v = SerializedJsonField::typed(&42_i64);
        assert_eq!(v.as_i64(), Some(42));
        let v = SerializedJsonField::typed(&true);
        assert_eq!(v.as_bool(), Some(true));
        let v = SerializedJsonField::typed(&"hello");
        assert_eq!(v.as_str(), Some("hello"));
    }

    /// Regression: a malicious payload combining every smuggling
    /// trick at once. Every byte must round-trip through the guard.
    #[test]
    fn json_field_tainted_handles_full_malicious_corpus() {
        let corpus: &[&str] = &[
            r#"{"key": "val"; "injected": true}"#,
            "line1\r\nContent-Length: 0\r\n\r\nhost: evil",
            "control\x00bytes\x01\x02\x03\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x1fend",
            "escapes \\n \\r \\t \\u0041 \\\\ \\\" literal",
            r#"deeply{"nested":{"json":{"inside":"of","a":"string"}}}deeply"#,
            "trailing-newline\n",
            "\"-prefixed",
            "back\\slash-suffix\\",
            // F-05 flavour: an SQL parser error message embedding a
            // user fragment that itself looks like JSON.
            r#"sql parse error: unexpected token "}" near "select * from t where j = '{\"x\":1}'""#,
        ];
        for payload in corpus {
            assert_eq!(round_trip(payload), *payload, "corpus payload: {payload:?}");
        }
    }
}
