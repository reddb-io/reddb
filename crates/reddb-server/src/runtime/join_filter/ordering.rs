//! Runtime ordering: ORDER BY comparison, sort, and top-K.
//!
//! Extracted from `join_filter.rs` as part of the join_filter
//! directory refactor (parent re-exports the whole module).
use super::*;

pub(crate) fn compare_runtime_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    compare_runtime_order_with_db(None, left, right, clauses, table_name, table_alias)
}

pub(crate) fn compare_runtime_order_with_db(
    db: Option<&RedDB>,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    let host = RuntimeOrderingHost {
        db,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::ordering::compare_runtime_order_with_host(
        &host,
        left,
        right,
        clauses,
        table_name,
        table_alias,
    )
}

pub(crate) fn sort_records_by_order_by(
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    sort_records_by_order_by_with_db(None, records, order_by, table_name, table_alias)
}

pub(crate) fn sort_records_by_order_by_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    let host = RuntimeOrderingHost {
        db,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::ordering::sort_records_by_order_by_with_host(
        &host,
        records,
        order_by,
        table_name,
        table_alias,
    )
}

/// Whether the `REDDB_DISABLE_TOPK` kill-switch is set. Cached so a
/// `std::env::var` lookup doesn't land in the per-query hot path once
/// the binary is warm.
pub(crate) fn topk_disabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("REDDB_DISABLE_TOPK")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "on" | "yes"))
            .unwrap_or(false)
    })
}

pub(crate) fn sort_or_top_k_records_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    offset: Option<u64>,
    limit: Option<u64>,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    let host = RuntimeOrderingHost {
        db,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::ordering::sort_or_top_k_records_with_host(
        &host,
        records,
        order_by,
        offset,
        limit,
        table_name,
        table_alias,
    )
}

pub(crate) fn top_k_records_by_order_by_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    k: usize,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    let host = RuntimeOrderingHost {
        db,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::ordering::top_k_records_by_order_by_with_host(
        &host,
        records,
        order_by,
        k,
        table_name,
        table_alias,
    )
}

pub(crate) fn compare_runtime_optional_values(
    left: Option<&Value>,
    right: Option<&Value>,
    nulls_first: bool,
) -> Ordering {
    reddb_engine::query::runtime::ordering::compare_runtime_optional_values(
        &crate::runtime::execution_context::secret_query_input,
        left,
        right,
        nulls_first,
    )
}

struct RuntimeOrderingHost<'a> {
    db: Option<&'a RedDB>,
    table_name: Option<&'a str>,
    table_alias: Option<&'a str>,
}

impl reddb_engine::query::runtime::ordering::OrderingHost for RuntimeOrderingHost<'_> {
    fn evaluate_expression(
        &self,
        expression: &reddb_rql::ast::Expr,
        record: &UnifiedRecord,
    ) -> Option<Value> {
        super::expr_eval::evaluate_runtime_expr_with_db(
            self.db,
            expression,
            record,
            self.table_name,
            self.table_alias,
        )
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        crate::runtime::execution_context::secret_query_input(value)
    }
    fn record_identity_key(&self, record: &UnifiedRecord) -> String {
        runtime_record_identity_key(record)
    }
    fn topk_disabled(&self) -> bool {
        topk_disabled()
    }
}
