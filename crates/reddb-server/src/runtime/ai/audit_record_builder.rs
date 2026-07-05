//! `AuditRecordBuilder` — pure builder for `red_ask_audit` rows.
//!
//! Issue #402 (PRD #391): every ASK call writes one row to the
//! `red_ask_audit` system collection. This module owns the *shape* of
//! that row: which fields exist, how `answer_hash` is computed, what
//! `include_answer` toggles, what the keys look like on the wire.
//!
//! Deep module: no I/O, no clock, no collection access. Inputs are
//! plain data (the call state assembled by `execute_ask`); the output
//! is a [`BTreeMap`] ready to be passed to the insert path. Keeping
//! the builder pure means the audit schema is pinned by unit tests
//! and can't drift behind the spec.
//!
//! ## Field policy
//!
//! Always present (PRD §audit, ADR-319 `red_*` convention):
//!
//! - `ts` — caller-injected wall time in epoch nanoseconds. The
//!   builder takes it as input rather than reading the clock so that
//!   tests are deterministic; production callers feed
//!   `SystemTime::now()`.
//! - `tenant`, `user`, `role` — identity columns. Empty strings are
//!   allowed (e.g. embedded usage with no auth context) so the audit
//!   row still lands.
//! - `question` — verbatim user question.
//! - `sources_urns` — JSON array of stable source URNs (post-fusion,
//!   pre-redaction). Order is preserved from the input.
//! - `provider`, `model` — provider token and model id sent to the
//!   LLM. Both are recorded as the caller actually used them, not
//!   what the user requested, so the determinism contract (#400) and
//!   the capability fallback (#396) are auditable.
//! - `prompt_tokens`, `completion_tokens` — usage counters reported
//!   by the provider. Stored as `i64` (some providers can return 0).
//! - `cost_usd` — cost accrued for this call, `f64`. Always recorded
//!   even on cache hits (zero in that case).
//! - `answer_hash` — lowercase hex SHA-256 of the answer string.
//!   Deterministic; recorded regardless of `include_answer`.
//! - `citations` — JSON array of 1-indexed citation markers found in
//!   the answer (from `CitationParser`).
//! - `cache_hit` — boolean; `true` when answered from the answer
//!   cache (#403) without calling the LLM.
//! - `mode` — `"strict"` or `"lenient"`, the mode *effectively* used
//!   after any provider-capability fallback (#396).
//! - `temperature`, `seed` — determinism knobs actually sent to the
//!   provider. `null` means the selected provider does not support that
//!   knob, not that a default was forgotten.
//! - `validation_ok` — boolean; `true` iff the citation validator
//!   (#395) returned `Decision::Ok` for the final answer.
//! - `retry_count` — `0` or `1`. The strict-mode retry budget is
//!   pinned at one (#395), but we record the count so a future budget
//!   change doesn't silently corrupt the schema.
//! - `errors` — JSON array of `{kind, detail}` objects. Empty when
//!   the call succeeded.
//!
//! Conditional:
//!
//! - `answer` — full answer string. Only present when
//!   `Settings::include_answer == true`. Default is `false`; the
//!   `ask.audit.include_answer` setting flips it on per deployment.
//!   The shape change is explicit (key absent vs key present) so
//!   downstream consumers can detect it without sentinels.
//!
//! ## Why a deep module
//!
//! The schema is a contract: operators write dashboards against it,
//! retention purges read `ts` directly, replication forwarders
//! serialize it. Concentrating the build logic in one place — with
//! every field pinned by a test — keeps the contract honest. The
//! `execute_ask` glue can then call `AuditRecordBuilder::build(...)`
//! and treat the result as opaque key/value rows.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::json;
use crate::runtime::ai::strict_validator::{Mode, ValidationError, ValidationErrorKind};
use crate::serde_json::Value;

/// Deployment-level audit settings.
///
/// Surfaced via the `ask.audit.*` settings tree; threaded into the
/// builder so tests can pin both shapes without touching global
/// config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// When `true`, store the full answer string under the `answer`
    /// key in addition to `answer_hash`. Default `false`.
    pub include_answer: bool,
}

