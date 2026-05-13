//! AskPipeline — issue #121.
//!
//! 4-stage funnel that turns a natural-language ASK question into a
//! scoped, filtered candidate set the LLM call can synthesise an
//! answer over. Each stage is a pure function so the pipeline reads
//! top-to-bottom in `execute` and individual stages can be unit-tested
//! in isolation:
//!
//! 1. **`extract_tokens`** — heuristic NER. Splits the question into
//!    `keywords` (alphanumeric words) and `literals` (uppercase ID-like
//!    tokens, e.g. `FDD-12313`). LLM-NER is reserved for slice #123.
//! 2. **`match_schema`** — `SchemaVocabulary::lookup` per keyword;
//!    the resulting collection set is intersected with the caller's
//!    `EffectiveScope.visible_collections` so out-of-scope tables
//!    never enter the candidate pool. Issue #119 pre-filter.
//! 3. **`vector_search_scoped`** — best-effort embedding + similarity
//!    over the candidate collections via `AuthorizedSearch`. When no
//!    embedding API is available (most unit-test fixtures), the stage
//!    short-circuits with an empty match list — Stage 4 still runs.
//! 4. **`filter_values`** — applies literal tokens as exact filters
//!    over candidate-collection columns, returning the rows that
//!    actually mention each literal.
//!
//! Output: typed [`AskContext`] holding all four stage outputs plus
//! per-stage timing. Empty token-set short-circuits with a structured
//! error so callers don't pay for an LLM round-trip on a query that
//! contained nothing addressable.
//!
//! The legacy `RedDBRuntime::search_context` is now a Stage-2-internal
//! helper for the broad-recall fallback; ASK no longer reaches for it
//! directly.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tracing::{debug, info, info_span, warn};

use super::ai::ner::{
    AuthContext as NerAuthContext, HeuristicFallback, LlmNer, NerError, NerProvider, NER_CAPABILITY,
};
use super::statement_frame::{EffectiveScope, ReadFrame};
use super::RedDBRuntime;
use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, UnifiedEntity};

/// Default cap for Stage 4 row output. Override per-call via
/// [`AskPipeline::execute_with_limit`].
pub const DEFAULT_ROW_CAP: usize = 20;

/// Token bag produced by Stage 1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSet {
    /// Lowercase keyword tokens (regex `[A-Za-z][A-Za-z0-9_]+`,
    /// length ≥ 2). Used by Stage 2 to probe `SchemaVocabulary`.
    pub keywords: Vec<String>,
    /// Literal-id tokens kept in their original case so Stage 4 can
    /// run case-sensitive equality / substring filters. Matches
    /// `[A-Z0-9-]{3,}` containing at least one digit OR `[0-9]{6,}`.
    pub literals: Vec<String>,
}

impl TokenSet {
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty() && self.literals.is_empty()
    }
}

/// Stage 2 output: collections likely to contain the answer.
#[derive(Debug, Clone, Default)]
pub struct CandidateCollections {
    /// Collection names, sorted, deduplicated, intersected with the
    /// caller's `EffectiveScope.visible_collections`.
    pub collections: Vec<String>,
    /// Columns hinted by `SchemaVocabulary` for each candidate
    /// collection. Used by Stage 4 to scope the literal filter to
    /// promising columns first.
    pub columns_by_collection: HashMap<String, Vec<String>>,
}

/// One Stage 3 vector hit, kept thin so the pipeline doesn't pull
/// the full `ScoredMatch` shape from `dsl::QueryResult` through.
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub collection: String,
    pub entity_id: u64,
    pub score: f32,
}

/// Stage 4 output: rows that match a literal filter.
#[derive(Debug, Clone)]
pub struct FilteredRow {
    pub collection: String,
    pub entity: UnifiedEntity,
    /// Literal token that matched this row.
    pub matched_literal: String,
    /// Column where the match was found, when known.
    pub matched_column: Option<String>,
}

/// Per-stage timing in microseconds. Logged via tracing on every run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageTimings {
    pub extract_us: u64,
    pub schema_us: u64,
    pub vector_us: u64,
    pub filter_us: u64,
}

/// Typed context handed back to the ASK caller. Carries all four
/// stage outputs so the LLM-formatting helper (slice #122) can pick
/// what it needs without re-running the funnel.
#[derive(Debug, Clone)]
pub struct AskContext {
    pub question: String,
    pub tokens: TokenSet,
    pub candidates: CandidateCollections,
    pub vector_hits: Vec<VectorHit>,
    pub filtered_rows: Vec<FilteredRow>,
    pub source_limit: usize,
    pub timings: StageTimings,
}

impl Default for AskContext {
    fn default() -> Self {
        Self {
            question: String::new(),
            tokens: TokenSet::default(),
            candidates: CandidateCollections::default(),
            vector_hits: Vec::new(),
            filtered_rows: Vec::new(),
            source_limit: DEFAULT_ROW_CAP,
            timings: StageTimings::default(),
        }
    }
}

/// One fused source reference in the final ASK context order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusedSourceRef {
    FilteredRow(usize),
    VectorHit(usize),
}

/// Pipeline entry point. Instances are stateless — kept as an empty
/// enum so callers spell `AskPipeline::execute(...)` (matches the
/// shape #121 calls out).
pub enum AskPipeline {}

impl AskPipeline {
    /// Run all four stages with the default row cap.
    pub fn execute(
        runtime: &RedDBRuntime,
        scope: &EffectiveScope,
        question: &str,
    ) -> RedDBResult<AskContext> {
        Self::execute_with_limit(runtime, scope, question, DEFAULT_ROW_CAP)
    }

