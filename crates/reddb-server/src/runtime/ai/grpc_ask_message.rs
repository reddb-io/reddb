//! `GrpcAskMessage` — pure builder pinning the typed gRPC `AskReply`
//! shape (issue #407, PRD #391).
//!
//! Deep module: no I/O, no transport, no codegen dependency. Defines
//! the typed gRPC message shape and a pure converter from the
//! canonical [`super::ask_response_envelope::AskResult`] to the typed
//! reply. The proto edit + service-impl wiring slice is deferred —
//! pinning the shape here means the wiring slice cannot quietly drop
//! `citations`, reorder field tags, or re-shape `validation` without
//! the tests in this file failing first.
//!
//! ## Why a separate module
//!
//! The current gRPC `Ask` RPC (`service_impl.rs::ask`) returns a
//! generic `PayloadReply { payload_json }` — the legacy bucketed-only
//! shape. PRD #391's AC for #407 requires the full ASK schema as a
//! typed gRPC message so existing gRPC clients (Go's `pgx`-style
//! drivers, JVM `io.grpc.*`, dotnet `Grpc.Net.Client`) get field-typed
//! deserialisation rather than a JSON blob to parse twice.
//!
//! Two things must stay aligned across the slice:
//!
//! 1. The proto field numbers are an external API — once a tag is
//!    shipped, it cannot change without breaking compiled clients.
//!    Pinned by [`PROTO_TAGS`] + tests.
//! 2. Field set must match [`super::ask_response_envelope`] one-to-one
//!    so the JSON envelope (used by JSON-RPC #406, MCP non-stream
//!    #409, PG-wire #408) and the gRPC reply describe the same data.
//!    Pinned by `field_set_matches_json_envelope`.
//!
//! ## Field tags (proto3)
//!
//! `AskReply` (top-level):
//! - 1 `string answer`
//! - 2 `string sources_flat_json`   (JSON-encoded array, same bytes as the envelope's `sources_flat`)
//! - 3 `repeated Citation citations`
//! - 4 `Validation validation`
//! - 5 `string provider`
//! - 6 `string model`
//! - 7 `uint32 prompt_tokens`
//! - 8 `uint32 completion_tokens`
//! - 9 `double cost_usd`
//! - 10 `bool cache_hit`
//! - 11 `string mode`               ("strict" | "lenient" — effective)
//! - 12 `uint32 retry_count`
//!
//! `Citation`:
//! - 1 `uint32 marker`
//! - 2 `string urn`
//!
//! `Validation`:
//! - 1 `bool ok`
//! - 2 `repeated ValidationItem warnings`
//! - 3 `repeated ValidationItem errors`
//!
//! `ValidationItem`:
//! - 1 `string kind`     ("malformed" | "out_of_range")
//! - 2 `string detail`
//!
//! `sources_flat` is carried as a single JSON string (`sources_flat_json`)
//! rather than a `repeated SourceRow` to keep parity with the envelope
//! shape and avoid forcing per-row payload re-encoding. Clients that
//! want structured rows parse the JSON; the same bytes already flow on
//! JSON-RPC #406, MCP #409, and PG-wire #408.
//!
//! Determinism = seed (#400) is *not* in the reply. Mirrors the JSON
//! envelope's omission — see `ask_response_envelope` rationale.

use super::ask_response_envelope::{
    AskResult, Citation as EnvCitation, Mode, SourceRow, Validation as EnvValidation,
    ValidationError, ValidationWarning,
};

/// One citation row in the typed gRPC reply.
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcCitation {
    pub marker: u32,
    pub urn: String,
}

/// One validation item (warning or error).
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcValidationItem {
    pub kind: String,
    pub detail: String,
}

/// Validation block.
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcValidation {
    pub ok: bool,
    pub warnings: Vec<GrpcValidationItem>,
    pub errors: Vec<GrpcValidationItem>,
}

/// Typed gRPC `AskReply` body.
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcAskReply {
    pub answer: String,
    pub sources_flat_json: String,
    pub citations: Vec<GrpcCitation>,
    pub validation: GrpcValidation,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub cache_hit: bool,
    pub mode: String,
    pub retry_count: u32,
}

