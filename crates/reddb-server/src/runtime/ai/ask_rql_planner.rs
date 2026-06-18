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
}