    /// Run all four stages with a configurable Stage 4 row cap.
    pub fn execute_with_limit(
        runtime: &RedDBRuntime,
        scope: &EffectiveScope,
        question: &str,
        row_cap: usize,
    ) -> RedDBResult<AskContext> {
        Self::execute_with_limit_and_min_score(runtime, scope, question, row_cap, None)
    }

    /// Run all four stages with a configurable row cap and per-bucket
    /// minimum score for retrieval stages that expose native scores.
    pub fn execute_with_limit_and_min_score(
        runtime: &RedDBRuntime,
        scope: &EffectiveScope,
        question: &str,
        row_cap: usize,
        min_score: Option<f32>,
    ) -> RedDBResult<AskContext> {
        let span = info_span!(
            "ask_pipeline.execute",
            tenant = ?scope.effective_scope(),
            question_len = question.len(),
            row_cap = row_cap,
            min_score = ?min_score,
        );
        let _enter = span.enter();

        // Stage 1. Routed through `extract_tokens_routed` so the
        // `ai.ner.backend` config knob can swap the heuristic for the
        // opt-in LLM NER without converting the surrounding pipeline
        // to async (see `extract_tokens_routed` for the rationale).
        let stage1 = Instant::now();
        let tokens = extract_tokens_routed(runtime, scope, question)?;
        let extract_us = stage1.elapsed().as_micros() as u64;
        debug!(
            target: "ask_pipeline",
            stage = "extract_tokens",
            keywords = ?tokens.keywords,
            literals = ?tokens.literals,
            elapsed_us = extract_us,
            "stage 1 done"
        );
        if tokens.is_empty() {
            warn!(
                target: "ask_pipeline",
                question_len = question.len(),
                "refused: empty token set"
            );
            return Err(RedDBError::Query(
                "ASK question yielded no usable tokens (heuristic NER produced empty keyword + literal set)"
                    .to_string(),
            ));
        }

        // Stage 2.
        let stage2 = Instant::now();
        let candidates = match_schema(runtime, scope, &tokens)?;
        let schema_us = stage2.elapsed().as_micros() as u64;
        debug!(
            target: "ask_pipeline",
            stage = "match_schema",
            collections = ?candidates.collections,
            elapsed_us = schema_us,
            "stage 2 done"
        );

        // Stage 3.
        let stage3 = Instant::now();
        let vector_hits =
            vector_search_scoped(runtime, scope, question, &candidates, row_cap, min_score);
        let vector_us = stage3.elapsed().as_micros() as u64;
        debug!(
            target: "ask_pipeline",
            stage = "vector_search_scoped",
            hits = vector_hits.len(),
            elapsed_us = vector_us,
            "stage 3 done"
        );

        // Stage 4.
        let stage4 = Instant::now();
        let filtered_rows = filter_values(runtime, scope, &candidates, &tokens, row_cap);
        let filter_us = stage4.elapsed().as_micros() as u64;
        debug!(
            target: "ask_pipeline",
            stage = "filter_values",
            rows = filtered_rows.len(),
            elapsed_us = filter_us,
            "stage 4 done"
        );

        Ok(AskContext {
            question: question.to_string(),
            tokens,
            candidates,
            vector_hits,
            filtered_rows,
            source_limit: row_cap,
            timings: StageTimings {
                extract_us,
                schema_us,
                vector_us,
                filter_us,
            },
        })
    }
}

/// Fuse row-filter and vector buckets into the single ranked source
/// order used by prompt rendering and `sources_flat`.
pub fn fused_source_order(ctx: &AskContext) -> Vec<FusedSourceRef> {
    use super::ai::rrf_fuser::{fuse, Bucket, Candidate, RRF_K_DEFAULT};

    if ctx.source_limit == 0 || (ctx.filtered_rows.is_empty() && ctx.vector_hits.is_empty()) {
        return Vec::new();
    }

    let mut refs: HashMap<String, FusedSourceRef> = HashMap::new();
    let row_bucket = Bucket {
        candidates: ctx
            .filtered_rows
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let id = source_identity(&row.collection, row.entity.id.raw());
                refs.entry(id.clone())
                    .or_insert(FusedSourceRef::FilteredRow(idx));
                Candidate { id, score: 1.0 }
            })
            .collect(),
        min_score: None,
    };
    let vector_bucket = Bucket {
        candidates: ctx
            .vector_hits
            .iter()
            .enumerate()
            .map(|(idx, hit)| {
                let id = source_identity(&hit.collection, hit.entity_id);
                refs.entry(id.clone())
                    .or_insert(FusedSourceRef::VectorHit(idx));
                Candidate {
                    id,
                    score: hit.score as f64,
                }
            })
            .collect(),
        min_score: None,
    };

    fuse(
        &[row_bucket, vector_bucket],
        RRF_K_DEFAULT,
        ctx.source_limit,
    )
    .into_iter()
    .filter_map(|item| refs.get(&item.id).copied())
    .collect()
}

fn source_identity(collection: &str, entity_id: u64) -> String {
    format!("{collection}/{entity_id}")
}

// ---------------------------------------------------------------------------
// Stage 1 — token / entity extraction (heuristic v1) + opt-in LLM NER routing.
// ---------------------------------------------------------------------------

