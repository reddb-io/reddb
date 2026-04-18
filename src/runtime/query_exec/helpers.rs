//! Filter + field extraction helpers used by the table query path.
//!
//! Pure functions split out of `query_exec.rs` to reduce the main
//! executor file size. Covers:
//!
//! - filter pre-processing (`extract_equality_prefilter`,
//!   `extract_index_candidate`, `extract_bloom_key_for_pk`)
//! - projection column name extraction
//! - entity field resolution including document path walking
//!
//! Every function is visible to the parent `query_exec` module as
//! `pub(crate)` so submodules can cross-reference if needed.

use super::json_writers::timeseries_tags_json_value;
use super::*;

/// Extract the first equality condition from an AND filter for fast pre-filtering.
/// For `WHERE city = 'NYC' AND age > 30`, returns Some(("city", Value::Text("NYC"))).
/// This lets us do a direct HashMap lookup before the full filter evaluation.
pub(crate) fn extract_equality_prefilter(filter: &Filter) -> Option<(String, Value)> {
    use crate::storage::query::ast::{CompareOp, FieldRef};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return None,
            };
            // Skip system fields (they're not in named HashMap)
            if col.starts_with('_') {
                return None;
            }
            Some((col, value.clone()))
        }
        Filter::And(left, right) => {
            extract_equality_prefilter(left).or_else(|| extract_equality_prefilter(right))
        }
        _ => None,
    }
}

/// Extract entity_id from `WHERE _entity_id = N` for O(1) direct lookup.
pub(crate) fn extract_entity_id_from_filter(
    filter: &Option<crate::storage::query::ast::Filter>,
) -> Option<u64> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    let filter = filter.as_ref()?;
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if field_name != "red_entity_id" && field_name != "entity_id" {
                return None;
            }
            match value {
                Value::Integer(n) => Some(*n as u64),
                Value::UnsignedInteger(n) => Some(*n),
                _ => None,
            }
        }
        Filter::And(left, right) => extract_entity_id_from_filter(&Some(*left.clone()))
            .or_else(|| extract_entity_id_from_filter(&Some(*right.clone()))),
        _ => None,
    }
}

/// Extract a bloom filter key hint from an internal-PK equality filter.
///
/// The segment-level bloom only indexes RedDB's synthetic
/// `red_entity_id` column — every entity is hashed into the bloom by
/// that key on insert. User-declared columns named `id`, `row_id`, or
/// `key` are NOT guaranteed to be in the bloom (they're application
/// data, not engine-managed PKs); treating them as bloom-keyed
/// produced false-negative pruning that silently dropped every row
/// matching `WHERE id = N` against tables whose `id` column was just
/// a regular user field.
///
/// Restricted to `red_entity_id` so the bloom probe is always sound.
/// User-PK pruning belongs in a separate code path tied to actual
/// PRIMARY KEY metadata or registered index hints.
pub(crate) fn extract_bloom_key_for_pk(
    filter: &crate::storage::query::ast::Filter,
) -> Option<Vec<u8>> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if field_name != "red_entity_id" {
                return None;
            }
            let key = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            Some(key)
        }
        Filter::And(left, right) => {
            extract_bloom_key_for_pk(left).or_else(|| extract_bloom_key_for_pk(right))
        }
        _ => None,
    }
}

/// Extract a (column_name, value_bytes) from a simple equality filter for index lookup.
pub(crate) fn extract_index_candidate(
    filter: &crate::storage::query::ast::Filter,
) -> Option<(String, Vec<u8>)> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let column = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return None,
            };
            let bytes = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            Some((column, bytes))
        }
        Filter::And(left, right) => {
            extract_index_candidate(left).or_else(|| extract_index_candidate(right))
        }
        _ => None,
    }
}

/// Extract ALL equality predicates from an AND-tree, one per indexed column.
/// Used by the TID bitmap path to AND multiple hash index lookups.
/// The triple is `(column_name, index_bytes, original_Value)`.
/// `original_Value` lets callers build covered-query records without decoding bytes.
/// Stops at OR / NOT — not AND-combinable.
pub(crate) fn extract_all_eq_candidates(
    filter: &crate::storage::query::ast::Filter,
    out: &mut Vec<(String, Vec<u8>, crate::storage::schema::Value)>,
) {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare {
            field,
            op: CompareOp::Eq,
            value,
        } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return,
            };
            let bytes = match value {
                crate::storage::schema::Value::Text(s) => s.as_bytes().to_vec(),
                crate::storage::schema::Value::Integer(n) => n.to_le_bytes().to_vec(),
                crate::storage::schema::Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return,
            };
            out.push((col, bytes, value.clone()));
        }
        Filter::And(left, right) => {
            extract_all_eq_candidates(left, out);
            extract_all_eq_candidates(right, out);
        }
        _ => {}
    }
}

