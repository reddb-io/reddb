//! DML RETURNING / patch-operation helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1633). Names and behaviour are unchanged
//! from `impl_dml`; the only adjustment is `pub(super)` visibility so the
//! sibling `impl_dml` module can keep calling these helpers by their bare
//! names.

use super::impl_dml::{CompiledUpdatePlan, MaterializedUpdateAssignments};
use super::impl_dml_support::*;
use super::record_search::runtime_any_record_from_entity_ref;
use super::*;
use crate::application::entity::{
    CreateEntityOutput, PatchEntityOperation, PatchEntityOperationType,
};
use crate::application::ports::entity_row_fields_snapshot;
use crate::presentation::entity_json::storage_value_to_json;
use crate::storage::query::ast::ReturningItem;
use crate::storage::query::unified::{
    sys_key_collection, sys_key_created_at, sys_key_kind, sys_key_rid, sys_key_tenant,
    sys_key_updated_at,
};

pub(super) fn build_patch_operations_from_materialized_assignments(
    entity: &UnifiedEntity,
    compiled_plan: &CompiledUpdatePlan,
    assignments: MaterializedUpdateAssignments,
) -> Vec<PatchEntityOperation> {
    let mut operations = Vec::with_capacity(
        compiled_plan.static_field_assignments.len()
            + compiled_plan.static_metadata_assignments.len()
            + assignments.dynamic_field_assignments.len()
            + assignments.dynamic_metadata_assignments.len(),
    );

    for (column, value) in &compiled_plan.static_field_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: update_patch_path_for_entity(entity, column),
            value: Some(storage_value_to_json(value)),
        });
    }

    for (column, value) in assignments.dynamic_field_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: update_patch_path_for_entity(entity, &column),
            value: Some(storage_value_to_json(&value)),
        });
    }

    for (key, value) in &compiled_plan.static_metadata_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["metadata".to_string(), key.clone()],
            value: Some(metadata_value_to_json(value)),
        });
    }

    for (key, value) in assignments.dynamic_metadata_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["metadata".to_string(), key],
            value: Some(metadata_value_to_json(&value)),
        });
    }

    operations
}

pub(super) fn update_patch_path_for_entity(entity: &UnifiedEntity, column: &str) -> Vec<String> {
    if matches!(
        (&entity.kind, &entity.data),
        (
            crate::storage::EntityKind::GraphNode(_),
            EntityData::Node(_)
        )
    ) && column.eq_ignore_ascii_case("node_type")
    {
        return vec!["node_type".to_string()];
    }
    if matches!(
        (&entity.kind, &entity.data),
        (
            crate::storage::EntityKind::GraphEdge(_),
            EntityData::Edge(_)
        )
    ) && column.eq_ignore_ascii_case("weight")
    {
        return vec!["weight".to_string()];
    }
    vec!["fields".to_string(), column.to_string()]
}

/// Rewrite `DELETE FROM <table> [WHERE …] [RETURNING …]` as
/// `SELECT * FROM <table> [WHERE …]` so the delete executor can
/// capture the pre-image before actually removing the rows. Returns
/// `None` when the input does not start with `DELETE`.
///
/// Case-insensitive on the keywords. Preserves everything between
/// the table name and the RETURNING clause, so WHERE / ORDER BY /
/// LIMIT survive untouched. The RETURNING tail — if present — is
/// truncated at the first top-level `RETURNING` token.
pub(super) fn delete_to_select_sql(sql: &str) -> Option<String> {
    let trimmed = sql.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    if !lowered.starts_with("delete ") && !lowered.starts_with("delete\t") {
        return None;
    }
    // Find `FROM` after DELETE.
    let from_idx = lowered.find(" from ")?;
    let after_from = &trimmed[from_idx + " from ".len()..];
    let after_from_lc = &lowered[from_idx + " from ".len()..];

    // Cut off the RETURNING tail (a naive search — the RETURNING
    // clause only appears once per statement at top level in our
    // grammar). Matches whitespace-bounded tokens to avoid clipping
    // `RETURNING` inside a string literal.
    let mut body = after_from.to_string();
    if let Some(pos) = find_top_level_keyword(after_from_lc, "returning") {
        body.truncate(pos);
    }
    Some(format!("SELECT * FROM {}", body.trim_end()))
}