/// Proto field tags for `AskReply` — pinned constants. Editing any of
/// these is a wire-breaking change and the tests in this module will
/// catch it.
pub mod proto_tags {
    pub mod ask_reply {
        pub const ANSWER: u32 = 1;
        pub const SOURCES_FLAT_JSON: u32 = 2;
        pub const CITATIONS: u32 = 3;
        pub const VALIDATION: u32 = 4;
        pub const PROVIDER: u32 = 5;
        pub const MODEL: u32 = 6;
        pub const PROMPT_TOKENS: u32 = 7;
        pub const COMPLETION_TOKENS: u32 = 8;
        pub const COST_USD: u32 = 9;
        pub const CACHE_HIT: u32 = 10;
        pub const MODE: u32 = 11;
        pub const RETRY_COUNT: u32 = 12;
    }
    pub mod citation {
        pub const MARKER: u32 = 1;
        pub const URN: u32 = 2;
    }
    pub mod validation {
        pub const OK: u32 = 1;
        pub const WARNINGS: u32 = 2;
        pub const ERRORS: u32 = 3;
    }
    pub mod validation_item {
        pub const KIND: u32 = 1;
        pub const DETAIL: u32 = 2;
    }
}

/// Build the typed gRPC reply from the canonical `AskResult`.
///
/// Citation ordering, `sources_flat` ordering, and field semantics
/// match [`super::ask_response_envelope::build`] one-to-one. Running
/// this on byte-equal input is byte-equal output (pinned by
/// `build_is_deterministic_across_calls`) — required by the ASK
/// determinism contract (#400).
pub fn build(result: &AskResult) -> GrpcAskReply {
    let mut citations: Vec<GrpcCitation> = result
        .citations
        .iter()
        .map(|c: &EnvCitation| GrpcCitation {
            marker: c.marker,
            urn: c.urn.clone(),
        })
        .collect();
    citations.sort_by_key(|c| c.marker);

    GrpcAskReply {
        answer: result.answer.clone(),
        sources_flat_json: sources_flat_json(&result.sources_flat),
        citations,
        validation: validation_from(&result.validation),
        provider: result.provider.clone(),
        model: result.model.clone(),
        prompt_tokens: result.prompt_tokens,
        completion_tokens: result.completion_tokens,
        cost_usd: result.cost_usd,
        cache_hit: result.cache_hit,
        mode: mode_str(result.effective_mode).to_string(),
        retry_count: result.retry_count,
    }
}

fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Strict => "strict",
        Mode::Lenient => "lenient",
    }
}

fn validation_from(v: &EnvValidation) -> GrpcValidation {
    GrpcValidation {
        ok: v.ok,
        warnings: v.warnings.iter().map(warning_item).collect(),
        errors: v.errors.iter().map(error_item).collect(),
    }
}

fn warning_item(w: &ValidationWarning) -> GrpcValidationItem {
    GrpcValidationItem {
        kind: w.kind.clone(),
        detail: w.detail.clone(),
    }
}

fn error_item(e: &ValidationError) -> GrpcValidationItem {
    GrpcValidationItem {
        kind: e.kind.clone(),
        detail: e.detail.clone(),
    }
}

