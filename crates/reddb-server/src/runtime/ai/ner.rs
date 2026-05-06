//! LLM-based NER for AskPipeline Stage 1 — issue #123.
//!
//! Opt-in replacement for the heuristic `extract_tokens` regex pass in
//! [`crate::runtime::ask_pipeline`]. Default backend stays heuristic;
//! when an operator turns on `ai.ner.backend = "llm"` (config knob —
//! flagged for follow-up wiring), `AskPipeline::extract_tokens` is
//! routed through [`LlmNer::extract`] instead.
//!
//! Design notes
//! ------------
//! * **Pure module.** This file imports [`TokenSet`] / [`EffectiveScope`]
//!   from `runtime::ask_pipeline` and `runtime::statement_frame` via
//!   `use` only. It never edits those modules — registration is a
//!   separate orchestrator-batch step.
//! * **Auth gate.** Mirrors ADR 0008: every LLM call checks
//!   `ai:ner:read` against an `AuthContext` trait defined locally.
//!   Production callers will plug the auth store in; tests use
//!   [`StubAuthContext`].
//! * **Output sanitization.** LLM responses are JSON-parsed and every
//!   token is run through the same defenses the rest of the engine
//!   uses on untrusted strings: control-byte / CRLF / NUL / quote
//!   injection rejection plus a secret-redactor pattern check (so a
//!   hallucinated `sk_live_...` or `Bearer ...` never leaks into the
//!   pipeline as a "literal").
//! * **Token cap.** Bounded at `max_tokens_returned` per call (default
//!   32). Excess returns [`NerError::ResponseExceedsTokenLimit`]
//!   rather than silently truncating, so callers can record a metric.
//! * **Stub variant.** [`NerProvider::Stub`] never touches the network
//!   — every test below uses it.
//!
//! Inline `#[cfg(test)] mod tests` covers stub Empty/Echo/Canned,
//! timeout simulation, malformed-response rejection, secret-in-response
//! rejection, token-cap enforcement, auth-gate denial, and each
//! [`HeuristicFallback`] variant.

use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::runtime::ask_pipeline::{extract_tokens as heuristic_extract_tokens, TokenSet};
use crate::runtime::statement_frame::EffectiveScope;

/// Default per-call token budget. Anything above this returns
/// [`NerError::ResponseExceedsTokenLimit`].
pub const DEFAULT_MAX_TOKENS: usize = 32;

/// Default LLM call timeout, in milliseconds. 5 seconds matches the
/// AskPipeline budget allocated to Stage 1 in PRD #118.
pub const DEFAULT_TIMEOUT_MS: u32 = 5_000;

/// Capability string the auth gate looks for.
pub const NER_CAPABILITY: &str = "ai:ner:read";

/// Provider abstraction. Network-bound variants share the same
/// request/response shape (chat completions w/ a JSON-mode prompt);
/// the `Stub` variant exists so tests and `disable_network` deploys
/// can exercise the surface without going over the wire.
#[derive(Debug, Clone)]
pub enum NerProvider {
    /// Calls an OpenAI-compatible chat endpoint (`/v1/chat/completions`)
    /// with a prompt that asks for entity extraction in a structured
    /// JSON shape. Hits the network.
    OpenAiCompat { endpoint: String, model: String },
    /// Anthropic native messages API (`/v1/messages`). Hits the network.
    AnthropicNative { endpoint: String, model: String },
    /// In-process stub returning a deterministic response. Used in
    /// tests and when network calls are administratively disabled.
    Stub(StubBehavior),
}

/// Behaviors the [`NerProvider::Stub`] variant can simulate.
#[derive(Debug, Clone)]
pub enum StubBehavior {
    /// Always returns an empty [`TokenSet`].
    Empty,
    /// Returns the input echoed as a single keyword (lowercased,
    /// trimmed). Useful for round-trip tests.
    Echo,
    /// Returns a fixed canned response — useful for snapshot tests
    /// where the caller wants to assert downstream stage output.
    Canned(TokenSet),
    /// Sleeps `Duration` then returns success — used to drive the
    /// timeout path under tests without waiting on real network I/O.
    SlowDuration(Duration),
    /// Returns a hand-crafted JSON string verbatim, pushed through the
    /// same parse + sanitize pipeline as a real LLM response. Lets
    /// adversarial-corpus tests stress the response sanitizer without
    /// a real provider.
    RawJson(String),
}