/// Extract range/equality predicates for zone-map segment pruning.
///
/// Walks a filter tree and collects `(column, ZoneColPred)` pairs for
/// simple comparisons on named user columns (not system fields).  The
/// caller passes the returned slice to `SegmentManager::for_each_entity_zoned`.
///
/// Returns owned `(String, Value)` pairs because the caller needs them to
/// outlive the filter borrow; `ZoneColPred` is reconstructed from refs at
/// the call site.
pub(crate) fn extract_zone_predicates(
    filter: &crate::storage::query::ast::Filter,
    out: &mut Vec<(
        String,
        crate::storage::schema::Value,
        crate::storage::unified::segment::ZoneColPredKind,
    )>,
) {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    use crate::storage::unified::segment::ZoneColPredKind;
    match filter {
        Filter::Compare { field, op, value } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return,
            };
            // Skip system fields — they live outside named HashMap
            if col.starts_with('_') || col == "red_entity_id" || col == "entity_id" {
                return;
            }
            let kind = match op {
                CompareOp::Eq => ZoneColPredKind::Eq,
                CompareOp::Gt => ZoneColPredKind::Gt,
                CompareOp::Ge => ZoneColPredKind::Gte,
                CompareOp::Lt => ZoneColPredKind::Lt,
                CompareOp::Le => ZoneColPredKind::Lte,
                _ => return,
            };
            out.push((col.to_string(), value.clone(), kind));
        }
        Filter::Between { field, low, high } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return,
            };
            if col.starts_with('_') || col == "red_entity_id" || col == "entity_id" {
                return;
            }
            out.push((col.to_string(), low.clone(), ZoneColPredKind::Gte));
            out.push((col.to_string(), high.clone(), ZoneColPredKind::Lte));
        }
        Filter::And(left, right) => {
            extract_zone_predicates(left, out);
            extract_zone_predicates(right, out);
        }
        // OR / NOT: can't prune — skip
        _ => {}
    }
}

/// Extract simple column names from SELECT projections for projection pushdown.
/// Returns empty Vec for SELECT * or when projections contain expressions/functions.
pub(crate) fn extract_select_column_names(projections: &[Projection]) -> Vec<String> {
    if projections.is_empty() || projections.iter().any(|p| matches!(p, Projection::All)) {
        return Vec::new();
    }
    projections
        .iter()
        .filter_map(|p| match p {
            Projection::Column(c) | Projection::Alias(c, _) => Some(c.clone()),
            Projection::Field(FieldRef::TableColumn { column: c, .. }, _) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Entity-level filter evaluation
// ─────────────────────────────────────────────────────────────────────────────
// These functions evaluate SQL WHERE clauses directly against raw UnifiedEntity
// data, avoiding the expensive intermediate step of creating a UnifiedRecord
// (which allocates a HashMap and copies ~10 system fields + all user fields).
//
// For a 5000-row table with a filter matching ~100 rows, this avoids creating
// ~4900 throwaway UnifiedRecords.
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve a field reference directly from an entity, without creating a UnifiedRecord.
/// Returns a borrowed Value when possible, or an owned Value for computed fields.
pub(crate) fn resolve_entity_field<'a>(
    entity: &'a crate::storage::unified::entity::UnifiedEntity,
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
) -> Option<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;

    let (column, document_path) = match field {
        FieldRef::TableColumn { table, column } => {
            // If table qualifier is present, verify it matches
            if !table.is_empty()
                && !runtime_table_context_matches(
                    table.as_str(),
                    Some(table_name),
                    Some(table_alias),
                )
            {
                return resolve_entity_document_path(entity, &format!("{table}.{column}"))
                    .map(Cow::Owned);
            }
            let document_path = column.contains('.').then_some(column.as_str());
            (column.as_str(), document_path)
        }
        _ => return None,
    };

    // System fields — accessed directly from entity struct fields
    match column {
        "red_entity_id" | "entity_id" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.id.raw())));
        }
        "created_at" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.created_at)));
        }
        "updated_at" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.updated_at)));
        }
        "red_sequence_id" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.sequence_id)));
        }
        "red_collection" => {
            return Some(Cow::Owned(Value::Text(
                entity.kind.collection().to_string(),
            )));
        }
        "red_kind" => {
            return Some(Cow::Owned(Value::Text(
                entity.kind.storage_type().to_string(),
            )));
        }
        "row_id" => {
            if let crate::storage::unified::entity::EntityKind::TableRow { row_id, .. } =
                &entity.kind
            {
                return Some(Cow::Owned(Value::UnsignedInteger(*row_id)));
            }
            return None;
        }
        _ => {}
    }

    // User fields — row data (named HashMap or columnar schema)
    if let Some(row) = entity.data.as_row() {
        if let Some(value) = row.get_field(column) {
            return Some(Cow::Borrowed(value));
        }
        // Positional column fallback (c0, c1, ...)
        if let Some(index) = column
            .strip_prefix('c')
            .and_then(|index| index.parse::<usize>().ok())
        {
            if let Some(value) = row.columns.get(index) {
                return Some(Cow::Borrowed(value));
            }
        }
    }

    // Node properties
    if let EntityData::Node(ref node) = entity.data {
        if let Some(value) = node.properties.get(column) {
            return Some(Cow::Borrowed(value));
        }
    }

    // Edge properties
    if let EntityData::Edge(ref edge) = entity.data {
        if column == "weight" {
            return Some(Cow::Owned(Value::Float(edge.weight as f64)));
        }
        if let Some(value) = edge.properties.get(column) {
            return Some(Cow::Borrowed(value));
        }
    }

    if let EntityData::TimeSeries(ref ts) = entity.data {
        match column {
            "metric" => return Some(Cow::Owned(Value::Text(ts.metric.clone()))),
            "timestamp_ns" => return Some(Cow::Owned(Value::UnsignedInteger(ts.timestamp_ns))),
            "timestamp" | "time" => {
                return Some(Cow::Owned(Value::UnsignedInteger(ts.timestamp_ns)));
            }
            "value" => return Some(Cow::Owned(Value::Float(ts.value))),
            "tags" => {
                return Some(Cow::Owned(timeseries_tags_json_value(&ts.tags)));
            }
            _ => {}
        }
    }

    if let Some(path) = document_path {
        if let Some(value) = resolve_entity_document_path(entity, path) {
            return Some(Cow::Owned(value));
        }
    }

    // EntityKind fields (label, node_type, from_node, to_node)
    match &entity.kind {
        EntityKind::GraphNode(ref gn) => match column {
            "label" => return Some(Cow::Owned(Value::Text(gn.label.to_string()))),
            "node_type" => return Some(Cow::Owned(Value::Text(gn.node_type.to_string()))),
            _ => {}
        },
        EntityKind::GraphEdge(ref ge) => match column {
            "label" => return Some(Cow::Owned(Value::Text(ge.label.to_string()))),
            "from_node" => return Some(Cow::Owned(Value::Text(ge.from_node.to_string()))),
            "to_node" => return Some(Cow::Owned(Value::Text(ge.to_node.to_string()))),
            _ => {}
        },
        _ => {}
    }

    None
}

