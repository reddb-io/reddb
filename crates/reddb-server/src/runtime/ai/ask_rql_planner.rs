//! Deterministic `ASK ... AS RQL` planner.
//!
//! This is deliberately not text-to-SQL execution. It builds a small,
//! read-only RQL candidate from the same token/schema vocabulary used by
//! AskPipeline, validates the generated text through the parser, and returns
//! the candidate for caller approval/execution.

use std::collections::BTreeSet;

use crate::api::{RedDBError, RedDBResult};
use crate::runtime::ask_pipeline::{CandidateCollections, TokenSet};
use crate::storage::query::ast::QueryExpr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRqlPlan {
    pub rql: String,
    pub field: String,
    pub value: String,
    pub collection: Option<String>,
    pub candidate_fields: Vec<String>,
    pub candidate_collections: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn plan(
    question: &str,
    tokens: &TokenSet,
    candidates: &CandidateCollections,
    requested_collection: Option<&str>,
) -> RedDBResult<AskRqlPlan> {
    let candidate_fields = candidate_fields(tokens, candidates);
    let field = candidate_fields.first().cloned().ok_or_else(|| {
        RedDBError::Query(
            "ASK AS RQL could not infer a WHERE field from the prompt and schema vocabulary"
                .to_string(),
        )
    })?;
    validate_ident("field", &field)?;

    let value = literal_for_field(question, tokens, &field).ok_or_else(|| {
        RedDBError::Query(
            "ASK AS RQL could not infer a literal value for the generated WHERE clause".to_string(),
        )
    })?;

    let collection = requested_collection.map(str::to_string);
    if let Some(collection) = &collection {
        validate_ident("collection", collection)?;
    }

    let rql = if let Some(collection) = &collection {
        format!(
            "SELECT * FROM {} WHERE {} = {}",
            collection,
            field,
            sql_string_literal(&value)
        )
    } else {
        format!("SELECT * WHERE {} = {}", field, sql_string_literal(&value))
    };

    validate_read_only_table_query(&rql)?;

    let mut warnings = Vec::new();
    if candidate_fields.len() > 1 {
        warnings.push(format!(
            "multiple candidate fields matched; selected `{}` from {:?}",
            field, candidate_fields
        ));
    }
    if tokens.literals.len() > 1 {
        warnings.push(format!(
            "multiple literal tokens matched; selected `{}` from {:?}",
            value, tokens.literals
        ));
    }
    if requested_collection.is_none() {
        warnings.push(
            "no COLLECTION was specified; generated RQL uses implicit universal source `any`"
                .to_string(),
        );
    }

    Ok(AskRqlPlan {
        rql,
        field,
        value,
        collection,
        candidate_fields,
        candidate_collections: candidates.collections.clone(),
        warnings,
    })
}

fn candidate_fields(tokens: &TokenSet, candidates: &CandidateCollections) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for columns in candidates.columns_by_collection.values() {
        for column in columns {
            all.insert(column.clone());
        }
    }

    let mut ordered = Vec::new();
    for keyword in &tokens.keywords {
        for column in &all {
            if column.eq_ignore_ascii_case(keyword) && !ordered.contains(column) {
                ordered.push(column.clone());
            }
        }
    }
    for column in all {
        if !ordered.contains(&column) {
            ordered.push(column);
        }
    }
    ordered
}

fn literal_for_field(question: &str, tokens: &TokenSet, field: &str) -> Option<String> {
    if let Some(literal) = tokens.literals.first() {
        return Some(literal.clone());
    }

    let terms = question_terms(question);
    for (idx, term) in terms.iter().enumerate() {
        if term.eq_ignore_ascii_case(field) {
            if let Some(next) = terms.get(idx + 1) {
                return Some(next.clone());
            }
        }
    }

    terms
        .into_iter()
        .find(|term| term.chars().any(|c| c.is_ascii_digit()) && term.len() >= 3)
}

fn question_terms(question: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut buf = String::new();
    for ch in question.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' || ch == ':' {
            buf.push(ch);
        } else if !buf.is_empty() {
            terms.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        terms.push(buf);
    }
    terms
}