/// What [`LlmNer::extract`] should do when the LLM call fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicFallback {
    /// On LLM error / timeout / disabled, fall through to the existing
    /// `extract_tokens` heuristic. Recommended default: keeps Stage 1
    /// answering even if the LLM provider is degraded.
    UseHeuristic,
    /// Return an empty [`TokenSet`] on failure (strict mode — useful
    /// when an empty result is preferred over a heuristic guess, e.g.
    /// for compliance audits).
    EmptyOnFail,
    /// Bubble the error up to the caller (caller-handles mode).
    Propagate,
}

/// All the ways [`LlmNer::extract`] can fail. Variants mirror the
/// metric labels emitted by `runtime/ai_ner_failures_total`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NerError {
    /// LLM call exceeded `timeout_ms`.
    NetworkTimeout,
    /// Provider returned a non-2xx status. `body_excerpt` is the first
    /// 256 bytes of the body, with control bytes stripped — safe to log.
    ProviderRejected { status: u16, body_excerpt: String },
    /// Response wasn't valid JSON, or didn't match the expected shape.
    ResponseMalformed { reason: String },
    /// Provider returned more than `max_tokens_returned` tokens.
    ResponseExceedsTokenLimit { count: usize, max: usize },
    /// A returned token matched a secret-redactor pattern. The
    /// `pattern` label says which one (`sk_live`, `bearer`, etc.) so
    /// SOC tooling can alert on hallucinated leaks.
    SecretInResponse { pattern: String },
    /// Caller does not hold `ai:ner:read`.
    AuthDenied,
}

impl std::fmt::Display for NerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NerError::NetworkTimeout => write!(f, "ner: network timeout"),
            NerError::ProviderRejected { status, .. } => {
                write!(f, "ner: provider rejected (status={status})")
            }
            NerError::ResponseMalformed { reason } => {
                write!(f, "ner: malformed response ({reason})")
            }
            NerError::ResponseExceedsTokenLimit { count, max } => {
                write!(f, "ner: response exceeds token limit ({count} > {max})")
            }
            NerError::SecretInResponse { pattern } => {
                write!(f, "ner: secret pattern in response ({pattern})")
            }
            NerError::AuthDenied => write!(f, "ner: auth denied (missing {NER_CAPABILITY})"),
        }
    }
}

impl std::error::Error for NerError {}

/// Trait the LLM-NER uses to gate calls. Production callers will plug
/// in the engine's auth store; tests use [`StubAuthContext`].
///
/// Defined here (and not in `statement_frame`) on purpose — this
/// module is the only consumer for now, and the trait is small enough
/// to inline. If a second consumer emerges, lift it out then.
pub trait AuthContext: std::fmt::Debug + Send + Sync {
    fn has_capability(&self, capability: &str) -> bool;
}

/// Test/embedded stub. Carries an explicit allowlist; matches by
/// exact string. Default-empty means "no capabilities" — the strictest
/// possible test setup.
#[derive(Debug, Clone, Default)]
pub struct StubAuthContext {
    capabilities: Vec<String>,
}

impl StubAuthContext {
    pub fn new(caps: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            capabilities: caps.into_iter().map(Into::into).collect(),
        }
    }

    pub fn allow_all() -> Self {
        Self::new([NER_CAPABILITY])
    }

    pub fn deny_all() -> Self {
        Self::default()
    }
}

impl AuthContext for StubAuthContext {
    fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// Top-level NER handle. Cheap to clone — providers carry only config
/// strings; no live HTTP client is held (a fresh blocking client is
/// built per call so timeout + cancellation are bounded).
#[derive(Debug, Clone)]
pub struct LlmNer {
    pub provider: NerProvider,
    pub fallback: HeuristicFallback,
    pub timeout_ms: u32,
    pub max_tokens_returned: usize,
}

impl LlmNer {
    /// Convenience constructor with the documented defaults.
    pub fn new(provider: NerProvider, fallback: HeuristicFallback) -> Self {
        Self {
            provider,
            fallback,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_tokens_returned: DEFAULT_MAX_TOKENS,
        }
    }