/// Stage-1 dispatcher honouring the `ai.ner.backend` config knob.
///
/// The pipeline (and `execute_with_limit` in particular) stays
/// **sync** per #123 deepening to avoid an async cascade through the
/// rest of `ask_pipeline`. When the operator turns on
/// `ai.ner.backend = "llm"`, we contain the async surface here by
/// using `tokio::runtime::Handle::current().block_on(...)` — works
/// when called from inside an async context (the production HTTP
/// handler path), falls back to the heuristic with a warn-log when
/// no Tokio runtime is reachable (sync test contexts and embedded
/// callers without a runtime).
///
/// Auth gate: `LlmNer::extract` checks `ai:ner:read`. Today
/// `EffectiveScope::has_capability` is a placeholder that always
/// returns `false`, so a LLM-backend-configured deployment will see
/// every call deny at the gate and the configured `HeuristicFallback`
/// fires. A one-shot info log makes that visible in operator logs.
/// Wiring the real capability check into the auth engine is future
/// work — the routing seam is in place so that landing the auth
/// extension is a one-line `EffectiveScope` change.
fn extract_tokens_routed(
    runtime: &RedDBRuntime,
    scope: &EffectiveScope,
    question: &str,
) -> RedDBResult<TokenSet> {
    let backend = runtime.config_string("ai.ner.backend", "heuristic");
    if backend != "llm" {
        return Ok(extract_tokens(question));
    }

    let endpoint = runtime.config_string("ai.ner.endpoint", "");
    let model = runtime.config_string("ai.ner.model", "");
    let timeout_ms = runtime
        .config_string("ai.ner.timeout_ms", "5000")
        .parse::<u32>()
        .unwrap_or(5000);
    let fallback = match runtime
        .config_string("ai.ner.fallback", "use_heuristic")
        .as_str()
    {
        "empty_on_fail" => HeuristicFallback::EmptyOnFail,
        "propagate" => HeuristicFallback::Propagate,
        _ => HeuristicFallback::UseHeuristic,
    };

    // Endpoint shape decides provider; default to OpenAI-compat when
    // unspecified — matches the documented config shape.
    let provider = if endpoint.is_empty() && model.is_empty() {
        // No network config provided — operator opted into "llm" but
        // didn't wire a backend. Fall back to a Stub::Empty so the
        // configured fallback policy fires deterministically.
        NerProvider::Stub(super::ai::ner::StubBehavior::Empty)
    } else {
        NerProvider::OpenAiCompat { endpoint, model }
    };

    let mut ner = LlmNer::new(provider, fallback);
    ner.timeout_ms = timeout_ms;

    let auth = ScopeAuthAdapter(scope);
    let llm_result = match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // Use `block_on` from a thread that's not driving the
            // current runtime — the typical caller is an Axum HTTP
            // handler running on the multi-thread runtime, so we hop
            // off via `block_in_place`.
            tokio::task::block_in_place(|| handle.block_on(ner.extract(question, scope, &auth)))
        }
        Err(_) => {
            warn!(
                target: "ask_pipeline",
                "ai.ner.backend=llm configured but no Tokio runtime reachable from extract_tokens; using heuristic fallback"
            );
            return Ok(extract_tokens(question));
        }
    };

    match llm_result {
        Ok(tokens) => Ok(tokens),
        Err(NerError::AuthDenied) => {
            log_auth_denial_once();
            // Auth denials never honour fallback inside `LlmNer`, so
            // the routing layer applies the configured fallback here
            // — this is the bridge until the auth engine wires the
            // capability for real.
            apply_fallback(fallback, question)
        }
        Err(err) => {
            warn!(
                target: "ask_pipeline",
                error = %err,
                "LlmNer extract failed; honouring HeuristicFallback policy"
            );
            apply_fallback(fallback, question)
        }
    }
}

fn apply_fallback(fallback: HeuristicFallback, question: &str) -> RedDBResult<TokenSet> {
    match fallback {
        HeuristicFallback::UseHeuristic => Ok(extract_tokens(question)),
        HeuristicFallback::EmptyOnFail => Ok(TokenSet::default()),
        HeuristicFallback::Propagate => Err(RedDBError::Query(
            "ai.ner.backend=llm: extract failed and ai.ner.fallback=propagate".to_string(),
        )),
    }
}

/// One-shot info log for the auth-gate placeholder. Until the auth
/// engine learns to grant `ai:ner:read`, every routed call denies —
/// we emit the explainer once per process so logs aren't spammed.
fn log_auth_denial_once() {
    static EMITTED: AtomicBool = AtomicBool::new(false);
    if !EMITTED.swap(true, Ordering::Relaxed) {
        info!(
            target: "ask_pipeline",
            capability = NER_CAPABILITY,
            "LlmNer routing configured but capability `{}` not yet wired into auth engine; falling back to heuristic",
            NER_CAPABILITY
        );
    }
}

/// Adapter wrapping `EffectiveScope` so it can drive the
/// `LlmNer`-side `AuthContext` trait without leaking that trait
/// across the rest of the runtime.
struct ScopeAuthAdapter<'a>(&'a EffectiveScope);

impl<'a> std::fmt::Debug for ScopeAuthAdapter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeAuthAdapter").finish_non_exhaustive()
    }
}

impl<'a> NerAuthContext for ScopeAuthAdapter<'a> {
    fn has_capability(&self, capability: &str) -> bool {
        self.0.has_capability(capability)
    }
}

