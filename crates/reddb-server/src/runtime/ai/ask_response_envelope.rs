//! `AskResponseEnvelope` — pure serializer for the canonical
//! non-streaming ASK JSON response (issue #406, PRD #391).
//!
//! Deep module: no I/O, no transport, no clock. Owns the on-wire JSON
//! shape that the embedded stdio JSON-RPC `query` method returns, and
//! that gRPC (#407), Postgres-wire (#408), and MCP non-stream (#409)
//! all embed verbatim. Pinning the shape here means a future transport
//! slice cannot accidentally drop `citations`, rename `cache_hit`, or
//! re-shape `validation` without the tests in this file failing first.
//!
//! ## Why a separate module
//!
//! ASK has six new fields the legacy bucketed response did not carry
//! (`answer`, `sources_flat`, `citations`, `validation`, `cache_hit`,
//! `cost_usd`). The acceptance criteria for #406 require every one
//! present in the JSON-RPC response. That's a "field-presence" bug
//! surface — easy to forget one when the wiring slice lands and hard
//! to notice in review. Building the JSON in a tested deep module
//! keeps the wiring slice focused on "where do I write these bytes"
//! and the contract here on "are the bytes right".
//!
//! ## Shape pinned by tests
//!
//! Top-level keys (alphabetised by the `BTreeMap`-backed encoder):
//!
//! - `answer` — full answer text with inline `[^N]` markers.
//! - `cache_hit` — bool.
//! - `citations` — `[{marker, urn}]`, sorted by marker ascending.
//! - `completion_tokens` — number.
//! - `cost_usd` — number.
//! - `mode` — `"strict"` or `"lenient"`, the *effective* mode after
//!   provider-capability fallback (#396) — mirrors the audit row #402.
//! - `model` — string.
//! - `prompt_tokens` — number.
//! - `provider` — string.
//! - `retry_count` — number (0 or 1 per #395's one-retry budget).
//! - `sources_flat` — `[{payload, urn}]`, post-RRF (#398) order
//!   preserved verbatim so the client can map `[^N]` → `sources_flat[N-1]`.
//! - `validation` — `{errors, ok, warnings}`. Errors and warnings are
//!   `[{detail, kind}]` to match the shapes audit_record_builder (#402)
//!   and sse_frame_encoder (#405) already pin.
//!
//! Determinism = seed (#400) is *not* in the response. It's recorded
//! in the audit row, not surfaced to the caller — leaking the seed
//! would let a hostile caller replay deterministic answers.

use crate::serde_json::{Map, Value};

/// One row from `sources_flat`. `urn` is the engine entity URN,
/// `payload` is the column-policy-redacted JSON serialised as a
/// string so the envelope JSON stays flat (the client re-parses if
/// it wants structure — matches the SSE `sources` frame shape #405).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    pub urn: String,
    pub payload: String,
}

/// One citation: `[^N]` in the answer ↔ `sources_flat[N-1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Citation {
    pub marker: u32,
    pub urn: String,
}

/// One validation warning. Same shape as the SSE terminal frame so
/// HTTP clients can share parsing code across streaming and non-
/// streaming paths.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationWarning {
    pub kind: String,
    pub detail: String,
}

/// One validation error. Same shape as warnings; `kind` is one of
/// `"malformed"` / `"out_of_range"` per #395's `ValidationErrorKind`.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub kind: String,
    pub detail: String,
}

/// Validation block. `ok = false` with non-empty `errors` corresponds
/// to the HTTP 422 path on retry exhaustion (#395).
#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    pub ok: bool,
    pub warnings: Vec<ValidationWarning>,
    pub errors: Vec<ValidationError>,
}

/// Effective mode actually applied — *after* provider-capability
/// fallback (#396). The originally-requested mode is recorded in the
/// audit row, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Strict,
    Lenient,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Strict => "strict",
            Mode::Lenient => "lenient",
        }
    }
}

/// Internal result a non-streaming ASK call produces — input to
/// [`build`]. The wiring slice (deferred) constructs this from
/// `execute_ask`'s outputs.
#[derive(Debug, Clone)]
pub struct AskResult {
    pub answer: String,
    pub sources_flat: Vec<SourceRow>,
    pub citations: Vec<Citation>,
    pub validation: Validation,
    pub cache_hit: bool,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_usd: f64,
    pub effective_mode: Mode,
    pub retry_count: u32,
}