    /// Stage 1 entrypoint. Mirrors the signature of
    /// `ask_pipeline::extract_tokens` but returns `Result` so the
    /// caller can decide whether to honor [`HeuristicFallback`].
    ///
    /// `auth` is passed separately rather than read off `scope` so
    /// embedded callers can plug a different gate without faking an
    /// `EffectiveScope`.
    pub async fn extract(
        &self,
        question: &str,
        scope: &EffectiveScope,
        auth: &dyn AuthContext,
    ) -> Result<TokenSet, NerError> {
        // Auth gate first — never even attempt the call without the
        // capability. This matches ADR 0008's "deny by default, log
        // every denial" posture.
        if !auth.has_capability(NER_CAPABILITY) {
            return Err(NerError::AuthDenied);
        }

        // `scope` participates in the prompt-construction step (visible
        // collections become a hint for the LLM). For the stub paths it
        // doesn't matter; for the network paths we read it via
        // `build_prompt`.
        let result = match &self.provider {
            NerProvider::Stub(behavior) => self.run_stub(behavior, question),
            NerProvider::OpenAiCompat { endpoint, model } => {
                self.run_openai_compat(endpoint, model, question, scope)
                    .await
            }
            NerProvider::AnthropicNative { endpoint, model } => {
                self.run_anthropic(endpoint, model, question, scope).await
            }
        };

        match result {
            Ok(tokens) => Ok(tokens),
            Err(err) => self.handle_failure(err, question),
        }
    }

    /// Apply [`HeuristicFallback`] policy.
    fn handle_failure(&self, err: NerError, question: &str) -> Result<TokenSet, NerError> {
        // Auth denials never fall back — that would defeat the gate.
        if matches!(err, NerError::AuthDenied) {
            return Err(err);
        }
        match self.fallback {
            HeuristicFallback::UseHeuristic => Ok(heuristic_extract_tokens(question)),
            HeuristicFallback::EmptyOnFail => Ok(TokenSet::default()),
            HeuristicFallback::Propagate => Err(err),
        }
    }

    /// Stub dispatcher.
    fn run_stub(&self, behavior: &StubBehavior, question: &str) -> Result<TokenSet, NerError> {
        match behavior {
            StubBehavior::Empty => Ok(TokenSet::default()),
            StubBehavior::Echo => {
                let trimmed = question.trim().to_lowercase();
                if trimmed.is_empty() {
                    Ok(TokenSet::default())
                } else {
                    Ok(TokenSet {
                        keywords: vec![trimmed],
                        literals: vec![],
                    })
                }
            }
            StubBehavior::Canned(tokens) => Ok(tokens.clone()),
            StubBehavior::SlowDuration(d) => {
                // Synthesize the timeout deterministically — never
                // actually sleeps `d`. We only check whether `d`
                // exceeds the configured budget.
                if d.as_millis() as u32 > self.timeout_ms {
                    Err(NerError::NetworkTimeout)
                } else {
                    Ok(TokenSet::default())
                }
            }
            StubBehavior::RawJson(raw) => parse_and_sanitize(raw, self.max_tokens_returned),
        }
    }

    // --- Network paths -----------------------------------------------------
    //
    // The two network providers are intentionally thin: build a prompt,
    // ship it, parse + sanitize the JSON body. Any provider-specific
    // shaping lives in `build_prompt` / `extract_payload`.
    //
    // NB: `reqwest` is in the workspace via `reddb-client` but NOT yet
    // a direct dep of `reddb-server`. Adding it is one of the flagged
    // orchestrator-batch edits; until then these paths fail to compile
    // when the network code is reached. To keep the file
    // self-contained and reviewable today, the network bodies are
    // gated behind `cfg(feature = "ai-ner-network")` — a feature that
    // will be wired by the same orchestrator batch that adds the dep.

    #[cfg(feature = "ai-ner-network")]
    async fn run_openai_compat(
        &self,
        endpoint: &str,
        model: &str,
        question: &str,
        scope: &EffectiveScope,
    ) -> Result<TokenSet, NerError> {
        let body = serde_json::json!({
            "model": model,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": NER_SYSTEM_PROMPT },
                { "role": "user", "content": build_prompt(question, scope) },
            ],
        });
        let raw = http_post_json(endpoint, &body, self.timeout_ms).await?;
        let payload = extract_openai_payload(&raw)?;
        parse_and_sanitize(&payload, self.max_tokens_returned)
    }

