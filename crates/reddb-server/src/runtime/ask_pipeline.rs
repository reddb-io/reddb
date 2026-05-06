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
use std::time::Instant;

use tracing::{debug, info_span, warn};

use super::statement_frame::{EffectiveScope, ReadFrame};
use super::RedDBRuntime;
use crate::api::{RedDBError, RedDBResult};
use crate::storage::unified::entity::{EntityData, UnifiedEntity};
use crate::storage::schema::Value;

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
#[derive(Debug, Clone, Default)]
pub struct AskContext {
    pub question: String,
    pub tokens: TokenSet,
    pub candidates: CandidateCollections,
    pub vector_hits: Vec<VectorHit>,
    pub filtered_rows: Vec<FilteredRow>,
    pub timings: StageTimings,
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
        let span = info_span!(
            "ask_pipeline.execute",
            tenant = ?scope.effective_scope(),
            question_len = question.len(),
            row_cap = row_cap,
        );
        let _enter = span.enter();

        // Stage 1.
        let stage1 = Instant::now();
        let tokens = extract_tokens(question);
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
        let vector_hits = vector_search_scoped(runtime, scope, question, &candidates, row_cap);
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
            timings: StageTimings {
                extract_us,
                schema_us,
                vector_us,
                filter_us,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Stage 1 — token / entity extraction (heuristic v1).
// ---------------------------------------------------------------------------

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

    let flush =
        |buf: &mut String, keywords: &mut Vec<String>, literals: &mut Vec<String>| {
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
        && word.chars().all(|c| c.is_ascii_digit() || c == '-' || c.is_ascii_uppercase())
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
    "a", "about", "an", "and", "are", "as", "at", "be", "by", "do", "for", "from",
    "how", "in", "is", "it", "of", "on", "or", "que", "qual", "quais", "sobre", "te",
    "the", "to", "what", "where", "which", "with",
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
            0.0,
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
    let response = crate::ai::openai_embeddings(crate::ai::OpenAiEmbeddingRequest {
        api_key,
        model,
        inputs: vec![question.to_string()],
        dimensions: None,
        api_base: provider.resolve_api_base(),
    })
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
            if let Some(hit) =
                literal_match_in_entity(&entity, &tokens.literals, hint_columns)
            {
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
        assert_eq!(
            tokens.literals.iter().filter(|l| *l == "FDD-1").count(),
            1
        );
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
        assert!(first_literal_in_value(
            &Value::text("nothing here"),
            &["FDD-12313".to_string()],
        )
        .is_none());
    }
}
