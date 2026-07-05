//! Read-only RQL candidate validation for the planner-first ASK path.
//!
//! The deterministic `ASK ... AS RQL` planner and its LLM inference variant
//! were removed in the ADR 0068 clean break (#1751): auto-execution of
//! read-only candidates is now the default and the planner-first path
//! ([`super::ask_planner`]) owns candidate generation. What survives here is
//! the shared seam that path depends on — re-validate a candidate RQL string
//! through the production parser and classify it read-only vs mutating so a
//! mutating candidate is never auto-executed.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::query::ast::QueryExpr;

/// Read-only vs mutating classification of a parser-validated candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateDisposition {
    /// The candidate only reads (SELECT / MATCH / vector / search / …) and
    /// may be auto-executed.
    ReadOnly,
    /// The candidate writes, drops, alters, or otherwise mutates state and
    /// is refused for auto-execution under any flag.
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

/// Re-validate a model-generated RQL candidate through the production
/// parser and classify it read-only vs mutating. An invalid candidate
/// surfaces an error and is never returned for execution.
pub fn validate_candidate(rql: &str) -> RedDBResult<ValidatedCandidate> {
    let trimmed = rql.trim();
    if trimmed.is_empty() {
        return Err(RedDBError::Query(
            "ASK planner produced an empty RQL candidate".to_string(),
        ));
    }
    let parsed = crate::storage::query::parser::parse(trimmed).map_err(|err| {
        RedDBError::Query(format!(
            "ASK planner produced an invalid RQL candidate: {err}"
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
        // Common DDL / streaming statements get distinct kind labels so the
        // how-to suggestion envelope and its audit row record what was
        // proposed. They remain `Mutating` — kept advisory, never executed.
        QueryExpr::CreateTable(_) => (Mutating, "create_table"),
        QueryExpr::CreateCollection(_) => (Mutating, "create_collection"),
        QueryExpr::CreateQueue(_) => (Mutating, "create_queue"),
        QueryExpr::CreateIndex(_) => (Mutating, "create_index"),
        QueryExpr::AlterTable(_) => (Mutating, "alter_table"),
        QueryExpr::EventsBackfill(_) => (Mutating, "events_backfill"),
        _ => (Mutating, "mutating"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_candidate_classifies_read_only_select() {
        let candidate =
            validate_candidate("SELECT * FROM travelers WHERE passport = 'FDD-1'").unwrap();
        assert!(candidate.is_read_only());
        assert_eq!(candidate.statement_type, "select");
    }

    #[test]
    fn validate_candidate_classifies_mutating_delete() {
        let candidate =
            validate_candidate("DELETE FROM travelers WHERE passport = 'FDD-1'").unwrap();
        assert_eq!(candidate.disposition, CandidateDisposition::Mutating);
        assert_eq!(candidate.statement_type, "delete");
    }

    #[test]
    fn validate_candidate_rejects_invalid() {
        let err = validate_candidate("this is not valid rql").unwrap_err();
        assert!(
            err.to_string().contains("invalid RQL candidate"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_candidate_rejects_empty() {
        let err = validate_candidate("   ").unwrap_err();
        assert!(err.to_string().contains("empty RQL candidate"));
    }
}
