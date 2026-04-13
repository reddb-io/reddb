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

/// Extract a bloom filter key hint from a PK/ID equality filter ONLY.
///
/// Bloom filters only index entity IDs and primary keys. Using them for
/// general column values causes incorrect pruning (false negatives).
/// Restricted to: _entity_id, row_id, id, key.
pub(crate) fn extract_bloom_key_for_pk(
    filter: &crate::storage::query::ast::Filter,
) -> Option<Vec<u8>> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            // Only use bloom for PK/ID fields
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !matches!(field_name, "red_entity_id" | "row_id" | "id" | "key") {
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

/// Evaluate a SQL Filter directly against a UnifiedEntity without creating a
/// UnifiedRecord. This is the main performance optimization for filtered scans.
pub(crate) fn evaluate_entity_filter(
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
        Filter::And(left, right) => {
            evaluate_entity_filter(entity, left, table_name, table_alias)
                && evaluate_entity_filter(entity, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_entity_filter(entity, left, table_name, table_alias)
                || evaluate_entity_filter(entity, right, table_name, table_alias)
        }
        Filter::Not(inner) => !evaluate_entity_filter(entity, inner, table_name, table_alias),
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
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| like_matches(&value, pattern))
        }
        Filter::StartsWith { field, prefix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.starts_with(prefix))
        }
        Filter::EndsWith { field, suffix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.ends_with(suffix))
        }
        Filter::Contains { field, substring } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.contains(substring))
        }
    }
}