fn validate_ident(kind: &str, value: &str) -> RedDBResult<()> {
    let mut chars = value.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(())
    } else {
        Err(RedDBError::Query(format!(
            "ASK AS RQL inferred unsafe {kind} identifier `{value}`"
        )))
    }
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn validate_read_only_table_query(rql: &str) -> RedDBResult<()> {
    let parsed = crate::storage::query::parser::parse(rql)
        .map_err(|err| RedDBError::Query(err.to_string()))?;
    match parsed.query {
        QueryExpr::Table(table) if table.filter.is_some() => Ok(()),
        QueryExpr::Table(_) => Err(RedDBError::Query(
            "ASK AS RQL generated a table query without a WHERE clause".to_string(),
        )),
        other => Err(RedDBError::Query(format!(
            "ASK AS RQL generated a non-table query: {other:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Inference path (#1273): augment — not replace — the deterministic planner.
//
// `ASK '<natural language>'` can be translated to an RQL candidate by the
// configured text2text (generate) provider. The model output is *never*
// trusted: it is always re-validated through the production parser via the
// same read-only-candidate seam used by the deterministic planner, and a
// mutating candidate is never auto-executed regardless of `EXECUTE`.
// ---------------------------------------------------------------------------

/// Read-only vs mutating classification of a parser-validated candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateDisposition {
    /// The candidate only reads (SELECT / MATCH / vector / search / …) and
    /// may be auto-executed when `EXECUTE` is requested.
    ReadOnly,
    /// The candidate writes, drops, alters, or otherwise mutates state and
    /// is refused for auto-execution regardless of `EXECUTE`.
    Mutating,
}

/// A model-generated RQL candidate that has passed the production parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCandidate {
    /// The candidate RQL, trimmed, exactly as it parsed.
    pub rql: String,
    /// Whether the candidate is read-only or mutating.
    pub disposition: CandidateDisposition,
    /// Canonical statement-type label for the candidate.
    pub statement_type: &'static str,
}

impl ValidatedCandidate {
    /// True when the candidate is safe to auto-execute.
    pub fn is_read_only(&self) -> bool {
        matches!(self.disposition, CandidateDisposition::ReadOnly)
    }
}

/// Result of an inference translation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRqlInference {
    /// The parser-validated candidate.
    pub candidate: ValidatedCandidate,
    /// The prompt that was sent to the model (for explain/audit).
    pub prompt: String,
    /// Whether the candidate should be auto-executed by the caller. True
    /// only when `EXECUTE` was requested *and* the candidate is read-only.
    pub execute: bool,
    /// Non-fatal advisories surfaced to the caller.
    pub warnings: Vec<String>,
}

/// A model that turns an inference prompt into a single RQL candidate.
///
/// The blanket impl over `Fn(&str) -> RedDBResult<String>` lets callers
/// pass a closure wrapping the configured provider (production) or a canned
/// string (tests / mock model) without a bespoke type.
pub trait RqlModel {
    fn generate_rql(&self, prompt: &str) -> RedDBResult<String>;
}

impl<F> RqlModel for F
where
    F: Fn(&str) -> RedDBResult<String>,
{
    fn generate_rql(&self, prompt: &str) -> RedDBResult<String> {
        self(prompt)
    }
}

/// Re-validate a model-generated RQL candidate through the production
/// parser and classify it read-only vs mutating. An invalid candidate
/// surfaces an error and is never returned for execution.
pub fn validate_candidate(rql: &str) -> RedDBResult<ValidatedCandidate> {
    let trimmed = rql.trim();
    if trimmed.is_empty() {
        return Err(RedDBError::Query(
            "ASK inference produced an empty RQL candidate".to_string(),
        ));
    }
    let parsed = crate::storage::query::parser::parse(trimmed).map_err(|err| {
        RedDBError::Query(format!(
            "ASK inference produced an invalid RQL candidate: {err}"
        ))
    })?;
    let (disposition, statement_type) = classify(&parsed.query);
    Ok(ValidatedCandidate {
        rql: trimmed.to_string(),
        disposition,
        statement_type,
    })
}

/// Classify a parsed query as read-only or mutating. The read-only set is
/// an explicit allowlist; anything not on it (writes, DDL, migrations,
/// maintenance, unknown future variants) is treated as mutating so it is
/// never auto-executed.
fn classify(query: &QueryExpr) -> (CandidateDisposition, &'static str) {
    use CandidateDisposition::{Mutating, ReadOnly};
    match query {
        QueryExpr::Table(_) => (ReadOnly, "select"),
        QueryExpr::Graph(_) => (ReadOnly, "graph"),
        QueryExpr::Join(_) => (ReadOnly, "join"),
        QueryExpr::Path(_) => (ReadOnly, "path"),
        QueryExpr::Vector(_) => (ReadOnly, "vector"),
        QueryExpr::Hybrid(_) => (ReadOnly, "hybrid"),
        QueryExpr::GraphCommand(_) => (ReadOnly, "graph_command"),
        QueryExpr::SearchCommand(_) => (ReadOnly, "search"),
        QueryExpr::RankOf(_) => (ReadOnly, "rank_of"),
        QueryExpr::ApproxRankOf(_) => (ReadOnly, "approx_rank_of"),
        QueryExpr::RankRange(_) => (ReadOnly, "rank_range"),
        QueryExpr::Insert(_) => (Mutating, "insert"),
        QueryExpr::Update(_) => (Mutating, "update"),
        QueryExpr::Delete(_) => (Mutating, "delete"),
        QueryExpr::Truncate(_) => (Mutating, "truncate"),
        _ => (Mutating, "mutating"),
    }
}

/// Translate a natural-language question into a parser-validated RQL
/// candidate via the supplied model, then apply the read-only-candidate /
/// `EXECUTE` policy.
///
/// - The candidate is *always* re-validated through the parser.
/// - Default (`execute = false`) returns the candidate without executing.
/// - `execute = true` marks read-only candidates for auto-execution.
/// - A mutating candidate is refused for auto-execution when `execute`
///   is requested, and is never marked executable.
pub fn infer<M: RqlModel>(
    question: &str,
    candidates: &CandidateCollections,
    requested_collection: Option<&str>,
    execute: bool,
    model: &M,
) -> RedDBResult<AskRqlInference> {
    let prompt = build_inference_prompt(question, candidates, requested_collection);
    let raw = model.generate_rql(&prompt)?;
    let candidate = validate_candidate(&raw)?;

    let mut warnings = Vec::new();
    let execute = if execute {
        match candidate.disposition {
            CandidateDisposition::ReadOnly => true,
            CandidateDisposition::Mutating => {
                return Err(RedDBError::Query(format!(
                    "ASK ... EXECUTE refused: generated `{}` candidate is mutating and is \
                     never auto-executed",
                    candidate.statement_type
                )));
            }
        }
    } else {
        if candidate.is_read_only() {
            warnings.push(
                "candidate not executed; add EXECUTE to auto-run read-only candidates".to_string(),
            );
        } else {
            warnings.push(format!(
                "candidate is a mutating `{}` statement and is never auto-executed",
                candidate.statement_type
            ));
        }
        false
    };

    if requested_collection.is_none() {
        warnings.push(
            "no COLLECTION was specified; candidate validated against the full schema vocabulary"
                .to_string(),
        );
    }

    Ok(AskRqlInference {
        candidate,
        prompt,
        execute,
        warnings,
    })
}

/// Assemble the inference prompt: the question plus the schema vocabulary
/// (collections + columns) the model may reference, with an explicit
/// instruction to emit a single read-only RQL statement.
pub fn build_inference_prompt(
    question: &str,
    candidates: &CandidateCollections,
    requested_collection: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Translate the user's question into a single read-only RQL SELECT statement. \
         Return only the RQL, with no commentary, code fences, or trailing semicolon.\n\n",
    );

    if let Some(collection) = requested_collection {
        prompt.push_str("Target collection: ");
        prompt.push_str(collection);
        prompt.push('\n');
    }

    if !candidates.collections.is_empty() {
        prompt.push_str("Available collections: ");
        prompt.push_str(&candidates.collections.join(", "));
        prompt.push('\n');
    }

    let mut columns: BTreeSet<String> = BTreeSet::new();
    for cols in candidates.columns_by_collection.values() {
        for col in cols {
            columns.insert(col.clone());
        }
    }
    if !columns.is_empty() {
        prompt.push_str("Available columns: ");
        prompt.push_str(&columns.into_iter().collect::<Vec<_>>().join(", "));
        prompt.push('\n');
    }

    prompt.push_str("\nQuestion: ");
    prompt.push_str(question);
    prompt
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn candidates() -> CandidateCollections {
        CandidateCollections {
            collections: vec!["incidents".to_string(), "travelers".to_string()],
            columns_by_collection: HashMap::from([
                ("incidents".to_string(), vec!["host".to_string()]),
                ("travelers".to_string(), vec!["passport".to_string()]),
            ]),
        }
    }

    #[test]
    fn plans_universal_field_literal_query() {
        let tokens = TokenSet {
            keywords: vec!["who".to_string(), "passport".to_string()],
            literals: vec!["FDD-12313".to_string()],
        };
        let plan = plan("who owns passport FDD-12313?", &tokens, &candidates(), None).unwrap();
        assert_eq!(plan.rql, "SELECT * WHERE passport = 'FDD-12313'");
        assert_eq!(plan.field, "passport");
        assert_eq!(plan.value, "FDD-12313");
    }

    #[test]
    fn plans_collection_scoped_query_with_ip_value() {
        let tokens = TokenSet {
            keywords: vec!["host".to_string()],
            literals: Vec::new(),
        };
        let plan = plan("host 10.0.0.1", &tokens, &candidates(), Some("incidents")).unwrap();
        assert_eq!(plan.rql, "SELECT * FROM incidents WHERE host = '10.0.0.1'");
    }

    #[test]
    fn rejects_missing_field() {
        let tokens = TokenSet {
            keywords: vec!["anything".to_string()],
            literals: vec!["FDD-12313".to_string()],
        };
        let err = plan(
            "anything FDD-12313",
            &tokens,
            &CandidateCollections::default(),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("could not infer a WHERE field"));
    }

    // ---- inference path (#1273) ------------------------------------

    /// A mock model that returns a fixed candidate string regardless of
    /// the prompt — stands in for the configured generate provider.
    fn mock_model(candidate: &'static str) -> impl RqlModel {
        move |_prompt: &str| Ok(candidate.to_string())
    }

    #[test]
    fn infer_validates_candidate_through_parser() {
        let err = infer(
            "anything",
            &candidates(),
            None,
            false,
            &mock_model("this is not valid rql"),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid RQL candidate"),
            "got: {err}"
        );
    }

    #[test]
    fn infer_default_returns_candidate_without_executing() {
        let out = infer(
            "who owns passport FDD-12313?",
            &candidates(),
            Some("travelers"),
            false,
            &mock_model("SELECT * FROM travelers WHERE passport = 'FDD-12313'"),
        )
        .unwrap();
        assert!(!out.execute, "default must not execute");
        assert!(out.candidate.is_read_only());
        assert_eq!(out.candidate.statement_type, "select");
        assert_eq!(
            out.candidate.rql,
            "SELECT * FROM travelers WHERE passport = 'FDD-12313'"
        );
    }

    #[test]
    fn infer_execute_marks_read_only_candidate_for_run() {
        let out = infer(
            "list travelers",
            &candidates(),
            Some("travelers"),
            true,
            &mock_model("SELECT * FROM travelers WHERE passport = 'FDD-12313'"),
        )
        .unwrap();
        assert!(out.execute, "EXECUTE on read-only candidate must run");
        assert!(out.candidate.is_read_only());
    }

    #[test]
    fn infer_refuses_mutating_candidate_for_execute() {
        let err = infer(
            "delete everything",
            &candidates(),
            Some("travelers"),
            true,
            &mock_model("DELETE FROM travelers WHERE passport = 'FDD-12313'"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("refused"), "got: {err}");
        assert!(err.to_string().contains("mutating"), "got: {err}");
    }

    #[test]
    fn infer_mutating_candidate_never_executes_without_execute() {
        let out = infer(
            "delete everything",
            &candidates(),
            Some("travelers"),
            false,
            &mock_model("DELETE FROM travelers WHERE passport = 'FDD-12313'"),
        )
        .unwrap();
        assert!(!out.execute);
        assert_eq!(out.candidate.disposition, CandidateDisposition::Mutating);
        assert_eq!(out.candidate.statement_type, "delete");
    }

    #[test]
    fn validate_candidate_rejects_empty() {
        let err = validate_candidate("   ").unwrap_err();
        assert!(err.to_string().contains("empty RQL candidate"));
    }
}