/// Find the byte offset of a whitespace-bounded keyword in a
/// lowercased haystack, skipping matches inside single-quoted
/// string literals. Naive — no escape handling — but enough for
/// the shapes the DML parser emits.
pub(super) fn find_top_level_keyword(haystack: &str, needle: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut i = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string
            && i + nlen <= bytes.len()
            && &bytes[i..i + nlen] == needle.as_bytes()
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
            && (i + nlen == bytes.len() || bytes[i + nlen].is_ascii_whitespace())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Build a `UnifiedResult` from the rows affected by a DML statement plus
/// its `RETURNING` clause. Each snapshot is a list of (column, value) pairs
/// for one affected row; `outputs`, when provided, supplies the engine-
/// assigned entity id for the same row (INSERT path). Projection honours
/// the RETURNING items: `*` expands to every snapshot column plus
/// the public row envelope when available.
pub(super) fn build_returning_result(
    items: &[ReturningItem],
    snapshots: &[Vec<(String, Value)>],
    outputs: Option<&[CreateEntityOutput]>,
) -> UnifiedResult {
    let project_all = items.iter().any(|it| matches!(it, ReturningItem::All));
    let public_item_outputs = outputs.is_some_and(|outs| {
        outs.first()
            .and_then(|out| out.entity.as_ref())
            .is_some_and(|entity| public_returning_item_kind(entity).is_some())
    });

    let mut columns: Vec<String> = if project_all {
        let mut cols: Vec<String> = Vec::new();
        if public_item_outputs {
            cols.extend(
                [
                    "rid",
                    "collection",
                    "kind",
                    "tenant",
                    "created_at",
                    "updated_at",
                ]
                .into_iter()
                .map(str::to_string),
            );
        } else if outputs.is_some() {
            cols.push("rid".to_string());
        }
        if let Some(first) = snapshots.first() {
            for (name, _) in first {
                cols.push(name.clone());
            }
        }
        cols
    } else {
        items
            .iter()
            .filter_map(|it| match it {
                ReturningItem::Column(c) => Some(c.clone()),
                ReturningItem::All => None,
            })
            .collect()
    };
    // Guarantee unique order-preserving column list.
    {
        let mut seen = std::collections::HashSet::new();
        columns.retain(|c| seen.insert(c.clone()));
    }

    let mut records: Vec<UnifiedRecord> = Vec::with_capacity(snapshots.len());
    for (idx, snap) in snapshots.iter().enumerate() {
        let mut values: HashMap<Arc<str>, Value> = HashMap::with_capacity(columns.len());
        if let Some(outs) = outputs {
            if let Some(out) = outs.get(idx) {
                if let Some(entity) = out.entity.as_ref() {
                    if let Some(kind) = public_returning_item_kind(entity) {
                        values.insert(
                            Arc::clone(&sys_key_rid()),
                            Value::UnsignedInteger(out.id.raw()),
                        );
                        values.insert(
                            Arc::clone(&sys_key_collection()),
                            Value::text(entity.kind.collection().to_string()),
                        );
                        values.insert(Arc::clone(&sys_key_kind()), Value::text(kind.to_string()));
                        values.insert(
                            Arc::clone(&sys_key_created_at()),
                            Value::UnsignedInteger(entity.created_at),
                        );
                        values.insert(
                            Arc::clone(&sys_key_updated_at()),
                            Value::UnsignedInteger(entity.updated_at),
                        );
                    } else {
                        values.insert(
                            Arc::clone(&sys_key_rid()),
                            Value::Integer(out.id.raw() as i64),
                        );
                    }
                } else {
                    values.insert(
                        Arc::clone(&sys_key_rid()),
                        Value::Integer(out.id.raw() as i64),
                    );
                }
            }
        }
        for (name, val) in snap {
            values.insert(Arc::from(name.as_str()), val.clone());
        }
        if !values.contains_key("tenant") {
            let tenant = values.get("tenant_id").cloned().unwrap_or(Value::Null);
            values.insert(Arc::clone(&sys_key_tenant()), tenant);
        }
        let mut rec = UnifiedRecord::default();
        // Only keep projected columns on the record.
        for col in &columns {
            if let Some(v) = values.get(col.as_str()) {
                rec.set_arc(Arc::from(col.as_str()), v.clone());
            }
        }
        records.push(rec);
    }

    UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    }
}