    #[cfg(not(feature = "ai-ner-network"))]
    async fn run_openai_compat(
        &self,
        _endpoint: &str,
        _model: &str,
        _question: &str,
        _scope: &EffectiveScope,
    ) -> Result<TokenSet, NerError> {
        // Until the orchestrator wires the `ai-ner-network` feature
        // (and the `reqwest` dep), report a NetworkTimeout so the
        // fallback policy still exercises end-to-end.
        Err(NerError::NetworkTimeout)
    }

    #[cfg(feature = "ai-ner-network")]
    async fn run_anthropic(
        &self,
        endpoint: &str,
        model: &str,
        question: &str,
        scope: &EffectiveScope,
    ) -> Result<TokenSet, NerError> {
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "system": NER_SYSTEM_PROMPT,
            "messages": [
                { "role": "user", "content": build_prompt(question, scope) },
            ],
        });
        let raw = http_post_json(endpoint, &body, self.timeout_ms).await?;
        let payload = extract_anthropic_payload(&raw)?;
        parse_and_sanitize(&payload, self.max_tokens_returned)
    }

    #[cfg(not(feature = "ai-ner-network"))]
    async fn run_anthropic(
        &self,
        _endpoint: &str,
        _model: &str,
        _question: &str,
        _scope: &EffectiveScope,
    ) -> Result<TokenSet, NerError> {
        Err(NerError::NetworkTimeout)
    }
}

/// System prompt the providers share. Kept short on purpose — the
/// fewer instructions, the fewer ways the model can wander off the
/// JSON shape we expect.
const NER_SYSTEM_PROMPT: &str = "\
You are an entity extraction service for a database query pipeline. \
Read the user's question and return a JSON object with two fields: \
'keywords' (array of lowercase content words, length >= 2) and \
'literals' (array of identifier-shaped tokens kept in original case). \
Return JSON only — no prose, no markdown.";

/// Build the user-message prompt. We pin the visible-collection list
/// so the LLM doesn't invent table names that aren't in scope.
#[allow(dead_code)] // used only by network paths; suppress warning when feature off
fn build_prompt(question: &str, scope: &EffectiveScope) -> String {
    use crate::runtime::statement_frame::ReadFrame;
    let visible: Vec<&str> = scope
        .visible_collections()
        .map(|set| set.iter().map(String::as_str).collect())
        .unwrap_or_default();
    format!(
        "Question: {q}\nVisible collections: {v:?}\nReturn JSON only.",
        q = question,
        v = visible
    )
}

#[cfg(feature = "ai-ner-network")]
async fn http_post_json(
    endpoint: &str,
    body: &serde_json::Value,
    timeout_ms: u32,
) -> Result<String, NerError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms as u64))
        .build()
        .map_err(|e| NerError::ResponseMalformed {
            reason: format!("client build: {e}"),
        })?;
    let resp = client.post(endpoint).json(body).send().await.map_err(|e| {
        if e.is_timeout() {
            NerError::NetworkTimeout
        } else {
            NerError::ResponseMalformed {
                reason: format!("transport: {e}"),
            }
        }
    })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| NerError::ResponseMalformed {
        reason: format!("body read: {e}"),
    })?;
    if !(200..300).contains(&status) {
        return Err(NerError::ProviderRejected {
            status,
            body_excerpt: scrub_excerpt(&text),
        });
    }
    Ok(text)
}

#[cfg(feature = "ai-ner-network")]
fn extract_openai_payload(raw: &str) -> Result<String, NerError> {
    let v: JsonValue = serde_json::from_str(raw).map_err(|e| NerError::ResponseMalformed {
        reason: format!("outer json: {e}"),
    })?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| NerError::ResponseMalformed {
            reason: "missing choices[0].message.content".into(),
        })
}

#[cfg(feature = "ai-ner-network")]
fn extract_anthropic_payload(raw: &str) -> Result<String, NerError> {
    let v: JsonValue = serde_json::from_str(raw).map_err(|e| NerError::ResponseMalformed {
        reason: format!("outer json: {e}"),
    })?;
    v["content"][0]["text"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| NerError::ResponseMalformed {
            reason: "missing content[0].text".into(),
        })
}