/// All call state needed to render one audit row. Plain data —
/// borrowed where cheap, owned only for the fields the caller almost
/// always owns at insert time.
#[derive(Debug, Clone)]
pub struct CallState<'a> {
    /// Epoch nanoseconds. Injected by the caller (do not read the
    /// clock inside the builder).
    pub ts_nanos: i64,
    pub tenant: &'a str,
    pub user: &'a str,
    pub role: &'a str,
    pub question: &'a str,
    pub sources_urns: &'a [String],
    pub provider: &'a str,
    pub model: &'a str,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd: f64,
    /// The full LLM answer text. Always hashed; only stored when
    /// `Settings::include_answer == true`.
    pub answer: &'a str,
    /// 1-indexed citation markers parsed from the answer.
    pub citations: &'a [u32],
    pub cache_hit: bool,
    /// The mode effectively used — after provider capability fallback
    /// (#396), not the mode the user asked for.
    pub effective_mode: Mode,
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
    pub validation_ok: bool,
    pub retry_count: u32,
    pub errors: &'a [ValidationError],
    /// Planner-first fields (ADR 0068 / #1747). `None` on the RAG path so
    /// the row shape is unchanged; `Some` on the planner path, where the
    /// audit row grows the routed intent, plan summary, and executed query.
    pub intent: Option<&'a str>,
    pub plan_summary: Option<&'a str>,
    pub executed_query: Option<&'a str>,
}

/// Produce one audit row, ready to insert into `red_ask_audit`.
///
/// The keys are stable (pinned by tests) and the value types match
/// what the storage layer expects for the collection's columns. The
/// `BTreeMap` ordering is alphabetical; downstream consumers MUST NOT
/// rely on insertion order, but operators reading raw rows benefit
/// from the predictable layout.
pub fn build(state: &CallState<'_>, settings: Settings) -> BTreeMap<&'static str, Value> {
    let mut row: BTreeMap<&'static str, Value> = BTreeMap::new();

    row.insert("ts", json!(state.ts_nanos));
    row.insert("tenant", json!(state.tenant));
    row.insert("user", json!(state.user));
    row.insert("role", json!(state.role));
    row.insert("question", json!(state.question));
    row.insert("sources_urns", json!(state.sources_urns));
    row.insert("provider", json!(state.provider));
    row.insert("model", json!(state.model));
    row.insert("prompt_tokens", json!(state.prompt_tokens));
    row.insert("completion_tokens", json!(state.completion_tokens));
    row.insert("cost_usd", json!(state.cost_usd));
    row.insert("answer_hash", json!(answer_hash(state.answer)));
    row.insert("citations", json!(state.citations));
    row.insert("cache_hit", json!(state.cache_hit));
    row.insert("mode", json!(mode_str(state.effective_mode)));
    row.insert(
        "temperature",
        state
            .temperature
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    row.insert(
        "seed",
        state.seed.map(|value| json!(value)).unwrap_or(Value::Null),
    );
    row.insert("validation_ok", json!(state.validation_ok));
    row.insert("retry_count", json!(state.retry_count));
    row.insert(
        "errors",
        Value::Array(state.errors.iter().map(error_json).collect()),
    );

    if settings.include_answer {
        row.insert("answer", json!(state.answer));
    }

    // Planner-first fields (#1747): present only on the planner path.
    if let Some(intent) = state.intent {
        row.insert("intent", json!(intent));
    }
    if let Some(plan_summary) = state.plan_summary {
        row.insert("plan_summary", json!(plan_summary));
    }
    if let Some(executed_query) = state.executed_query {
        row.insert("executed_query", json!(executed_query));
    }

    row
}

/// Lowercase hex SHA-256 of the answer text. Deterministic; the
/// audit row is identical for byte-equal answers regardless of when
/// or where the call ran.
pub fn answer_hash(answer: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(answer.as_bytes());
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Strict => "strict",
        Mode::Lenient => "lenient",
    }
}

fn error_kind_str(kind: ValidationErrorKind) -> &'static str {
    match kind {
        ValidationErrorKind::Malformed => "malformed",
        ValidationErrorKind::OutOfRange => "out_of_range",
    }
}