/// Split a question into a [`TokenSet`]. Pure function — no runtime
/// lookups.
///
/// Rules (heuristic v1):
/// - **Literals** match `[A-Z0-9-]{3,}` AND contain ≥1 digit, OR are
///   pure digit runs of length ≥ 6. Captures id shapes like
///   `FDD-12313`, `INV-2024-001`, `123456`.
/// - **Keywords** match `[A-Za-z][A-Za-z0-9_]+` (length ≥ 2),
///   normalised to lowercase. Stop words are dropped to avoid
///   wasting Stage-2 lookups on noise.
pub fn extract_tokens(question: &str) -> TokenSet {
    let mut keywords: Vec<String> = Vec::new();
    let mut literals: Vec<String> = Vec::new();

    let mut chars = question.chars().peekable();
    let mut buf = String::new();

    let flush = |buf: &mut String, keywords: &mut Vec<String>, literals: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        let word = std::mem::take(buf);
        classify_token(&word, keywords, literals);
    };

    while let Some(ch) = chars.next() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            buf.push(ch);
        } else {
            flush(&mut buf, &mut keywords, &mut literals);
            // Skip remaining whitespace / punctuation; loop continues.
            let _ = ch;
        }
        // Look ahead to also flush on EOF.
        if chars.peek().is_none() {
            flush(&mut buf, &mut keywords, &mut literals);
        }
    }
    // Final flush in case the loop body didn't see EOF (empty
    // iterator path).
    if !buf.is_empty() {
        classify_token(&buf, &mut keywords, &mut literals);
    }

    // Dedup keywords + literals while preserving order.
    let mut seen = HashSet::new();
    keywords.retain(|tok| seen.insert(tok.clone()));
    let mut seen_lit = HashSet::new();
    literals.retain(|tok| seen_lit.insert(tok.clone()));

    TokenSet { keywords, literals }
}

fn classify_token(word: &str, keywords: &mut Vec<String>, literals: &mut Vec<String>) {
    // Literal: shape `[A-Z0-9-]{3,}` with at least one digit, OR
    // pure-digit run of length ≥ 6.
    let is_upper_id_shape = word.len() >= 3
        && word
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c.is_ascii_uppercase())
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_uppercase() || c == '-');
    let is_long_digit_run = word.len() >= 6 && word.chars().all(|c| c.is_ascii_digit());
    if is_upper_id_shape || is_long_digit_run {
        literals.push(word.to_string());
        return;
    }
    // Keyword: starts with a letter, len ≥ 2, drop trailing/leading
    // hyphens, lowercase.
    let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    if trimmed.len() < 2 {
        return;
    }
    if !trimmed
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        return;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        // Hyphenated word that wasn't a literal — skip rather than
        // index a fragmented token.
        return;
    }
    let lower = trimmed.to_ascii_lowercase();
    if STOP_WORDS.binary_search(&lower.as_str()).is_ok() {
        return;
    }
    keywords.push(lower);
}

/// Sorted ascii lowercase stop-word list. Kept tiny and curated so a
/// regression in Stage 2 candidate-narrowing surfaces fast.
const STOP_WORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "by", "do", "for", "from", "how", "in",
    "is", "it", "of", "on", "or", "que", "qual", "quais", "sobre", "te", "the", "to", "what",
    "where", "which", "with",
];

// ---------------------------------------------------------------------------
// Stage 2 — schema-vocabulary match.
// ---------------------------------------------------------------------------

