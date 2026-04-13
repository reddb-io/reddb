use super::*;

mod aggregate;
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
pub(crate) use helpers::{evaluate_entity_filter, extract_entity_id_from_filter};
pub(super) use hybrid::execute_runtime_hybrid_query;
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
    // ── AGGREGATE PATH: COUNT, AVG, SUM, MIN, MAX, GROUP BY ──
    if has_aggregate_projections(&query.columns) {
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
    if query.filter.is_some()
        && query.order_by.is_empty()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
        && query.offset.is_none()
        && !is_universal_query_source(&query.table)
    {
        if let Some(entity_id) = extract_entity_id_from_filter(&query.filter) {
            let store = db.store();
            if let Some(entity) = store.get(&query.table, EntityId::new(entity_id)) {
                let json = execute_runtime_serialize_single_entity(&entity);
                let records: Vec<UnifiedRecord> = runtime_table_record_from_entity(entity)
                    .into_iter()
                    .collect();
                let columns = projected_columns(&records, &query.columns);
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

    let records = execute_runtime_canonical_table_query_indexed(db, query, index_store)?;
    let columns = projected_columns(&records, &query.columns);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(super) fn runtime_record_has_document_capability(record: &UnifiedRecord) -> bool {
    record
        .values
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
    .find_map(|field| record.values.get(field).and_then(runtime_value_number))
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
            let result =
                crate::storage::query::unified::UnifiedExecutor::execute_on_with_node_properties(
                    &graph,
                    expr,
                    node_properties,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            Ok(result.records)
        }
        QueryExpr::Vector(vector) => Ok(execute_runtime_vector_query(db, vector)?.records),
        QueryExpr::Hybrid(hybrid) => Ok(execute_runtime_hybrid_query(db, hybrid)?.records),
        other => Err(RedDBError::Query(format!(
            "canonical join execution does not yet support {} child expressions",
            query_expr_name(other)
        ))),
    }
}
