use super::*;
use crate::storage::query::ast::{Expr, SelectItem};
use crate::storage::query::evaluator;
use crate::storage::query::sql_lowering::{
    effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
    effective_table_projections,
};

mod aggregate;
mod aggregate_planner;
mod aggregate_pushdown_dispatch;
mod filter_compiled;
mod helpers;
mod hybrid;
mod indexed_scan;
mod join;
mod json_writers;
mod table;
mod vector;

// Re-export public helpers so call sites outside this module
// (e.g. `super::*` in runtime/*.rs, and `crate::runtime::query_exec::X`
// in wire/listener.rs) keep working.
//
// `pub(crate)` is needed for the two helpers because wire/listener.rs
// reaches them via `crate::runtime::query_exec::X` (cross-module path,
// not just super-path).
pub(crate) use filter_compiled::CompiledEntityFilter;
pub(crate) use helpers::{
    evaluate_entity_filter, evaluate_entity_filter_with_db, extract_entity_id_from_filter,
    extract_zone_predicates, try_hash_eq_lookup,
};
pub(super) use hybrid::execute_runtime_hybrid_query;
pub(crate) use indexed_scan::try_sorted_index_lookup;
pub(super) use join::execute_runtime_join_query;
pub(super) use json_writers::execute_runtime_serialize_single_entity;
pub(super) use vector::execute_runtime_vector_query;

// Private imports used by functions still in query_exec.rs.
use aggregate::{execute_aggregate_query, has_aggregate_projections};
use table::{
    execute_runtime_canonical_table_node, execute_runtime_canonical_table_query_indexed,
    RuntimeTableExecutionContext,
};

pub(super) fn execute_runtime_table_query(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<UnifiedResult> {
    let effective_projections = effective_table_projections(query);
    let effective_filter = effective_table_filter(query);
    let effective_group_by = effective_table_group_by_exprs(query);
    let effective_having = effective_table_having_filter(query);

    // Scalar SELECT without FROM: evaluate once against an empty row
    // instead of scanning implicit universal records.
    if table_query_is_implicit_scalar_select(query)
        && !has_aggregate_projections(&effective_projections)
        && effective_group_by.is_empty()
        && effective_having.is_none()
    {
        let source = UnifiedRecord::new();
        let filter_matches = effective_filter.as_ref().is_none_or(|filter| {
            super::join_filter::evaluate_runtime_filter_with_db(
                Some(db),
                &source,
                filter,
                None,
                None,
            )
        });
        let mut records = if filter_matches {
            vec![project_scalar_via_evaluator(
                Some(db),
                &query.select_items,
                &effective_projections,
                &source,
            )?]
        } else {
            Vec::new()
        };

        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset >= records.len() {
                records.clear();
            } else {
                records.drain(..offset);
            }
        }
        if let Some(limit) = query.limit {
            records.truncate(limit as usize);
        }

        let columns = projected_columns(&records, &effective_projections);
        return Ok(UnifiedResult {
            columns,
            records,
            stats: Default::default(),
            pre_serialized_json: None,
        });
    }

    // ── AGGREGATE PATH: COUNT, AVG, SUM, MIN, MAX, GROUP BY ──
    if has_aggregate_projections(&effective_projections) {
        return execute_aggregate_query(db, query);
    }

    // ── FAST ENTITY-ID PATH: O(1) lookup for WHERE _entity_id = N ──
    //
    // Previously this path emitted only `pre_serialized_json` and
    // left `records` empty, which broke every consumer that walks
    // `result.records` (including the embedded runtime API, the
    // Secret decryption post-pass, and the CLI).  We now materialize
    // a `UnifiedRecord` as well — JSON callers still get the fast
    // pre-serialized blob, but non-HTTP callers see the row too.
    if effective_filter.is_some()
        && query.order_by.is_empty()
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && query.offset.is_none()
        && !is_universal_query_source(&query.table)
    {
        if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
            if let Some(entity) =
                resolve_table_row_by_logical_id(db, &query.table, EntityId::new(entity_id))
            {
                let json = execute_runtime_serialize_single_entity(&entity);
                let records: Vec<UnifiedRecord> = runtime_table_record_from_entity(entity)
                    .into_iter()
                    .collect();
                let columns = projected_columns(&records, &effective_projections);
                return Ok(UnifiedResult {
                    columns,
                    records,
                    stats: crate::storage::query::unified::QueryStats {
                        rows_scanned: 1,
                        ..Default::default()
                    },
                    pre_serialized_json: Some(json),
                });
            }
            return Ok(UnifiedResult::default());
        }
    }

    let mut records = execute_runtime_canonical_table_query_indexed(db, query, index_store)?;
    // Issue #580 — DeclarativeRetention slice 1. Lazy-on-scan: drop
    // rows older than `now - retention_duration_ms` after the scan
    // assembles them, before the result reaches the caller. No-op for
    // collections without a retention policy on the contract.
    let contract = db.collection_contract(query.table.as_str());
    crate::runtime::retention_filter::apply(&mut records, contract.as_ref());
    // Issue #585 slice 8 — SESSIONIZE post-scan operator. Annotates
    // each row with `session_id` (opaque base32) based on the
    // (actor_col, gap_ms, order_col) supplied either explicitly on
    // the clause or by the source contract's SESSION_KEY/SESSION_GAP
    // descriptor defaults. Errors as `RedDBError::Query(
    // "MissingSessionKey: …")` when neither side supplies the
    // required fields.
    if let Some(clause) = query.sessionize.as_ref() {
        crate::runtime::sessionize::apply(&mut records, contract.as_ref(), clause)?;
    }
    let columns = projected_columns(&records, &effective_projections);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