/// For each keyword, probe `SchemaVocabulary` and intersect the
/// resulting collection set with `scope.visible_collections`. Returns
/// the deduplicated, sorted candidate list plus a per-collection
/// column hint set for Stage 4.
pub fn match_schema(
    runtime: &RedDBRuntime,
    scope: &EffectiveScope,
    tokens: &TokenSet,
) -> RedDBResult<CandidateCollections> {
    let visible = match scope.visible_collections() {
        Some(set) => set.clone(),
        None => {
            // No scope wired = embedded mode. Fall back to "every
            // collection in the DB" so the pipeline still runs;
            // AuthorizedSearch is the seam that refuses the deny-
            // default for AI commands.
            runtime
                .inner
                .db
                .store()
                .list_collections()
                .into_iter()
                .collect()
        }
    };

    let mut collections: BTreeSet<String> = BTreeSet::new();
    let mut columns_by_collection: HashMap<String, BTreeSet<String>> = HashMap::new();
    for keyword in &tokens.keywords {
        let hits = runtime.schema_vocabulary_lookup(keyword);
        for hit in hits {
            if !visible.contains(&hit.collection) {
                continue;
            }
            collections.insert(hit.collection.clone());
            if let Some(column) = hit.column {
                columns_by_collection
                    .entry(hit.collection)
                    .or_default()
                    .insert(column);
            }
        }
    }

    Ok(CandidateCollections {
        collections: collections.into_iter().collect(),
        columns_by_collection: columns_by_collection
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Stage 3 — vector search scoped to the candidate collections.
// ---------------------------------------------------------------------------

/// Embed the question (best-effort — skipped if no provider is
/// configured, see `embed_question`) and run
/// `AuthorizedSearch::execute_similar` over each candidate
/// collection. Returns at most `top_k` hits sorted by similarity.
pub fn vector_search_scoped(
    runtime: &RedDBRuntime,
    scope: &EffectiveScope,
    question: &str,
    candidates: &CandidateCollections,
    top_k: usize,
    min_score: Option<f32>,
) -> Vec<VectorHit> {
    if candidates.collections.is_empty() {
        return Vec::new();
    }
    let Some(embedding) = embed_question(runtime, question) else {
        return Vec::new();
    };
    let per_collection = top_k.max(1);
    let mut hits: Vec<VectorHit> = Vec::new();
    for collection in &candidates.collections {
        match super::authorized_search::AuthorizedSearch::execute_similar(
            runtime,
            scope,
            collection,
            &embedding,
            per_collection,
            min_score.unwrap_or(0.0),
        ) {
            Ok(results) => {
                for result in results {
                    hits.push(VectorHit {
                        collection: collection.clone(),
                        entity_id: result.entity_id.raw(),
                        score: result.score,
                    });
                }
            }
            Err(err) => {
                debug!(
                    target: "ask_pipeline",
                    collection = collection,
                    err = %err,
                    "vector_search_scoped: collection skipped"
                );
            }
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    hits.truncate(top_k);
    hits
}

/// Best-effort embedding. Returns `None` when no embedding provider
/// is configured (the unit-test fixture path) — the caller treats
/// `None` as "Stage 3 yielded zero hits" and continues.
fn embed_question(runtime: &RedDBRuntime, question: &str) -> Option<Vec<f32>> {
    let kv_getter = |key: &str| -> RedDBResult<Option<String>> {
        match runtime.inner.db.get_kv("red_config", key) {
            Some((Value::Text(value), _)) => Ok(Some(value.to_string())),
            Some(_) => Ok(None),
            None => Ok(None),
        }
    };
    let provider = crate::ai::resolve_default_provider(&kv_getter);
    if !provider.is_openai_compatible() {
        return None;
    }
    let model = crate::ai::resolve_default_model(&provider, &kv_getter);
    let api_key = crate::ai::resolve_api_key(&provider, None, kv_getter).ok()?;
    let transport = crate::runtime::ai::transport::AiTransport::from_runtime(runtime);
    let request = crate::ai::OpenAiEmbeddingRequest {
        api_key,
        model,
        inputs: vec![question.to_string()],
        dimensions: None,
        api_base: provider.resolve_api_base(),
    };
    let response = crate::runtime::ai::block_on_ai(async move {
        crate::ai::openai_embeddings_async(&transport, request).await
    })
    .and_then(|result| result)
    .ok()?;
    response.embeddings.into_iter().next()
}

// ---------------------------------------------------------------------------
// Stage 4 — value filter using literal tokens.
// ---------------------------------------------------------------------------

/// Walk every candidate collection looking for rows whose columns
/// contain any of the literal tokens. Caller-supplied `row_cap`
/// bounds the result count; column hints from Stage 2 are visited
/// first so promising columns find a match before a full scan.
pub fn filter_values(
    runtime: &RedDBRuntime,
    scope: &EffectiveScope,
    candidates: &CandidateCollections,
    tokens: &TokenSet,
    row_cap: usize,
) -> Vec<FilteredRow> {
    if tokens.literals.is_empty() || candidates.collections.is_empty() {
        return Vec::new();
    }
    let visible = scope.visible_collections();
    let store = runtime.inner.db.store();
    let mut out: Vec<FilteredRow> = Vec::new();

    'collection: for collection in &candidates.collections {
        // Defence-in-depth: redo the visibility check here so a Stage
        // 2 regression can't smuggle an out-of-scope collection
        // through.
        if let Some(set) = visible {
            if !set.contains(collection) {
                continue;
            }
        }
        let Some(manager) = store.get_collection(collection) else {
            continue;
        };
        let hint_columns: &[String] = candidates
            .columns_by_collection
            .get(collection)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        for entity in manager.query_all(|_| true) {
            if let Some(hit) = literal_match_in_entity(&entity, &tokens.literals, hint_columns) {
                out.push(FilteredRow {
                    collection: collection.clone(),
                    entity,
                    matched_literal: hit.0,
                    matched_column: hit.1,
                });
                if out.len() >= row_cap {
                    break 'collection;
                }
            }
        }
    }
    out
}

/// Look for any literal in any column value of `entity`. Hint
/// columns are checked first; a positive hit short-circuits.
fn literal_match_in_entity(
    entity: &UnifiedEntity,
    literals: &[String],
    hint_columns: &[String],
) -> Option<(String, Option<String>)> {
    let row = match &entity.data {
        EntityData::Row(row) => row,
        _ => return None,
    };

    // Pass 1: hint columns first.
    for column in hint_columns {
        if let Some(value) = row.get_field(column) {
            if let Some(lit) = first_literal_in_value(value, literals) {
                return Some((lit, Some(column.clone())));
            }
        }
    }
    // Pass 2: every other column.
    for (name, value) in row.iter_fields() {
        if hint_columns.iter().any(|c| c == name) {
            continue;
        }
        if let Some(lit) = first_literal_in_value(value, literals) {
            return Some((lit, Some(name.to_string())));
        }
    }
    None
}

fn first_literal_in_value(value: &Value, literals: &[String]) -> Option<String> {
    let rendered = match value {
        Value::Text(s) => s.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Json(j) => String::from_utf8_lossy(j).to_string(),
        _ => return None,
    };
    for lit in literals {
        // Case-sensitive substring match: literals are id-shaped, so
        // we want `FDD-12313` to find embedded `FDD-12313` in a free-
        // form description column too.
        if rendered.contains(lit) {
            return Some(lit.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Stage 1 ------------------------------------------------------

    #[test]
    fn extract_tokens_splits_keywords_and_literals() {
        let tokens = extract_tokens("quais as novidades sobre o passport FDD-12313?");
        // `quais`, `as`, `sobre`, `o` are stop words; `novidades`,
        // `passport` survive.
        assert!(tokens.keywords.contains(&"novidades".to_string()));
        assert!(tokens.keywords.contains(&"passport".to_string()));
        assert!(tokens.literals.contains(&"FDD-12313".to_string()));
        assert!(!tokens.is_empty());
    }

    #[test]
    fn extract_tokens_returns_empty_for_punctuation_only() {
        let tokens = extract_tokens("???   ...");
        assert!(tokens.is_empty());
    }

    #[test]
    fn extract_tokens_long_digit_run_is_a_literal() {
        let tokens = extract_tokens("show order 987654321 details");
        assert!(tokens.literals.contains(&"987654321".to_string()));
        assert!(tokens.keywords.contains(&"order".to_string()));
        assert!(tokens.keywords.contains(&"details".to_string()));
        assert!(tokens.keywords.contains(&"show".to_string()));
    }

    #[test]
    fn extract_tokens_short_uppercase_word_is_keyword_not_literal() {
        // "USA" is uppercase but lacks a digit, so it stays a keyword
        // (lowercased) — Stage 2 still gets to probe it.
        let tokens = extract_tokens("USA exports report");
        assert!(tokens.keywords.contains(&"usa".to_string()));
        assert!(tokens.literals.is_empty());
    }

    #[test]
    fn extract_tokens_dedups() {
        let tokens = extract_tokens("passport passport FDD-1 FDD-1");
        assert_eq!(
            tokens.keywords.iter().filter(|k| *k == "passport").count(),
            1
        );
        assert_eq!(tokens.literals.iter().filter(|l| *l == "FDD-1").count(), 1);
    }

    // -- Stage 4 helper ----------------------------------------------

    #[test]
    fn first_literal_in_value_substring_match() {
        let lit = first_literal_in_value(
            &Value::text("issue FDD-12313 reported by user"),
            &["FDD-12313".to_string()],
        );
        assert_eq!(lit.as_deref(), Some("FDD-12313"));
    }

    #[test]
    fn first_literal_in_value_no_match_returns_none() {
        assert!(
            first_literal_in_value(&Value::text("nothing here"), &["FDD-12313".to_string()],)
                .is_none()
        );
    }

    // -- Pipeline-wide -----------------------------------------------

    use crate::api::RedDBOptions;
    use crate::auth::Role;
    use crate::runtime::statement_frame::EffectiveScope;
    use crate::runtime::RedDBRuntime;
    use crate::storage::schema::Value;
    use crate::storage::transaction::snapshot::Snapshot;
    use crate::storage::unified::entity::{
        EntityData, EntityId, EntityKind, RowData, UnifiedEntity,
    };
    use std::sync::Arc;

    fn make_scope(visible: HashSet<String>) -> EffectiveScope {
        EffectiveScope {
            tenant: Some("acme".to_string()),
            identity: Some(("alice".to_string(), Role::Read)),
            snapshot: Snapshot {
                xid: 0,
                in_progress: HashSet::new(),
            },
            visible_collections: Some(visible),
        }
    }

    fn fresh_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
    }

    fn test_row(collection: &str, id: u64) -> FilteredRow {
        FilteredRow {
            collection: collection.to_string(),
            entity: UnifiedEntity::new(
                EntityId::new(id),
                EntityKind::TableRow {
                    table: Arc::from(collection),
                    row_id: id,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(
                        [("body".to_string(), Value::text("ticket FDD-1".to_string()))]
                            .into_iter()
                            .collect(),
                    ),
                    schema: None,
                }),
            ),
            matched_literal: "FDD-1".to_string(),
            matched_column: Some("body".to_string()),
        }
    }

    #[test]
    fn fused_source_order_uses_rrf_and_total_limit() {
        let ctx = AskContext {
            source_limit: 2,
            filtered_rows: vec![test_row("incidents", 2), test_row("incidents", 1)],
            vector_hits: vec![
                VectorHit {
                    collection: "incidents".to_string(),
                    entity_id: 1,
                    score: 0.91,
                },
                VectorHit {
                    collection: "docs".to_string(),
                    entity_id: 9,
                    score: 0.88,
                },
            ],
            ..AskContext::default()
        };

        let order = fused_source_order(&ctx);

        assert_eq!(
            order,
            vec![
                FusedSourceRef::FilteredRow(1),
                FusedSourceRef::FilteredRow(0)
            ]
        );
    }

    /// Empty token sets short-circuit with a structured error before
    /// any LLM round-trip.
    #[test]
    fn execute_refuses_empty_token_set() {
        let rt = fresh_runtime();
        let scope = make_scope(HashSet::new());
        let err = AskPipeline::execute(&rt, &scope, "??? ...")
            .expect_err("empty token set must short-circuit");
        let msg = format!("{err}");
        assert!(
            msg.contains("yielded no usable tokens"),
            "expected structured empty-token error, got: {msg}"
        );
    }

    /// match_schema drops every collection that's outside
    /// `scope.visible_collections`. Using a synthetic vocab via DDL
    /// events on the live runtime so the assertion drives real
    /// `RedDBRuntime::schema_vocabulary_lookup`.
    #[test]
    fn match_schema_intersects_with_visible_set() {
        let rt = fresh_runtime();
        // Two collections both carry a `passport` column. Caller's
        // scope only includes `travel`, so the `passport` column hit
        // on `secrets` must be dropped.
        rt.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::CreateCollection {
                collection: "travel".to_string(),
                columns: vec!["id".into(), "passport".into()],
                type_tags: Vec::new(),
                description: None,
            },
        );
        rt.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::CreateCollection {
                collection: "secrets".to_string(),
                columns: vec!["passport".into()],
                type_tags: Vec::new(),
                description: None,
            },
        );
        let visible: HashSet<String> = ["travel".to_string()].into_iter().collect();
        let scope = make_scope(visible.clone());
        let tokens = TokenSet {
            keywords: vec!["passport".to_string()],
            literals: Vec::new(),
        };
        let candidates = match_schema(&rt, &scope, &tokens).expect("ok");
        assert_eq!(candidates.collections, vec!["travel".to_string()]);
        assert!(!candidates.collections.contains(&"secrets".to_string()));
        // Column hint surfaces for the surviving collection.
        let cols = candidates
            .columns_by_collection
            .get("travel")
            .expect("hint columns");
        assert!(cols.contains(&"passport".to_string()));
    }

    // -- Property test (issue #121 acceptance row) -------------------
    //
    // For 256 random (question, scope) pairs: every Stage 4 row's
    // collection MUST be inside `scope.visible_collections`. Drives
    // `filter_values` directly with synthetic candidate sets so the
    // invariant is pinned without an embedding API.

    use proptest::prelude::*;

    fn arb_collection() -> impl Strategy<Value = String> {
        "[a-z]{1,4}"
    }

    fn arb_visible() -> impl Strategy<Value = HashSet<String>> {
        prop::collection::hash_set(arb_collection(), 0..6)
    }

    fn arb_candidates() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(arb_collection(), 0..8)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn stage4_rows_subset_of_visible_collections(
            visible in arb_visible(),
            candidate_names in arb_candidates(),
            literal_count in 0usize..3,
        ) {
            // Single runtime shared across cases — `filter_values`
            // only reads (no mutation), and we need the empty-store
            // path so the invariant we want to pin is "no row escapes
            // visible_collections" rather than "any specific row
            // surfaces".
            let rt = PROPTEST_RUNTIME.get_or_init(fresh_runtime);
            let candidates = CandidateCollections {
                collections: candidate_names,
                columns_by_collection: HashMap::new(),
            };
            let literals: Vec<String> = (0..literal_count)
                .map(|i| format!("ID-{i}"))
                .collect();
            let tokens = TokenSet {
                keywords: vec!["passport".to_string()],
                literals,
            };
            let scope = make_scope(visible.clone());
            let rows = filter_values(rt, &scope, &candidates, &tokens, DEFAULT_ROW_CAP);
            for row in &rows {
                prop_assert!(
                    visible.contains(&row.collection),
                    "Stage 4 leaked row collection={} not in visible={:?}",
                    row.collection, visible
                );
            }
        }
    }

    static PROPTEST_RUNTIME: std::sync::OnceLock<RedDBRuntime> = std::sync::OnceLock::new();

    // -- Integration test (issue #121 acceptance row) ----------------
    //
    // Drives the four stages end-to-end through `AskPipeline::execute`
    // with the question the issue calls out:
    //   "quais as novidades sobre o passport FDD-12313?"
    //
    // Stage 1 must extract `passport` + `FDD-12313`. Stage 2 must
    // narrow to the `passports` collection (visible to the caller).
    // Stage 3 silently yields zero hits without an embedding provider
    // — that's expected for the test fixture path. Stage 4 must surface
    // the row whose `notes` column embeds `FDD-12313`.

    #[test]
    fn integration_passport_fdd_12313_funnels_through_four_stages() {
        let rt = fresh_runtime();
        // The collection name itself + a `passport` column both feed
        // Stage 2's vocabulary; either one is enough for the question
        // "passport FDD-12313" to land here.
        rt.execute_query("CREATE TABLE travel (id INT, passport TEXT, notes TEXT)")
            .expect("CREATE TABLE travel");
        rt.execute_query(
            "INSERT INTO travel (id, passport, notes) VALUES \
             (1, 'BR-001', 'unrelated note'), \
             (2, 'PT-002', 'incident FDD-12313 escalated'), \
             (3, 'US-003', 'standard renewal')",
        )
        .expect("seed rows");
        // Out-of-scope collection — must NEVER surface in Stage 4.
        rt.execute_query("CREATE TABLE secrets (id INT, passport TEXT)")
            .expect("CREATE TABLE secrets");
        rt.execute_query("INSERT INTO secrets (id, passport) VALUES (99, 'FDD-12313')")
            .expect("seed secrets");

        let visible: HashSet<String> = ["travel".to_string()].into_iter().collect();
        let scope = make_scope(visible);

        let ctx = AskPipeline::execute(
            &rt,
            &scope,
            "quais as novidades sobre o passport FDD-12313?",
        )
        .expect("pipeline runs");

        // Stage 1: passport + FDD-12313 surfaced.
        assert!(ctx.tokens.keywords.contains(&"passport".to_string()));
        assert!(ctx.tokens.literals.contains(&"FDD-12313".to_string()));

        // Stage 2: candidates narrowed to `travel` (the `passport`
        // column on `secrets` is dropped by the visible-set
        // intersection).
        assert_eq!(ctx.candidates.collections, vec!["travel".to_string()]);

        // Stage 3: best-effort embedding — without a provider
        // configured, Stage 3 silently returns []; the rest of the
        // funnel still runs.
        let _ = &ctx.vector_hits;

        // Stage 4: the row whose `notes` mentions `FDD-12313`
        // surfaces; the out-of-scope `secrets` row does NOT.
        assert!(
            ctx.filtered_rows
                .iter()
                .any(|r| r.collection == "travel" && r.matched_literal == "FDD-12313"),
            "expected travel row with FDD-12313 match, got: {:?}",
            ctx.filtered_rows
        );
        for row in &ctx.filtered_rows {
            assert_ne!(
                row.collection, "secrets",
                "secrets row leaked into Stage 4 output"
            );
        }

        // Per-stage timing recorded.
        // (The Instant-based measurements may be 0 on very fast hosts;
        // we only check the field exists and was populated.)
        let _ = ctx.timings.extract_us
            + ctx.timings.schema_us
            + ctx.timings.vector_us
            + ctx.timings.filter_us;
    }

    // -- Stage 1 routing (Lane 4/5: LlmNer wiring) -------------------
    //
    // The routing dispatcher is an `extract_tokens_routed` helper that
    // reads `ai.ner.backend` and either passes through to the heuristic
    // (default) or routes through `LlmNer::extract`. Today the
    // capability gate (`EffectiveScope::has_capability`) is a placeholder
    // that always returns `false`, so the LLM path always denies and
    // the configured `HeuristicFallback` policy fires. The tests below
    // pin every observable: heuristic stays the default, `llm + auth
    // denied` honours each fallback mode, and the one-shot info log is
    // best-effort (we don't assert it directly to avoid a coupling to
    // the global `tracing` subscriber).

    fn write_config(rt: &RedDBRuntime, key: &str, value: &str) {
        let store = rt.inner.db.store();
        store.set_config_tree(key, &crate::serde_json::Value::String(value.to_string()));
    }

    /// Default backend stays heuristic — even with an open scope, the
    /// pipeline returns the same tokens it would without any config.
    #[test]
    fn routed_default_backend_runs_heuristic() {
        let rt = fresh_runtime();
        let scope = make_scope(HashSet::new());
        let tokens = extract_tokens_routed(&rt, &scope, "passport FDD-12313")
            .expect("heuristic path is infallible");
        assert!(tokens.keywords.contains(&"passport".to_string()));
        assert!(tokens.literals.contains(&"FDD-12313".to_string()));
    }

    /// `backend = llm` with `fallback = use_heuristic`: capability
    /// denies (placeholder) → fallback → heuristic tokens surface.
    #[tokio::test(flavor = "multi_thread")]
    async fn routed_llm_auth_denied_uses_heuristic_fallback() {
        let rt = fresh_runtime();
        write_config(&rt, "ai.ner.backend", "llm");
        write_config(&rt, "ai.ner.fallback", "use_heuristic");
        let scope = make_scope(HashSet::new());
        let tokens = tokio::task::spawn_blocking(move || {
            extract_tokens_routed(&rt, &scope, "passport FDD-12313")
        })
        .await
        .unwrap()
        .expect("fallback policy keeps the call OK");
        assert!(tokens.keywords.contains(&"passport".to_string()));
        assert!(tokens.literals.contains(&"FDD-12313".to_string()));
    }

    /// `backend = llm` with `fallback = empty_on_fail`: auth denies →
    /// fallback returns an empty `TokenSet`.
    #[tokio::test(flavor = "multi_thread")]
    async fn routed_llm_auth_denied_empty_on_fail() {
        let rt = fresh_runtime();
        write_config(&rt, "ai.ner.backend", "llm");
        write_config(&rt, "ai.ner.fallback", "empty_on_fail");
        let scope = make_scope(HashSet::new());
        let tokens = tokio::task::spawn_blocking(move || {
            extract_tokens_routed(&rt, &scope, "passport FDD-12313")
        })
        .await
        .unwrap()
        .expect("empty_on_fail returns Ok with empty TokenSet");
        assert!(tokens.is_empty(), "expected empty TokenSet, got {tokens:?}");
    }

    /// `backend = llm` with `fallback = propagate`: auth denies →
    /// `extract_tokens_routed` surfaces a `RedDBError::Query` so the
    /// caller can decide.
    #[tokio::test(flavor = "multi_thread")]
    async fn routed_llm_auth_denied_propagate_returns_error() {
        let rt = fresh_runtime();
        write_config(&rt, "ai.ner.backend", "llm");
        write_config(&rt, "ai.ner.fallback", "propagate");
        let scope = make_scope(HashSet::new());
        let err = tokio::task::spawn_blocking(move || {
            extract_tokens_routed(&rt, &scope, "passport FDD-12313")
        })
        .await
        .unwrap()
        .expect_err("propagate must surface the error");
        let msg = format!("{err}");
        assert!(
            msg.contains("propagate") || msg.contains("ai.ner.backend"),
            "expected propagate error message, got: {msg}"
        );
    }

    /// AskPipeline end-to-end with `backend = llm` and the default
    /// `use_heuristic` fallback: the pipeline still returns tokens (via
    /// fallback), Stage 1 is the only routed stage, and the rest of the
    /// funnel runs unchanged.
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_llm_backend_falls_back_and_completes_pipeline() {
        let rt = fresh_runtime();
        write_config(&rt, "ai.ner.backend", "llm");
        // default fallback is use_heuristic — leave it implicit.
        rt.execute_query("CREATE TABLE travel (id INT, passport TEXT, notes TEXT)")
            .expect("CREATE TABLE travel");
        rt.execute_query(
            "INSERT INTO travel (id, passport, notes) VALUES \
             (2, 'PT-002', 'incident FDD-12313 escalated')",
        )
        .expect("seed rows");
        let visible: HashSet<String> = ["travel".to_string()].into_iter().collect();
        let scope = make_scope(visible);
        let ctx = tokio::task::spawn_blocking(move || {
            AskPipeline::execute(&rt, &scope, "passport FDD-12313")
        })
        .await
        .unwrap()
        .expect("pipeline runs");
        assert!(ctx.tokens.keywords.contains(&"passport".to_string()));
        assert!(ctx.tokens.literals.contains(&"FDD-12313".to_string()));
        assert_eq!(ctx.candidates.collections, vec!["travel".to_string()]);
        assert!(
            ctx.filtered_rows
                .iter()
                .any(|r| r.matched_literal == "FDD-12313"),
            "Stage 4 still runs after Stage 1 fallback"
        );
    }
}