fn error_json(err: &ValidationError) -> Value {
    json!({
        "kind": error_kind_str(err.kind),
        "detail": err.detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state<'a>(
        question: &'a str,
        urns: &'a [String],
        answer: &'a str,
        citations: &'a [u32],
        errors: &'a [ValidationError],
    ) -> CallState<'a> {
        CallState {
            ts_nanos: 1_700_000_000_000_000_000,
            tenant: "acme",
            user: "alice",
            role: "analyst",
            question,
            sources_urns: urns,
            provider: "openai",
            model: "gpt-4o-mini",
            prompt_tokens: 123,
            completion_tokens: 45,
            cost_usd: 0.0012,
            answer,
            citations,
            cache_hit: false,
            effective_mode: Mode::Strict,
            temperature: Some(0.0),
            seed: Some(42),
            validation_ok: true,
            retry_count: 0,
            errors,
            intent: None,
            plan_summary: None,
            executed_query: None,
        }
    }

    #[test]
    fn planner_fields_present_only_when_set() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let mut state = base_state("q?", &urns, "answer", &citations, &errors);
        let row = build(&state, Settings::default());
        assert!(!row.contains_key("intent"));
        assert!(!row.contains_key("plan_summary"));
        assert!(!row.contains_key("executed_query"));

        state.intent = Some("factual");
        state.plan_summary = Some("intent=factual; query=SELECT * FROM t WHERE a = 'x'");
        state.executed_query = Some("SELECT * FROM t WHERE a = 'x'");
        let row = build(&state, Settings::default());
        assert_eq!(row.get("intent"), Some(&json!("factual")));
        assert_eq!(
            row.get("executed_query"),
            Some(&json!("SELECT * FROM t WHERE a = 'x'"))
        );
        assert!(row.contains_key("plan_summary"));
    }

    // ---- answer_hash ----------------------------------------------------

    #[test]
    fn answer_hash_is_deterministic_sha256() {
        // Known SHA-256 of empty string.
        assert_eq!(
            answer_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn answer_hash_known_value_for_short_string() {
        // sha256("hello") — pinned so a regression in hashing is loud.
        assert_eq!(
            answer_hash("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn answer_hash_repeated_calls_byte_equal() {
        let a = answer_hash("the cat sat on the mat");
        let b = answer_hash("the cat sat on the mat");
        assert_eq!(a, b);
    }

    #[test]
    fn answer_hash_differs_for_differing_input() {
        assert_ne!(answer_hash("a"), answer_hash("b"));
    }

    // ---- core schema ---------------------------------------------------

    #[test]
    fn build_emits_every_required_field() {
        let urns = vec!["urn:a".to_string(), "urn:b".to_string()];
        let citations = vec![1u32, 2];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q?", &urns, "answer text", &citations, &errors);

        let row = build(&state, Settings::default());

        for key in [
            "ts",
            "tenant",
            "user",
            "role",
            "question",
            "sources_urns",
            "provider",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cost_usd",
            "answer_hash",
            "citations",
            "cache_hit",
            "mode",
            "temperature",
            "seed",
            "validation_ok",
            "retry_count",
            "errors",
        ] {
            assert!(row.contains_key(key), "row missing required field `{key}`");
        }
    }

    #[test]
    fn build_field_values_match_state() {
        let urns = vec!["urn:x".to_string()];
        let citations = vec![3u32];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("why?", &urns, "because", &citations, &errors);

        let row = build(&state, Settings::default());

        assert_eq!(row["ts"], json!(1_700_000_000_000_000_000_i64));
        assert_eq!(row["tenant"], json!("acme"));
        assert_eq!(row["user"], json!("alice"));
        assert_eq!(row["role"], json!("analyst"));
        assert_eq!(row["question"], json!("why?"));
        assert_eq!(row["sources_urns"], json!(["urn:x"]));
        assert_eq!(row["provider"], json!("openai"));
        assert_eq!(row["model"], json!("gpt-4o-mini"));
        assert_eq!(row["prompt_tokens"], json!(123));
        assert_eq!(row["completion_tokens"], json!(45));
        assert_eq!(row["cost_usd"], json!(0.0012));
        assert_eq!(row["answer_hash"], json!(answer_hash("because")));
        assert_eq!(row["citations"], json!([3]));
        assert_eq!(row["cache_hit"], json!(false));
        assert_eq!(row["mode"], json!("strict"));
        assert_eq!(row["temperature"], json!(0.0));
        assert_eq!(row["seed"], json!(42u64));
        assert_eq!(row["validation_ok"], json!(true));
        assert_eq!(row["retry_count"], json!(0));
        assert_eq!(row["errors"], json!([]));
    }

    #[test]
    fn unsupported_determinism_knobs_are_recorded_as_null() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let mut state = base_state("q", &urns, "a", &citations, &errors);
        state.temperature = None;
        state.seed = None;

        let row = build(&state, Settings::default());

        assert_eq!(row["temperature"], Value::Null);
        assert_eq!(row["seed"], Value::Null);
    }

    // ---- include_answer toggle -----------------------------------------

    #[test]
    fn answer_field_absent_by_default() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q", &urns, "secret answer", &citations, &errors);

        let row = build(&state, Settings::default());

        assert!(!row.contains_key("answer"));
        // Hash is still recorded — operators can compare hashes
        // without the answer itself.
        assert_eq!(row["answer_hash"], json!(answer_hash("secret answer")));
    }

    #[test]
    fn answer_field_present_when_include_answer_set() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q", &urns, "full text", &citations, &errors);

        let row = build(
            &state,
            Settings {
                include_answer: true,
            },
        );

        assert_eq!(row["answer"], json!("full text"));
        // Hash is still present — toggling the flag must not silently
        // remove other fields.
        assert_eq!(row["answer_hash"], json!(answer_hash("full text")));
    }

    // ---- mode + validation --------------------------------------------

    #[test]
    fn lenient_mode_serializes_as_lenient_string() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let mut state = base_state("q", &urns, "a", &citations, &errors);
        state.effective_mode = Mode::Lenient;

        let row = build(&state, Settings::default());

        assert_eq!(row["mode"], json!("lenient"));
    }

    #[test]
    fn errors_round_trip_with_kind_and_detail() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors = vec![
            ValidationError {
                kind: ValidationErrorKind::Malformed,
                detail: "empty marker body".to_string(),
            },
            ValidationError {
                kind: ValidationErrorKind::OutOfRange,
                detail: "marker [^9] references source #9".to_string(),
            },
        ];
        let mut state = base_state("q", &urns, "a", &citations, &errors);
        state.validation_ok = false;
        state.retry_count = 1;

        let row = build(&state, Settings::default());

        assert_eq!(row["validation_ok"], json!(false));
        assert_eq!(row["retry_count"], json!(1));
        assert_eq!(
            row["errors"],
            json!([
                json!({"kind": "malformed", "detail": "empty marker body"}),
                json!({"kind": "out_of_range", "detail": "marker [^9] references source #9"}),
            ])
        );
    }

    // ---- cache hit ------------------------------------------------------

    #[test]
    fn cache_hit_recorded() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let mut state = base_state("q", &urns, "cached", &citations, &errors);
        state.cache_hit = true;
        state.prompt_tokens = 0;
        state.completion_tokens = 0;
        state.cost_usd = 0.0;

        let row = build(&state, Settings::default());

        assert_eq!(row["cache_hit"], json!(true));
        // Cost is recorded even when zero — downstream sum() must not
        // see a missing-field surprise.
        assert_eq!(row["cost_usd"], json!(0.0));
        assert_eq!(row["prompt_tokens"], json!(0));
    }

    // ---- edge cases ----------------------------------------------------

    #[test]
    fn empty_identity_fields_allowed() {
        // Embedded use with no auth context — we still want the row
        // to land. Empty strings serialize as empty strings, not null.
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let mut state = base_state("q", &urns, "a", &citations, &errors);
        state.tenant = "";
        state.user = "";
        state.role = "";

        let row = build(&state, Settings::default());

        assert_eq!(row["tenant"], json!(""));
        assert_eq!(row["user"], json!(""));
        assert_eq!(row["role"], json!(""));
    }

    #[test]
    fn empty_sources_serializes_as_empty_array() {
        let urns: Vec<String> = vec![];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q", &urns, "a", &citations, &errors);

        let row = build(&state, Settings::default());

        assert_eq!(row["sources_urns"], json!([]));
        assert_eq!(row["citations"], json!([]));
        assert_eq!(row["errors"], json!([]));
    }

    #[test]
    fn sources_order_preserved() {
        // RRF (#398) emits a *ranked* list — the audit row MUST keep
        // that ranking so post-hoc analysis of "what did the model
        // see, in what order" stays honest.
        let urns = vec![
            "urn:c".to_string(),
            "urn:a".to_string(),
            "urn:b".to_string(),
        ];
        let citations: Vec<u32> = vec![];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q", &urns, "a", &citations, &errors);

        let row = build(&state, Settings::default());

        assert_eq!(row["sources_urns"], json!(["urn:c", "urn:a", "urn:b"]));
    }

    #[test]
    fn build_is_deterministic_across_calls() {
        // Same inputs → byte-equal rows. Required by the ASK
        // determinism contract (#400): the audit trail must not
        // depend on map-randomization or any clock side-effect.
        let urns = vec!["urn:a".to_string()];
        let citations = vec![1u32];
        let errors: Vec<ValidationError> = vec![];
        let state = base_state("q", &urns, "a", &citations, &errors);

        let a = build(&state, Settings::default());
        let b = build(&state, Settings::default());
        assert_eq!(a, b);
    }
}