fn sources_flat_json(rows: &[SourceRow]) -> String {
    // Order preserved verbatim — post-RRF rank is the contract since
    // citation `[^N]` indexes into the array, and reordering would
    // silently break grounding. Keys alphabetised (`payload`, `urn`)
    // to match the envelope's `BTreeMap`-backed encoder.
    let mut out = String::from("[");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        out.push_str("\"payload\":");
        push_json_string(&mut out, &r.payload);
        out.push(',');
        out.push_str("\"urn\":");
        push_json_string(&mut out, &r.urn);
        out.push('}');
    }
    out.push(']');
    out
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::proto_tags::*;
    use super::*;
    use crate::runtime::ai::ask_response_envelope::{
        AskResult, Citation as EnvCitation, Mode, SourceRow, Validation as EnvValidation,
        ValidationError, ValidationWarning,
    };

    fn sample_result() -> AskResult {
        AskResult {
            answer: "The capital is Lisbon [^1].".to_string(),
            sources_flat: vec![SourceRow {
                urn: "urn:reddb:row:cities/42".to_string(),
                payload: "{\"name\":\"Lisbon\"}".to_string(),
            }],
            citations: vec![EnvCitation {
                marker: 1,
                urn: "urn:reddb:row:cities/42".to_string(),
            }],
            validation: EnvValidation {
                ok: true,
                warnings: vec![],
                errors: vec![],
            },
            cache_hit: false,
            provider: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            prompt_tokens: 123,
            completion_tokens: 17,
            cost_usd: 0.0042,
            effective_mode: Mode::Strict,
            retry_count: 0,
        }
    }

    #[test]
    fn build_emits_every_top_level_field() {
        let r = sample_result();
        let reply = build(&r);
        assert_eq!(reply.answer, r.answer);
        assert_eq!(reply.provider, r.provider);
        assert_eq!(reply.model, r.model);
        assert_eq!(reply.prompt_tokens, r.prompt_tokens);
        assert_eq!(reply.completion_tokens, r.completion_tokens);
        assert_eq!(reply.cost_usd, r.cost_usd);
        assert_eq!(reply.cache_hit, r.cache_hit);
        assert_eq!(reply.retry_count, r.retry_count);
        assert_eq!(reply.mode, "strict");
        assert!(reply.validation.ok);
        assert_eq!(reply.citations.len(), 1);
        assert_eq!(reply.citations[0].marker, 1);
        assert!(reply.sources_flat_json.starts_with('['));
        assert!(reply.sources_flat_json.ends_with(']'));
    }

    #[test]
    fn mode_strict_serialises_as_strict() {
        let mut r = sample_result();
        r.effective_mode = Mode::Strict;
        assert_eq!(build(&r).mode, "strict");
    }

    #[test]
    fn mode_lenient_serialises_as_lenient() {
        let mut r = sample_result();
        r.effective_mode = Mode::Lenient;
        assert_eq!(build(&r).mode, "lenient");
    }

    #[test]
    fn citations_sorted_by_marker_ascending() {
        let mut r = sample_result();
        r.citations = vec![
            EnvCitation { marker: 3, urn: "urn:c".to_string() },
            EnvCitation { marker: 1, urn: "urn:a".to_string() },
            EnvCitation { marker: 2, urn: "urn:b".to_string() },
        ];
        let reply = build(&r);
        assert_eq!(
            reply.citations.iter().map(|c| c.marker).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn citation_same_marker_is_stable() {
        let mut r = sample_result();
        r.citations = vec![
            EnvCitation { marker: 1, urn: "urn:first".to_string() },
            EnvCitation { marker: 1, urn: "urn:second".to_string() },
        ];
        let reply = build(&r);
        assert_eq!(reply.citations[0].urn, "urn:first");
        assert_eq!(reply.citations[1].urn, "urn:second");
    }

    #[test]
    fn sources_flat_preserves_order_verbatim() {
        let mut r = sample_result();
        r.sources_flat = vec![
            SourceRow { urn: "urn:b".to_string(), payload: "{}".to_string() },
            SourceRow { urn: "urn:a".to_string(), payload: "{}".to_string() },
        ];
        let reply = build(&r);
        let pos_b = reply.sources_flat_json.find("urn:b").unwrap();
        let pos_a = reply.sources_flat_json.find("urn:a").unwrap();
        assert!(pos_b < pos_a, "RRF order must be preserved");
    }

    #[test]
    fn empty_sources_serialises_as_empty_array() {
        let mut r = sample_result();
        r.sources_flat = vec![];
        assert_eq!(build(&r).sources_flat_json, "[]");
    }

    #[test]
    fn empty_citations_yields_empty_vec_not_panic() {
        let mut r = sample_result();
        r.citations = vec![];
        assert!(build(&r).citations.is_empty());
    }

    #[test]
    fn sources_flat_json_keys_alphabetised() {
        let mut r = sample_result();
        r.sources_flat = vec![SourceRow {
            urn: "urn:x".to_string(),
            payload: "p".to_string(),
        }];
        let json = build(&r).sources_flat_json;
        let pos_payload = json.find("\"payload\"").unwrap();
        let pos_urn = json.find("\"urn\"").unwrap();
        assert!(pos_payload < pos_urn, "envelope parity: payload before urn");
    }

    #[test]
    fn sources_flat_json_escapes_quotes_and_backslashes() {
        let mut r = sample_result();
        r.sources_flat = vec![SourceRow {
            urn: "urn:row".to_string(),
            payload: "{\"k\":\"v\\\"\"}".to_string(),
        }];
        let json = build(&r).sources_flat_json;
        // Round-trip via serde_json: must parse to a JSON array of one object.
        let parsed: crate::serde_json::Value = crate::serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn sources_flat_json_escapes_control_chars() {
        let mut r = sample_result();
        r.sources_flat = vec![SourceRow {
            urn: "urn:row".to_string(),
            payload: "line1\nline2\ttab\u{0001}ctrl".to_string(),
        }];
        let json = build(&r).sources_flat_json;
        let parsed: crate::serde_json::Value = crate::serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        let payload = arr[0]["payload"].as_str().unwrap();
        assert!(payload.contains('\n'));
        assert!(payload.contains('\t'));
        assert!(payload.contains('\u{0001}'));
    }

    #[test]
    fn validation_warnings_and_errors_roundtrip() {
        let mut r = sample_result();
        r.validation = EnvValidation {
            ok: false,
            warnings: vec![ValidationWarning {
                kind: "malformed".to_string(),
                detail: "missing marker".to_string(),
            }],
            errors: vec![ValidationError {
                kind: "out_of_range".to_string(),
                detail: "marker > sources".to_string(),
            }],
        };
        let reply = build(&r);
        assert!(!reply.validation.ok);
        assert_eq!(reply.validation.warnings.len(), 1);
        assert_eq!(reply.validation.warnings[0].kind, "malformed");
        assert_eq!(reply.validation.warnings[0].detail, "missing marker");
        assert_eq!(reply.validation.errors.len(), 1);
        assert_eq!(reply.validation.errors[0].kind, "out_of_range");
    }

    #[test]
    fn cache_hit_records_zero_cost_and_tokens_when_zero() {
        let mut r = sample_result();
        r.cache_hit = true;
        r.prompt_tokens = 0;
        r.completion_tokens = 0;
        r.cost_usd = 0.0;
        let reply = build(&r);
        assert!(reply.cache_hit);
        assert_eq!(reply.prompt_tokens, 0);
        assert_eq!(reply.completion_tokens, 0);
        assert_eq!(reply.cost_usd, 0.0);
    }

    #[test]
    fn build_is_deterministic_across_calls() {
        let r = sample_result();
        assert_eq!(build(&r), build(&r));
    }

    #[test]
    fn does_not_expose_seed_or_temperature() {
        // Compile-time pin: `GrpcAskReply` has no seed/temperature fields.
        // Adding one would break this destructuring.
        let r = sample_result();
        let GrpcAskReply {
            answer: _,
            sources_flat_json: _,
            citations: _,
            validation: _,
            provider: _,
            model: _,
            prompt_tokens: _,
            completion_tokens: _,
            cost_usd: _,
            cache_hit: _,
            mode: _,
            retry_count: _,
        } = build(&r);
    }

    #[test]
    fn ask_reply_proto_tags_pinned() {
        assert_eq!(ask_reply::ANSWER, 1);
        assert_eq!(ask_reply::SOURCES_FLAT_JSON, 2);
        assert_eq!(ask_reply::CITATIONS, 3);
        assert_eq!(ask_reply::VALIDATION, 4);
        assert_eq!(ask_reply::PROVIDER, 5);
        assert_eq!(ask_reply::MODEL, 6);
        assert_eq!(ask_reply::PROMPT_TOKENS, 7);
        assert_eq!(ask_reply::COMPLETION_TOKENS, 8);
        assert_eq!(ask_reply::COST_USD, 9);
        assert_eq!(ask_reply::CACHE_HIT, 10);
        assert_eq!(ask_reply::MODE, 11);
        assert_eq!(ask_reply::RETRY_COUNT, 12);
    }

    #[test]
    fn ask_reply_proto_tags_are_unique_and_contiguous() {
        let tags = [
            ask_reply::ANSWER,
            ask_reply::SOURCES_FLAT_JSON,
            ask_reply::CITATIONS,
            ask_reply::VALIDATION,
            ask_reply::PROVIDER,
            ask_reply::MODEL,
            ask_reply::PROMPT_TOKENS,
            ask_reply::COMPLETION_TOKENS,
            ask_reply::COST_USD,
            ask_reply::CACHE_HIT,
            ask_reply::MODE,
            ask_reply::RETRY_COUNT,
        ];
        let mut sorted = tags.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len(), "duplicate proto field tag");
        assert_eq!(sorted, (1u32..=tags.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn nested_message_proto_tags_pinned() {
        assert_eq!(citation::MARKER, 1);
        assert_eq!(citation::URN, 2);
        assert_eq!(validation::OK, 1);
        assert_eq!(validation::WARNINGS, 2);
        assert_eq!(validation::ERRORS, 3);
        assert_eq!(validation_item::KIND, 1);
        assert_eq!(validation_item::DETAIL, 2);
    }

    #[test]
    fn field_set_matches_json_envelope() {
        // Parity check: every top-level key in the JSON envelope must
        // map to a GrpcAskReply field. If the envelope grows, this
        // test forces a matching GrpcAskReply field + proto tag.
        let r = sample_result();
        let envelope =
            crate::runtime::ai::ask_response_envelope::build(&r);
        let keys: Vec<&str> = envelope
            .as_object()
            .unwrap()
            .keys()
            .map(|s| s.as_str())
            .collect();
        let expected = [
            "answer",
            "cache_hit",
            "citations",
            "completion_tokens",
            "cost_usd",
            "mode",
            "model",
            "prompt_tokens",
            "provider",
            "retry_count",
            "sources_flat",
            "validation",
        ];
        for k in &expected {
            assert!(keys.contains(k), "envelope missing key {k}");
        }
        assert_eq!(keys.len(), expected.len(), "envelope keys drift detected");
    }
}