pub(crate) fn resolve_entity_document_path(
    entity: &crate::storage::unified::entity::UnifiedEntity,
    path: &str,
) -> Option<Value> {
    let segments = parse_runtime_document_path(path);
    let (root, tail) = segments.split_first()?;

    if let Some(row) = entity.data.as_row() {
        if let Some(value) = row.get_field(root) {
            if tail.is_empty() {
                return Some(value.clone());
            }
            return resolve_runtime_document_path_from_value(value, tail);
        }
    }

    if let EntityData::Node(ref node) = entity.data {
        if let Some(value) = node.properties.get(root) {
            if tail.is_empty() {
                return Some(value.clone());
            }
            return resolve_runtime_document_path_from_value(value, tail);
        }
    }

    if let EntityData::Edge(ref edge) = entity.data {
        if let Some(value) = edge.properties.get(root) {
            if tail.is_empty() {
                return Some(value.clone());
            }
            return resolve_runtime_document_path_from_value(value, tail);
        }
    }

    if let EntityData::TimeSeries(ref ts) = entity.data {
        let root_value = match root.as_str() {
            "tags" => Some(timeseries_tags_json_value(&ts.tags)),
            _ => None,
        }?;
        if tail.is_empty() {
            return Some(root_value);
        }
        return resolve_runtime_document_path_from_value(&root_value, tail);
    }

    None
}

/// Try to resolve a simple equality filter (`WHERE col = val`) via hash index.
///
/// Returns `Some(entity_ids)` when:
/// - The filter contains a simple `col = val` equality predicate
/// - A hash index exists for that column on the given table
/// - The lookup succeeds
///
/// Returns `None` when any condition isn't met — caller falls back to full scan.
/// Only extracts the first equality predicate; compound filters with extra
/// predicates still need post-fetch filtering.
pub(crate) fn try_hash_eq_lookup(
    filter: &crate::storage::query::ast::Filter,
    table: &str,
    idx_store: &super::super::index_store::IndexStore,
) -> Option<Vec<crate::storage::unified::entity::EntityId>> {
    use crate::storage::query::ast::{FieldRef, Filter};

    // `WHERE col IN (v1, v2, ...)` — one hash lookup per value, union the
    // id sets. Cheap per-value because each lookup is O(1) in the HashMap
    // bucket. Without this path `WHERE id IN (...)` falls through to a
    // full scan, which is catastrophic for bulk_update batches.
    if let Filter::In { field, values } = filter {
        let col = match field {
            FieldRef::TableColumn { column, .. } => column.as_str(),
            _ => return None,
        };
        let idx = idx_store.find_index_for_column(table, col)?;
        let mut ids = Vec::new();
        for value in values {
            let bytes = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            let lookup = idx_store.hash_lookup(table, &idx.name, &bytes).ok()?;
            ids.extend(lookup);
        }
        return Some(ids);
    }

    let (col, val_bytes) = extract_index_candidate(filter)?;
    let idx = idx_store.find_index_for_column(table, &col)?;
    idx_store.hash_lookup(table, &idx.name, &val_bytes).ok()
}