fn resolve_table_row_by_logical_id(
    db: &RedDB,
    table: &str,
    logical_id: EntityId,
) -> Option<UnifiedEntity> {
    let store = db.store();
    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver::captured(snapshot)
        .resolve_logical_id(&store, table, logical_id)
}

pub(super) fn runtime_record_has_document_capability(record: &UnifiedRecord) -> bool {
    record
        .get("red_capabilities")
        .and_then(|value| match value {
            crate::storage::schema::Value::Text(value) => Some(value),
            _ => None,
        })
        .map(|capabilities| {
            capabilities
                .split(',')
                .any(|capability| capability.trim() == "document")
        })
        .unwrap_or(false)
}

fn table_query_is_implicit_scalar_select(query: &TableQuery) -> bool {
    query.table == "any"
        && query.alias.is_none()
        && query.source.is_none()
        && !query.select_items.is_empty()
        && query
            .select_items
            .iter()
            .all(select_item_is_source_free_scalar)
}

fn select_item_is_source_free_scalar(item: &SelectItem) -> bool {
    match item {
        SelectItem::Wildcard => false,
        SelectItem::Expr { expr, .. } => expr_is_source_free(expr),
    }
}

fn expr_is_source_free(expr: &Expr) -> bool {
    match expr {
        Expr::Literal { .. } | Expr::Parameter { .. } => true,
        Expr::Column { .. } => false,
        Expr::UnaryOp { operand, .. } => expr_is_source_free(operand),
        Expr::BinaryOp { lhs, rhs, .. } => expr_is_source_free(lhs) && expr_is_source_free(rhs),
        Expr::Cast { inner, .. } => expr_is_source_free(inner),
        Expr::FunctionCall { name, args, .. } => {
            if name.eq_ignore_ascii_case("CONFIG") {
                return (1..=2).contains(&args.len())
                    && expr_is_path_like(&args[0])
                    && args.get(1).is_none_or(|expr| {
                        matches!(expr, Expr::Column { .. }) || expr_is_source_free(expr)
                    });
            }
            if name.eq_ignore_ascii_case("KV") {
                return (2..=3).contains(&args.len())
                    && expr_is_path_like(&args[0])
                    && expr_is_path_like(&args[1])
                    && args.get(2).is_none_or(|expr| {
                        matches!(expr, Expr::Column { .. }) || expr_is_source_free(expr)
                    });
            }
            args.iter().all(expr_is_source_free)
        }
        Expr::Case {
            branches, else_, ..
        } => {
            branches
                .iter()
                .all(|(cond, value)| expr_is_source_free(cond) && expr_is_source_free(value))
                && else_.as_deref().is_none_or(expr_is_source_free)
        }
        Expr::IsNull { operand, .. } => expr_is_source_free(operand),
        Expr::InList { target, values, .. } => {
            expr_is_source_free(target) && values.iter().all(expr_is_source_free)
        }
        Expr::Between {
            target, low, high, ..
        } => expr_is_source_free(target) && expr_is_source_free(low) && expr_is_source_free(high),
        Expr::Subquery { .. } => false,
    }
}