#[allow(dead_code)] // used by `ProviderRejected` excerpt path
fn scrub_excerpt(s: &str) -> String {
    let trimmed: String = s
        .chars()
        .take(256)
        .filter(|c| !c.is_control() || *c == ' ')
        .collect();
    trimmed
}

/// Core sanitizer. Parses `raw` as JSON, expects `{ keywords: [...],
/// literals: [...] }`, and rejects anything that smells off.
///
/// All policy lives here so both the network and stub paths share the
/// exact same defenses.
fn parse_and_sanitize(raw: &str, max_tokens: usize) -> Result<TokenSet, NerError> {
    let parsed: JsonValue = serde_json::from_str(raw).map_err(|e| NerError::ResponseMalformed {
        reason: format!("json parse: {e}"),
    })?;
    let obj = parsed.as_object().ok_or_else(|| NerError::ResponseMalformed {
        reason: "expected JSON object at root".into(),
    })?;

    let keywords = collect_string_array(obj.get("keywords"), "keywords")?;
    let literals = collect_string_array(obj.get("literals"), "literals")?;

    let total = keywords.len() + literals.len();
    if total > max_tokens {
        return Err(NerError::ResponseExceedsTokenLimit {
            count: total,
            max: max_tokens,
        });
    }

    for token in keywords.iter().chain(literals.iter()) {
        validate_token(token)?;
    }

    Ok(TokenSet { keywords, literals })
}