/// Evaluate a SQL Filter directly against a UnifiedEntity without creating a
/// UnifiedRecord. This is the main performance optimization for filtered scans.
pub(crate) fn evaluate_entity_filter(
    entity: &crate::storage::unified::entity::UnifiedEntity,
    filter: &Filter,
    table_name: &str,
    table_alias: &str,
) -> bool {
    evaluate_entity_filter_with_db(None, entity, filter, table_name, table_alias)
}

pub(crate) fn evaluate_entity_filter_with_db(
    db: Option<&RedDB>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
    filter: &Filter,
    table_name: &str,
    table_alias: &str,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .map(|candidate| compare_runtime_values(candidate.as_ref(), value, *op))
                .unwrap_or(false)
        }
        Filter::CompareFields { left, op, right } => {
            let left_val = resolve_entity_field(entity, left, table_name, table_alias);
            let right_val = resolve_entity_field(entity, right, table_name, table_alias);
            match (left_val, right_val) {
                (Some(l), Some(r)) => compare_runtime_values(l.as_ref(), r.as_ref(), *op),
                _ => false,
            }
        }
        Filter::CompareExpr { .. } => {
            // Entity-level evaluator can only resolve FieldRef-based
            // operands. For expression-shaped predicates we return
            // `true` so the row is NOT pre-filtered — downstream
            // per-record evaluation (which has full `UnifiedRecord`
            // context and the Expr walker) applies the real predicate.
            // Correctness is preserved; selectivity at the scan layer
            // is lost. The planner's `filter_compiled` layer also
            // routes this variant through its `Fallback` opcode.
            //
            // Phase 2.5.5 RLS universal: non-TableRow entities (graph
            // nodes, vectors, queue messages, timeseries points) need
            // the `any` record builder so policy predicates can reach
            // their native fields (node.properties, edge props,
            // message.payload, vector.metadata, timeseries.tags).
            let Some(db) = db else { return true };
            let table_record = runtime_table_record_from_entity(entity.clone());
            let record = match table_record {
                Some(r) => Some(r),
                None => super::super::record_search_helpers::any_record_from_entity(
                    entity.clone(),
                ),
            };
            let Some(record) = record else {
                return false;
            };
            super::super::join_filter::evaluate_runtime_filter_with_db(
                Some(db),
                &record,
                filter,
                Some(table_name),
                Some(table_alias),
            )
        }
        Filter::And(left, right) => {
            evaluate_entity_filter_with_db(db, entity, left, table_name, table_alias)
                && evaluate_entity_filter_with_db(db, entity, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_entity_filter_with_db(db, entity, left, table_name, table_alias)
                || evaluate_entity_filter_with_db(db, entity, right, table_name, table_alias)
        }
        Filter::Not(inner) => {
            !evaluate_entity_filter_with_db(db, entity, inner, table_name, table_alias)
        }
        Filter::IsNull(field) => resolve_entity_field(entity, field, table_name, table_alias)
            .map(|value| value.as_ref() == &Value::Null)
            .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_entity_field(entity, field, table_name, table_alias)
            .map(|value| value.as_ref() != &Value::Null)
            .unwrap_or(false),
        Filter::In { field, values } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    values.iter().any(|value| {
                        compare_runtime_values(candidate.as_ref(), value, CompareOp::Eq)
                    })
                })
        }
        Filter::Between { field, low, high } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    compare_runtime_values(candidate.as_ref(), low, CompareOp::Ge)
                        && compare_runtime_values(candidate.as_ref(), high, CompareOp::Le)
                })
        }
        Filter::Like { field, pattern } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text_cow(v.as_ref()))
                .is_some_and(|value| like_matches(value.as_ref(), pattern))
        }
        Filter::StartsWith { field, prefix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text_cow(v.as_ref()))
                .is_some_and(|value| value.starts_with(prefix.as_str()))
        }
        Filter::EndsWith { field, suffix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text_cow(v.as_ref()))
                .is_some_and(|value| value.ends_with(suffix.as_str()))
        }
        Filter::Contains { field, substring } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text_cow(v.as_ref()))
                .is_some_and(|value| value.contains(substring.as_str()))
        }
    }
}