/// Serialise an [`AskResult`] to its canonical JSON envelope.
///
/// Output is a `Value::Object` ready to drop into a JSON-RPC `result`
/// field, a gRPC message, or a Postgres-wire single-row result set.
/// Re-running on byte-equal input is byte-equal output (pinned by
/// `build_is_deterministic_across_calls`) — required by the ASK
/// determinism contract (#400).
pub fn build(result: &AskResult) -> Value {
    let mut m = Map::new();
    m.insert("answer".into(), Value::String(result.answer.clone()));
    m.insert("cache_hit".into(), Value::Bool(result.cache_hit));
    m.insert("citations".into(), citations_value(&result.citations));
    m.insert(
        "completion_tokens".into(),
        Value::Number(result.completion_tokens as f64),
    );
    m.insert("cost_usd".into(), Value::Number(result.cost_usd));
    m.insert("mode".into(), Value::String(result.effective_mode.as_str().into()));
    m.insert("model".into(), Value::String(result.model.clone()));
    m.insert(
        "prompt_tokens".into(),
        Value::Number(result.prompt_tokens as f64),
    );
    m.insert("provider".into(), Value::String(result.provider.clone()));
    m.insert("retry_count".into(), Value::Number(result.retry_count as f64));
    m.insert("sources_flat".into(), sources_value(&result.sources_flat));
    m.insert("validation".into(), validation_value(&result.validation));
    Value::Object(m)
}

fn citations_value(cites: &[Citation]) -> Value {
    // Marker order is the contract — `[^1]` must come before `[^2]`
    // in the array so the index aligns with the marker. Pinned by
    // `citations_are_sorted_by_marker_ascending`.
    let mut sorted: Vec<Citation> = cites.to_vec();
    sorted.sort_by_key(|c| c.marker);
    Value::Array(
        sorted
            .iter()
            .map(|c| {
                let mut o = Map::new();
                o.insert("marker".into(), Value::Number(c.marker as f64));
                o.insert("urn".into(), Value::String(c.urn.clone()));
                Value::Object(o)
            })
            .collect(),
    )
}

fn sources_value(rows: &[SourceRow]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("payload".into(), Value::String(r.payload.clone()));
                o.insert("urn".into(), Value::String(r.urn.clone()));
                Value::Object(o)
            })
            .collect(),
    )
}

fn warning_value(w: &ValidationWarning) -> Value {
    let mut o = Map::new();
    o.insert("detail".into(), Value::String(w.detail.clone()));
    o.insert("kind".into(), Value::String(w.kind.clone()));
    Value::Object(o)
}

fn error_value(e: &ValidationError) -> Value {
    let mut o = Map::new();
    o.insert("detail".into(), Value::String(e.detail.clone()));
    o.insert("kind".into(), Value::String(e.kind.clone()));
    Value::Object(o)
}