fn expr_is_path_like(expr: &Expr) -> bool {
    matches!(expr, Expr::Literal { .. } | Expr::Column { .. })
}

/// Evaluate a source-free scalar SELECT list via [`evaluator::evaluate`].
///
/// Used for `SELECT expr [AS alias] FROM any` when every expression is
/// column-free (no table scan needed). Falls back to
/// `join_filter::project_runtime_record_with_db` per-projection for special
/// functions (CONFIG, KV, ML_*) that the new evaluator doesn't handle yet.
fn project_scalar_via_evaluator(
    db: Option<&RedDB>,
    items: &[SelectItem],
    projections: &[Projection],
    source: &UnifiedRecord,
) -> RedDBResult<UnifiedRecord> {
    let empty_row: &dyn evaluator::Row = &|_: &FieldRef| -> Option<Value> { None };
    let mut record = UnifiedRecord::new();
    for (item, proj) in items.iter().zip(projections.iter()) {
        let SelectItem::Expr { expr, .. } = item else {
            continue;
        };
        let col_name = super::join_filter::projection_name(proj);
        let value = match evaluator::evaluate(expr, empty_row) {
            Ok(v) => v,
            // Fall back for CONFIG, KV, ML_* and any other special-cased
            // functions the evaluator does not cover yet.
            Err(evaluator::EvalError::UnknownFunction { .. }) => {
                super::join_filter::project_runtime_record_with_db(
                    db,
                    source,
                    std::slice::from_ref(proj),
                    None,
                    None,
                    false,
                    false,
                )
                .get(col_name.as_str())
                .cloned()
                .unwrap_or(Value::Null)
            }
            Err(err) => return Err(RedDBError::Query(err.to_string())),
        };
        record.set(&col_name, value);
    }
    Ok(record)
}

pub(super) fn evaluate_runtime_document_filter(
    record: &UnifiedRecord,
    filter: &crate::storage::query::ast::Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter(record, filter, table_name, table_alias)
}

pub(super) fn runtime_record_rank_score(record: &UnifiedRecord) -> f64 {
    [
        "_score",
        "hybrid_score",
        "final_score",
        "score",
        "graph_score",
        "table_score",
        "graph_match",
        "vector_score",
        "vector_similarity",
        "structured_score",
        "structured_match",
        "text_relevance",
    ]
    .into_iter()
    .find_map(|field| record.get(field).and_then(runtime_value_number))
    .unwrap_or(0.0)
}

pub(super) fn compare_runtime_ranked_records(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
) -> Ordering {
    runtime_record_rank_score(right)
        .partial_cmp(&runtime_record_rank_score(left))
        .unwrap_or(Ordering::Equal)
        .then_with(|| runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right)))
}

pub(super) fn execute_runtime_canonical_expr_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    expr: &QueryExpr,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match expr {
        QueryExpr::Table(table) => {
            let table_name = table.table.as_str();
            let table_alias = table.alias.as_deref().unwrap_or(table_name);
            let context = RuntimeTableExecutionContext {
                query: table,
                table_name,
                table_alias,
            };
            execute_runtime_canonical_table_node(db, node, &context)
        }
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            let node_properties = materialize_graph_node_properties(db.store().as_ref())?;
            let edge_properties = materialize_graph_edge_properties(db.store().as_ref())?;
            let result =
                crate::storage::query::unified::UnifiedExecutor::execute_on_with_graph_properties(
                    &graph,
                    expr,
                    node_properties,
                    edge_properties,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            Ok(result.records)
        }
        QueryExpr::Vector(vector) => Ok(execute_runtime_vector_query(db, vector)?.records),
        QueryExpr::Hybrid(hybrid) => Ok(execute_runtime_hybrid_query(db, hybrid)?.records),
        QueryExpr::Join(join) => join::execute_runtime_canonical_join_node(db, node, join),
        other => Err(RedDBError::Query(format!(
            "canonical join execution does not yet support {} child expressions",
            query_expr_name(other)
        ))),
    }
}