pub(super) fn public_returning_item_kind(
    entity: &crate::storage::UnifiedEntity,
) -> Option<&'static str> {
    match (&entity.kind, &entity.data) {
        (crate::storage::EntityKind::GraphNode(_), crate::storage::EntityData::Node(_)) => {
            Some("node")
        }
        (crate::storage::EntityKind::GraphEdge(_), crate::storage::EntityData::Edge(_)) => {
            Some("edge")
        }
        (_, crate::storage::EntityData::Row(_)) => Some(public_returning_row_kind(entity)),
        // #1369 — every entity model must expose its `rid` in RETURNING *.
        // Vectors carry their payload in `EntityData::Vector`, not `Row`, so
        // they were falling through to a no-rid envelope
        // and `RETURNING *` never surfaced the entity-id.
        (_, crate::storage::EntityData::Vector(_)) => Some("vector"),
        _ => None,
    }
}

pub(super) fn public_returning_row_kind(entity: &crate::storage::UnifiedEntity) -> &'static str {
    let Some(row) = entity.data.as_row() else {
        return "row";
    };

    let is_kv = row.named.as_ref().is_some_and(|named| {
        (named.len() == 2 && named.contains_key("key") && named.contains_key("value"))
            || (named.len() == 1 && (named.contains_key("key") || named.contains_key("value")))
    });
    if is_kv {
        return "kv";
    }

    let is_document = row
        .named
        .as_ref()
        .is_some_and(|named| named.values().any(runtime_returning_documentish_value))
        || row.columns.iter().any(runtime_returning_documentish_value);
    if is_document {
        "document"
    } else {
        "row"
    }
}

pub(super) fn runtime_returning_documentish_value(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

pub(super) fn row_insert_returning_snapshots(
    outputs: &[CreateEntityOutput],
    fallback: Vec<Vec<(String, Value)>>,
) -> Vec<Vec<(String, Value)>> {
    outputs
        .iter()
        .enumerate()
        .map(|(idx, out)| {
            out.entity
                .as_ref()
                .map(entity_row_fields_snapshot)
                .filter(|snap| !snap.is_empty())
                .unwrap_or_else(|| fallback.get(idx).cloned().unwrap_or_default())
        })
        .collect()
}

pub(super) fn graph_insert_returning_snapshots(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
    ids: &[EntityId],
) -> Vec<Vec<(String, Value)>> {
    let Some(manager) = store.get_collection(collection) else {
        return Vec::new();
    };

    ids.iter()
        .filter_map(|id| manager.get(*id))
        .filter_map(|entity| {
            let mut record = runtime_any_record_from_entity_ref(&entity)?;
            record.set_arc(sys_key_collection(), Value::text(collection.to_string()));
            Some(record)
        })
        .map(|record| {
            record
                .iter_fields()
                .map(|(key, value)| (key.as_ref().to_string(), value.clone()))
                .collect()
        })
        .collect()
}

pub(super) fn graph_update_returning_snapshots(
    runtime: &RedDBRuntime,
    collection: &str,
    ids: &[EntityId],
) -> Vec<Vec<(String, Value)>> {
    let store = runtime.db().store();
    let Some(manager) = store.get_collection(collection) else {
        return Vec::new();
    };

    manager
        .get_many(ids)
        .into_iter()
        .flatten()
        .filter_map(|entity| runtime_any_record_from_entity_ref(&entity))
        .map(|record| {
            record
                .iter_fields()
                .map(|(key, value)| (key.as_ref().to_string(), value.clone()))
                .collect()
        })
        .collect()
}

pub(super) fn restore_kv_returning_keys(
    runtime: &RedDBRuntime,
    collection: &str,
    ids: &[EntityId],
    snapshots: &mut [Vec<(String, Value)>],
) {
    if ids.is_empty() || snapshots.is_empty() {
        return;
    }

    let store = runtime.db().store();
    for (idx, id) in ids.iter().enumerate() {
        let Some(snapshot) = snapshots.get_mut(idx) else {
            continue;
        };
        if snapshot.iter().any(|(name, _)| name == "key") {
            continue;
        }
        let Some(entity) = store.get(collection, *id) else {
            continue;
        };
        let logical_id = entity.logical_id();
        let key = store
            .table_row_versions_by_logical_id(collection, logical_id)
            .into_iter()
            .find_map(kv_key_value_from_entity)
            .or_else(|| kv_key_value_from_entity(entity));
        if let Some(key) = key {
            snapshot.push(("key".to_string(), key));
        }
    }
}

pub(super) fn kv_key_value_from_entity(entity: UnifiedEntity) -> Option<Value> {
    let row = entity.data.as_row()?;
    row.get_field("key")
        .cloned()
        .or_else(|| row.columns.first().cloned())
}