fn validation_value(v: &Validation) -> Value {
    let mut o = Map::new();
    o.insert(
        "errors".into(),
        Value::Array(v.errors.iter().map(error_value).collect()),
    );
    o.insert("ok".into(), Value::Bool(v.ok));
    o.insert(
        "warnings".into(),
        Value::Array(v.warnings.iter().map(warning_value).collect()),
    );
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AskResult {
        AskResult {
            answer: "X is 42 [^1].".into(),
            sources_flat: vec![SourceRow {
                urn: "urn:reddb:row:1".into(),
                payload: "{\"k\":\"v\"}".into(),
            }],
            citations: vec![Citation {
                marker: 1,
                urn: "urn:reddb:row:1".into(),
            }],
            validation: Validation {
                ok: true,
                warnings: vec![],
                errors: vec![],
            },
            cache_hit: false,
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt_tokens: 123,
            completion_tokens: 45,
            cost_usd: 0.000_321,
            effective_mode: Mode::Strict,
            retry_count: 0,
        }
    }

    #[test]
    fn build_emits_every_required_key() {
        let v = build(&fixture());
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
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
            ]
        );
    }

    #[test]
    fn answer_text_preserved_with_inline_markers() {
        let v = build(&fixture());
        assert_eq!(v.get("answer").and_then(|x| x.as_str()), Some("X is 42 [^1]."));
    }

    #[test]
    fn cache_hit_serializes_as_bool() {
        let mut r = fixture();
        r.cache_hit = true;
        let v = build(&r);
        assert_eq!(v.get("cache_hit").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn citations_are_sorted_by_marker_ascending() {
        let mut r = fixture();
        r.citations = vec![
            Citation {
                marker: 3,
                urn: "urn:c".into(),
            },
            Citation {
                marker: 1,
                urn: "urn:a".into(),
            },
            Citation {
                marker: 2,
                urn: "urn:b".into(),
            },
        ];
        let v = build(&r);
        let arr = v.get("citations").and_then(|x| x.as_array()).unwrap();
        let markers: Vec<u64> = arr
            .iter()
            .map(|c| c.get("marker").and_then(|m| m.as_u64()).unwrap())
            .collect();
        assert_eq!(markers, vec![1, 2, 3]);
    }

    #[test]
    fn sources_flat_preserves_input_order() {
        // Post-RRF order is the contract — `[^N]` indexes into this
        // array, so reordering would silently break grounding.
        let mut r = fixture();
        r.sources_flat = vec![
            SourceRow {
                urn: "urn:z".into(),
                payload: "{}".into(),
            },
            SourceRow {
                urn: "urn:a".into(),
                payload: "{}".into(),
            },
        ];
        let v = build(&r);
        let arr = v.get("sources_flat").and_then(|x| x.as_array()).unwrap();
        assert_eq!(
            arr[0].get("urn").and_then(|x| x.as_str()),
            Some("urn:z")
        );
        assert_eq!(
            arr[1].get("urn").and_then(|x| x.as_str()),
            Some("urn:a")
        );
    }

    #[test]
    fn sources_row_carries_payload_as_string() {
        let v = build(&fixture());
        let arr = v.get("sources_flat").and_then(|x| x.as_array()).unwrap();
        assert_eq!(
            arr[0].get("payload").and_then(|x| x.as_str()),
            Some("{\"k\":\"v\"}")
        );
    }

    #[test]
    fn validation_ok_carries_empty_arrays() {
        let v = build(&fixture());
        let val = v.get("validation").unwrap();
        assert_eq!(val.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(val.get("warnings").and_then(|x| x.as_array()).unwrap().len(), 0);
        assert_eq!(val.get("errors").and_then(|x| x.as_array()).unwrap().len(), 0);
    }

    #[test]
    fn validation_carries_warnings_and_errors_with_kind_detail() {
        let mut r = fixture();
        r.validation = Validation {
            ok: false,
            warnings: vec![ValidationWarning {
                kind: "mode_fallback".into(),
                detail: "ollama".into(),
            }],
            errors: vec![ValidationError {
                kind: "out_of_range".into(),
                detail: "marker 7 > 3 sources".into(),
            }],
        };
        let v = build(&r);
        let val = v.get("validation").unwrap();
        assert_eq!(val.get("ok").and_then(|x| x.as_bool()), Some(false));
        let warns = val.get("warnings").and_then(|x| x.as_array()).unwrap();
        assert_eq!(warns[0].get("kind").and_then(|x| x.as_str()), Some("mode_fallback"));
        assert_eq!(warns[0].get("detail").and_then(|x| x.as_str()), Some("ollama"));
        let errs = val.get("errors").and_then(|x| x.as_array()).unwrap();
        assert_eq!(errs[0].get("kind").and_then(|x| x.as_str()), Some("out_of_range"));
    }

    #[test]
    fn mode_serializes_as_strict_or_lenient() {
        let mut r = fixture();
        r.effective_mode = Mode::Strict;
        assert_eq!(build(&r).get("mode").and_then(|x| x.as_str()), Some("strict"));
        r.effective_mode = Mode::Lenient;
        assert_eq!(build(&r).get("mode").and_then(|x| x.as_str()), Some("lenient"));
    }

    #[test]
    fn usage_fields_flat_at_top_level() {
        // Matches the audit row shape (#402) and SSE audit frame
        // (#405). Nested `usage: {...}` would force every transport
        // and SDK to re-shape.
        let v = build(&fixture());
        assert_eq!(v.get("prompt_tokens").and_then(|x| x.as_u64()), Some(123));
        assert_eq!(v.get("completion_tokens").and_then(|x| x.as_u64()), Some(45));
        assert!(v.get("cost_usd").is_some());
    }

    #[test]
    fn cost_usd_keeps_fractional_precision() {
        let mut r = fixture();
        r.cost_usd = 0.000_321;
        let v = build(&r);
        assert_eq!(v.get("cost_usd").and_then(|x| x.as_f64()), Some(0.000_321));
    }

    #[test]
    fn retry_count_zero_and_one_both_round_trip() {
        // #395 caps retries at one — pinning both endpoints guards
        // against an off-by-one if the budget ever changes.
        let mut r = fixture();
        r.retry_count = 0;
        assert_eq!(
            build(&r).get("retry_count").and_then(|x| x.as_u64()),
            Some(0)
        );
        r.retry_count = 1;
        assert_eq!(
            build(&r).get("retry_count").and_then(|x| x.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn does_not_expose_seed_or_temperature() {
        // Determinism inputs (#400) are recorded in the audit row,
        // not surfaced to the caller. Leaking the seed would let a
        // hostile caller replay deterministic answers.
        let v = build(&fixture());
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("seed"));
        assert!(!obj.contains_key("temperature"));
    }

    #[test]
    fn empty_sources_and_citations_are_arrays_not_null() {
        // Empty arrays are well-formed (`STRICT OFF` on a refusal can
        // produce no citations). Missing keys would break a downstream
        // `.length` access.
        let mut r = fixture();
        r.sources_flat = vec![];
        r.citations = vec![];
        let v = build(&r);
        assert!(v.get("sources_flat").and_then(|x| x.as_array()).unwrap().is_empty());
        assert!(v.get("citations").and_then(|x| x.as_array()).unwrap().is_empty());
    }

    #[test]
    fn answer_escaping_handled_by_compact_encoder() {
        let mut r = fixture();
        r.answer = "she said \"hi\"\nnewline".into();
        let bytes = build(&r).to_string_compact();
        assert!(bytes.contains(r#"\"hi\""#));
        assert!(bytes.contains(r#"\n"#));
    }

    #[test]
    fn build_is_deterministic_across_calls() {
        let r = fixture();
        let a = build(&r).to_string_compact();
        let b = build(&r).to_string_compact();
        assert_eq!(a, b);
    }

    #[test]
    fn build_is_deterministic_across_clone_inputs() {
        let r1 = fixture();
        let r2 = r1.clone();
        assert_eq!(build(&r1).to_string_compact(), build(&r2).to_string_compact());
    }

    #[test]
    fn top_level_key_order_is_alphabetical() {
        // Pinned because clients on weak parsers (e.g. some PG-wire
        // bindings doing string slicing) have been known to depend on
        // it. BTreeMap-backed encoder gives it for free.
        let bytes = build(&fixture()).to_string_compact();
        let answer_pos = bytes.find("\"answer\"").unwrap();
        let cache_pos = bytes.find("\"cache_hit\"").unwrap();
        let citations_pos = bytes.find("\"citations\"").unwrap();
        let validation_pos = bytes.find("\"validation\"").unwrap();
        assert!(answer_pos < cache_pos);
        assert!(cache_pos < citations_pos);
        assert!(citations_pos < validation_pos);
    }

    #[test]
    fn citation_with_same_marker_is_stable_under_sort() {
        // Defensive: if two citations share a marker (malformed input
        // from the validator path), the sort must be stable so the
        // input order is preserved. Pinned because a different sort
        // strategy (unstable + tie on marker) would non-determinise
        // the response and break #400.
        let mut r = fixture();
        r.citations = vec![
            Citation {
                marker: 1,
                urn: "urn:first".into(),
            },
            Citation {
                marker: 1,
                urn: "urn:second".into(),
            },
        ];
        let v = build(&r);
        let arr = v.get("citations").and_then(|x| x.as_array()).unwrap();
        assert_eq!(arr[0].get("urn").and_then(|x| x.as_str()), Some("urn:first"));
        assert_eq!(arr[1].get("urn").and_then(|x| x.as_str()), Some("urn:second"));
    }
}