/// Pull a `Vec<String>` out of a JSON value, with structural errors
/// labeled by `field` so debugging the provider is easier.
fn collect_string_array(v: Option<&JsonValue>, field: &str) -> Result<Vec<String>, NerError> {
    let arr = match v {
        Some(JsonValue::Array(a)) => a,
        Some(JsonValue::Null) | None => return Ok(Vec::new()),
        Some(other) => {
            return Err(NerError::ResponseMalformed {
                reason: format!("{field}: expected array, got {}", json_kind(other)),
            });
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        match item {
            JsonValue::String(s) => out.push(s.clone()),
            other => {
                return Err(NerError::ResponseMalformed {
                    reason: format!("{field}[{i}]: expected string, got {}", json_kind(other)),
                });
            }
        }
    }
    Ok(out)
}

fn json_kind(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

/// Single-token validation. Order matters: secret detection runs
/// before structural checks so a hallucinated `sk_live_...` always
/// reports as `SecretInResponse`, not as `ResponseMalformed`.
fn validate_token(token: &str) -> Result<(), NerError> {
    if let Some(pattern) = match_secret_pattern(token) {
        return Err(NerError::SecretInResponse {
            pattern: pattern.into(),
        });
    }
    if token.is_empty() {
        return Err(NerError::ResponseMalformed {
            reason: "empty token".into(),
        });
    }
    if token.len() > 256 {
        return Err(NerError::ResponseMalformed {
            reason: format!("token too long ({} bytes)", token.len()),
        });
    }
    for (i, byte) in token.as_bytes().iter().enumerate() {
        match byte {
            // NUL, CR, LF — classic injection vectors.
            0x00 => {
                return Err(NerError::ResponseMalformed {
                    reason: format!("NUL byte at offset {i}"),
                });
            }
            b'\n' | b'\r' => {
                return Err(NerError::ResponseMalformed {
                    reason: format!("CR/LF at offset {i}"),
                });
            }
            // Quote injection — keep the parsing surface simple.
            b'"' | b'\'' | b'`' => {
                return Err(NerError::ResponseMalformed {
                    reason: format!("quote injection at offset {i}"),
                });
            }
            // Other control bytes (anything < 0x20 except \t).
            b if *b < 0x20 && *b != b'\t' => {
                return Err(NerError::ResponseMalformed {
                    reason: format!("control byte 0x{b:02x} at offset {i}"),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Secret-redactor patterns. Mirrors the inline policy used by the
/// audit / log boundary guards (see `secret_redactor` in the audit
/// pipeline). Kept here as a const-table so the CI lint can scan a
/// single canonical list.
fn match_secret_pattern(token: &str) -> Option<&'static str> {
    // Constructed at runtime per the secret_fixture_gen pattern — we
    // never materialize the full secret in source, only the prefix.
    const PATTERNS: &[(&str, &str)] = &[
        ("sk_", "sk_prefix"),
        ("rs_", "rs_prefix"),
        ("reddb_", "reddb_prefix"),
        ("Bearer ", "bearer"),
        ("bearer ", "bearer"),
    ];
    for (prefix, label) in PATTERNS {
        if token.starts_with(prefix) {
            return Some(label);
        }
    }
    // JWT shape: three base64url segments separated by dots, each
    // segment at least 4 chars. Cheap structural match — good enough
    // to flag a hallucinated token without false-positiving on real
    // identifiers.
    if looks_like_jwt(token) {
        return Some("jwt");
    }
    // Connection-string credentials: `://user:pass@host`.
    if token.contains("://") && token.contains(':') && token.contains('@') {
        if let Some(scheme_end) = token.find("://") {
            let rest = &token[scheme_end + 3..];
            if let Some(at) = rest.find('@') {
                let userpass = &rest[..at];
                if userpass.contains(':') {
                    return Some("conn_string_credentials");
                }
            }
        }
    }
    None
}

fn looks_like_jwt(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        p.len() >= 4
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ask_pipeline::TokenSet;

    /// Minimal `EffectiveScope` for tests. Built via the public
    /// `Default`-equivalent path used in `ask_pipeline`'s own tests.
    fn make_scope() -> EffectiveScope {
        // EffectiveScope's fields are pub(crate), and this module
        // lives in the same crate, so direct construction is fine.
        // We mirror the `make_scope` pattern from ask_pipeline tests.
        use crate::storage::transaction::snapshot::Snapshot;
        use std::collections::HashSet;
        EffectiveScope {
            tenant: None,
            identity: None,
            snapshot: Snapshot {
                xid: 0,
                in_progress: HashSet::new(),
            },
            visible_collections: None,
        }
    }

    fn allow() -> StubAuthContext {
        StubAuthContext::allow_all()
    }

    fn deny() -> StubAuthContext {
        StubAuthContext::deny_all()
    }

    // --- Stub variants -----------------------------------------------------

    #[tokio::test]
    async fn stub_empty_returns_empty_token_set() {
        let ner = LlmNer::new(NerProvider::Stub(StubBehavior::Empty), HeuristicFallback::Propagate);
        let out = ner.extract("anything", &make_scope(), &allow()).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn stub_echo_returns_lowercased_keyword() {
        let ner = LlmNer::new(NerProvider::Stub(StubBehavior::Echo), HeuristicFallback::Propagate);
        let out = ner
            .extract("  Hello WORLD  ", &make_scope(), &allow())
            .await
            .unwrap();
        assert_eq!(out.keywords, vec!["hello world".to_string()]);
        assert!(out.literals.is_empty());
    }

    #[tokio::test]
    async fn stub_echo_empty_question_yields_empty_set() {
        let ner = LlmNer::new(NerProvider::Stub(StubBehavior::Echo), HeuristicFallback::Propagate);
        let out = ner.extract("   ", &make_scope(), &allow()).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn stub_canned_returns_provided_tokens() {
        let canned = TokenSet {
            keywords: vec!["passport".into()],
            literals: vec!["FDD-1".into()],
        };
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::Canned(canned.clone())),
            HeuristicFallback::Propagate,
        );
        let out = ner.extract("q?", &make_scope(), &allow()).await.unwrap();
        assert_eq!(out, canned);
    }

    // --- Timeout simulation ------------------------------------------------

    #[tokio::test]
    async fn slow_stub_within_budget_succeeds() {
        let mut ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::SlowDuration(Duration::from_millis(10))),
            HeuristicFallback::Propagate,
        );
        ner.timeout_ms = 100;
        assert!(ner.extract("q?", &make_scope(), &allow()).await.is_ok());
    }

    #[tokio::test]
    async fn slow_stub_over_budget_times_out_and_propagates() {
        let mut ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::SlowDuration(Duration::from_millis(500))),
            HeuristicFallback::Propagate,
        );
        ner.timeout_ms = 50;
        let err = ner.extract("q?", &make_scope(), &allow()).await.unwrap_err();
        assert_eq!(err, NerError::NetworkTimeout);
    }

    // --- Malformed-response rejection -------------------------------------

    #[tokio::test]
    async fn malformed_not_json_is_rejected() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson("not-json".into())),
            HeuristicFallback::Propagate,
        );
        let err = ner.extract("q?", &make_scope(), &allow()).await.unwrap_err();
        assert!(matches!(err, NerError::ResponseMalformed { .. }));
    }

    #[tokio::test]
    async fn malformed_wrong_root_type_is_rejected() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson("[1,2,3]".into())),
            HeuristicFallback::Propagate,
        );
        let err = ner.extract("q?", &make_scope(), &allow()).await.unwrap_err();
        assert!(matches!(err, NerError::ResponseMalformed { .. }));
    }

    #[tokio::test]
    async fn malformed_keywords_not_array_is_rejected() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson(r#"{"keywords":"oops"}"#.into())),
            HeuristicFallback::Propagate,
        );
        let err = ner.extract("q?", &make_scope(), &allow()).await.unwrap_err();
        assert!(matches!(err, NerError::ResponseMalformed { .. }));
    }

    // --- Adversarial corpus (control / quote / secret patterns) -----------

    /// Build the adversarial corpus at runtime so no real-shaped
    /// secret ever sits in source. Returns 12 payloads — each one
    /// must trigger either `ResponseMalformed` or `SecretInResponse`.
    fn adversarial_corpus() -> Vec<(&'static str, String)> {
        // Constructed prefixes — keeps the lint scanners happy.
        let sk_prefix = format!("{}{}", "sk_", "live_DEADBEEFcafe");
        let rs_prefix = format!("{}{}", "rs_", "test_TOKENtoken");
        let reddb_prefix = format!("{}{}", "reddb_", "internal_secret_X");
        let bearer = format!("{}{}", "Bearer ", "ABC.DEF.GHI");
        let jwt = format!("{}.{}.{}", "abcd1234", "wxyz5678", "qrst9012");
        let conn = "postgres://user:pwd@host:5432/db".to_string();

        vec![
            ("crlf_in_keyword", "{\"keywords\":[\"foo\\r\\nbar\"]}".into()),
            ("nul_in_literal", "{\"literals\":[\"foo\\u0000bar\"]}".into()),
            ("dquote_injection", "{\"keywords\":[\"foo\\\"bar\"]}".into()),
            ("squote_injection", "{\"keywords\":[\"foo'bar\"]}".into()),
            ("backtick_injection", "{\"keywords\":[\"foo`bar\"]}".into()),
            ("control_byte_low", "{\"keywords\":[\"foo\\u0007bar\"]}".into()),
            ("sk_live", format!(r#"{{"keywords":["{sk_prefix}"]}}"#)),
            ("rs_test", format!(r#"{{"keywords":["{rs_prefix}"]}}"#)),
            ("reddb_internal", format!(r#"{{"literals":["{reddb_prefix}"]}}"#)),
            ("bearer_token", format!(r#"{{"keywords":["{bearer}"]}}"#)),
            ("jwt_shape", format!(r#"{{"literals":["{jwt}"]}}"#)),
            ("conn_string", format!(r#"{{"keywords":["{conn}"]}}"#)),
        ]
    }

    #[tokio::test]
    async fn adversarial_corpus_is_fully_rejected() {
        let corpus = adversarial_corpus();
        assert!(corpus.len() >= 10, "corpus must be ≥10 payloads");
        for (label, raw) in corpus {
            let ner = LlmNer::new(
                NerProvider::Stub(StubBehavior::RawJson(raw)),
                HeuristicFallback::Propagate,
            );
            let err = ner
                .extract("q?", &make_scope(), &allow())
                .await
                .expect_err(&format!("payload {label} should have been rejected"));
            assert!(
                matches!(
                    err,
                    NerError::ResponseMalformed { .. } | NerError::SecretInResponse { .. }
                ),
                "payload {label}: unexpected error variant {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn secret_in_response_reports_pattern_label() {
        let raw = format!(r#"{{"keywords":["{}{}"]}}"#, "sk_", "live_zzzz");
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson(raw)),
            HeuristicFallback::Propagate,
        );
        match ner.extract("q?", &make_scope(), &allow()).await.unwrap_err() {
            NerError::SecretInResponse { pattern } => assert_eq!(pattern, "sk_prefix"),
            other => panic!("expected SecretInResponse, got {other:?}"),
        }
    }

    // --- Token-cap enforcement --------------------------------------------

    #[tokio::test]
    async fn token_cap_excess_is_rejected() {
        // Build a payload with 33 keywords (one over default 32).
        let kws: Vec<String> = (0..33).map(|i| format!("kw{i}")).collect();
        let raw = serde_json::json!({ "keywords": kws }).to_string();
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson(raw)),
            HeuristicFallback::Propagate,
        );
        let err = ner.extract("q?", &make_scope(), &allow()).await.unwrap_err();
        match err {
            NerError::ResponseExceedsTokenLimit { count, max } => {
                assert_eq!(count, 33);
                assert_eq!(max, DEFAULT_MAX_TOKENS);
            }
            other => panic!("expected ResponseExceedsTokenLimit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_cap_at_limit_succeeds() {
        let kws: Vec<String> = (0..DEFAULT_MAX_TOKENS).map(|i| format!("kw{i}")).collect();
        let raw = serde_json::json!({ "keywords": kws }).to_string();
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson(raw)),
            HeuristicFallback::Propagate,
        );
        let out = ner.extract("q?", &make_scope(), &allow()).await.unwrap();
        assert_eq!(out.keywords.len(), DEFAULT_MAX_TOKENS);
    }

    // --- Auth gate ---------------------------------------------------------

    #[tokio::test]
    async fn auth_gate_denies_without_capability() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::Empty),
            HeuristicFallback::UseHeuristic,
        );
        let err = ner.extract("q?", &make_scope(), &deny()).await.unwrap_err();
        assert_eq!(err, NerError::AuthDenied);
    }

    #[tokio::test]
    async fn auth_gate_denial_does_not_fall_back() {
        // Even with UseHeuristic, AuthDenied must propagate — falling
        // back would silently bypass the gate.
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::Empty),
            HeuristicFallback::UseHeuristic,
        );
        let err = ner.extract("FDD-1", &make_scope(), &deny()).await.unwrap_err();
        assert_eq!(err, NerError::AuthDenied);
    }

    // --- Fallback semantics ------------------------------------------------

    #[tokio::test]
    async fn fallback_use_heuristic_runs_extract_tokens() {
        // RawJson with malformed payload → triggers failure → fallback.
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson("not-json".into())),
            HeuristicFallback::UseHeuristic,
        );
        let out = ner
            .extract("show order 987654321 details", &make_scope(), &allow())
            .await
            .unwrap();
        // Heuristic recognizes long digit run as a literal.
        assert!(out.literals.iter().any(|l| l == "987654321"));
    }

    #[tokio::test]
    async fn fallback_empty_on_fail_returns_empty() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson("not-json".into())),
            HeuristicFallback::EmptyOnFail,
        );
        let out = ner
            .extract("show order 987654321 details", &make_scope(), &allow())
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn fallback_propagate_returns_error() {
        let ner = LlmNer::new(
            NerProvider::Stub(StubBehavior::RawJson("not-json".into())),
            HeuristicFallback::Propagate,
        );
        let err = ner
            .extract("show order 987654321 details", &make_scope(), &allow())
            .await
            .unwrap_err();
        assert!(matches!(err, NerError::ResponseMalformed { .. }));
    }

    // --- Helpers -----------------------------------------------------------

    #[test]
    fn jwt_detector_matches_three_segments() {
        assert!(looks_like_jwt("abcd.efgh.ijkl"));
        assert!(!looks_like_jwt("abcd.efgh"));
        assert!(!looks_like_jwt("abc.def.ghi.jkl"));
        assert!(!looks_like_jwt("ab.cd.ef")); // segments too short
    }

    #[test]
    fn scrub_excerpt_drops_control_bytes() {
        let s = format!("ok\x07bad\nstill");
        let cleaned = scrub_excerpt(&s);
        assert!(!cleaned.contains('\x07'));
        assert!(!cleaned.contains('\n'));
    }

    #[test]
    fn validate_token_accepts_normal_strings() {
        assert!(validate_token("passport").is_ok());
        assert!(validate_token("FDD-12313").is_ok());
        assert!(validate_token("foo_bar.baz").is_ok());
    }
}
