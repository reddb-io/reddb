//! Concurrent-claim ORDER BY index-gate (ADR 0063, #1607).
//!
//! ADR 0063 requires a concurrent `CLAIM` to order its candidates through a
//! compatible index: "Without an explicit order, or with an order the planner
//! cannot serve through an index, the statement is rejected rather than relying
//! on physical layout, incidental scan order, or a broad write-path sort." The
//! parser already enforces that `CLAIM LIMIT n` / `CLAIM EXACT n` carries an
//! `ORDER BY`; this module supplies the semantic half — verifying that the
//! ordering maps to an index on the target collection.
//!
//! The check is intentionally storage-agnostic: it consumes the ordered column
//! list of every index already registered on the claim collection (looked up by
//! the catalog-bound runtime) and reasons purely over column names. That keeps
//! the crate graph acyclic — no `reddb-io-rql -> reddb-server` back-edge — while
//! the runtime owns the index catalog and turns a rejection into its planner
//! error variant.

use crate::ast::{FieldRef, OrderByClause, UpdateQuery};

/// The single indexable column a claim ORDER BY clause references, if any.
///
/// Expression sort keys (`ORDER BY <expr>`) have no single indexable column,
/// and non-column field references (node/edge properties, node ids) are not
/// table columns, so neither participates in the prefix check.
fn claim_order_column(clause: &OrderByClause) -> Option<&str> {
    if clause.expr.is_some() {
        return None;
    }
    match &clause.field {
        FieldRef::TableColumn { table, column } if table.is_empty() => Some(column.as_str()),
        _ => None,
    }
}

/// The user-facing ORDER BY columns a claim needs an index to serve.
///
/// The implicit `rid` tie-breaker the parser appends is the model's stable
/// claim identity (ADR 0063) — it is intrinsically ordered and always
/// available, so it is never index-gated and is filtered out here.
pub fn claim_order_columns(query: &UpdateQuery) -> Vec<String> {
    query
        .order_by
        .iter()
        .filter_map(claim_order_column)
        .filter(|column| !column.eq_ignore_ascii_case("rid"))
        .map(|column| column.to_string())
        .collect()
}

/// Whether some index in `available_indexes` (each an ordered column list)
/// covers `order_columns` as a leading prefix.
///
/// A compatible index covers the ORDER BY when the ordered claim columns are a
/// prefix of the index's columns — a single-column index on `col` covers
/// `ORDER BY col`, and a compound `(col_a, col_b)` index covers both
/// `ORDER BY col_a` and `ORDER BY col_a, col_b`. Column matching is
/// case-insensitive, matching the rest of the planner.
pub fn claim_order_is_index_backed(
    order_columns: &[String],
    available_indexes: &[Vec<String>],
) -> bool {
    if order_columns.is_empty() {
        return true;
    }
    available_indexes.iter().any(|index_columns| {
        order_columns.len() <= index_columns.len()
            && order_columns
                .iter()
                .zip(index_columns.iter())
                .all(|(want, have)| have.eq_ignore_ascii_case(want))
    })
}

/// Reject a concurrent `CLAIM ... ORDER BY` whose ordering cannot be served by
/// any compatible index on the target collection (ADR 0063).
///
/// `available_indexes` is the ordered column list of every index registered on
/// the claim collection. Returns `Ok(())` when the statement carries no claim,
/// orders only by the intrinsic claim identity, or a compatible index exists;
/// otherwise returns an error message naming the uncovered ORDER BY column(s)
/// so the operator knows which index to create.
pub fn check_claim_order_by_index_gate(
    query: &UpdateQuery,
    available_indexes: &[Vec<String>],
) -> Result<(), String> {
    if query.claim_limit.is_none() {
        return Ok(());
    }
    let order_columns = claim_order_columns(query);
    if claim_order_is_index_backed(&order_columns, available_indexes) {
        return Ok(());
    }
    Err(format!(
        "concurrent CLAIM on '{table}' requires an index covering ORDER BY column(s) {columns}; \
         create a compatible index (e.g. CREATE INDEX ON {table} ({columns}))",
        table = query.table,
        columns = order_columns.join(", "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::QueryExpr;
    use crate::parser::Parser;

    fn update_query(sql: &str) -> UpdateQuery {
        let mut parser = Parser::new(sql).expect("lexer");
        match parser.parse().expect("parse should succeed") {
            QueryExpr::Update(query) => query,
            other => panic!("expected UPDATE, got {other:?}"),
        }
    }

    #[test]
    fn rejects_claim_order_by_without_covering_index() {
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 2 ORDER BY rank ASC",
        );
        let err = check_claim_order_by_index_gate(&query, &[])
            .expect_err("claim without a covering index should be rejected");
        assert!(err.contains("rank"), "message should name the column: {err}");
        assert!(err.contains("tasks"), "message should name the collection: {err}");
    }

    #[test]
    fn accepts_claim_order_by_with_single_column_index() {
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 2 ORDER BY rank ASC",
        );
        let indexes = vec![vec!["rank".to_string()]];
        assert!(check_claim_order_by_index_gate(&query, &indexes).is_ok());
    }

    #[test]
    fn claim_exact_follows_the_same_gate() {
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM EXACT 2 ORDER BY rank ASC",
        );
        assert!(check_claim_order_by_index_gate(&query, &[]).is_err());
        let indexes = vec![vec!["rank".to_string()]];
        assert!(check_claim_order_by_index_gate(&query, &indexes).is_ok());
    }

    #[test]
    fn compound_index_covers_leading_order_column() {
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 1 ORDER BY rank ASC",
        );
        // A compound `(rank, created_at)` index still covers `ORDER BY rank`.
        let indexes = vec![vec!["rank".to_string(), "created_at".to_string()]];
        assert!(check_claim_order_by_index_gate(&query, &indexes).is_ok());
    }

    #[test]
    fn implicit_rid_tiebreaker_is_not_index_gated() {
        // The parser appends `rid` as the stable claim-identity tie-breaker;
        // ordering only by `rid` needs no secondary index.
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 1 ORDER BY rid ASC",
        );
        assert!(claim_order_columns(&query).is_empty());
        assert!(check_claim_order_by_index_gate(&query, &[]).is_ok());
    }

    #[test]
    fn non_claim_update_is_never_gated() {
        let query = update_query("UPDATE tasks SET status = 'done' ORDER BY rank ASC LIMIT 5");
        assert!(check_claim_order_by_index_gate(&query, &[]).is_ok());
    }

    #[test]
    fn column_matching_is_case_insensitive() {
        let query = update_query(
            "UPDATE tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 1 ORDER BY Rank ASC",
        );
        let indexes = vec![vec!["rank".to_string()]];
        assert!(check_claim_order_by_index_gate(&query, &indexes).is_ok());
    }
}
