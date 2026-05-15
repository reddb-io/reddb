//! DML target scan: locate the entity ids a DML statement should
//! mutate.
//!
//! Lifts the "find the rows that match WHERE" loop out of the inline
//! DELETE path into a small testable module. The same Interface will
//! be reused by UPDATE in a follow-up; this commit covers DELETE only.
//!
//! Behaviour preservation
//! ----------------------
//! The scan is a faithful refactor of the previous inline logic in
//! `execute_delete_inner`. In particular, the **visibility filtering**
//! semantics are preserved exactly:
//!
//! * `_entity_id = N` fast path: no visibility check (the row was
//!   named directly).
//! * Hash / sorted index fast paths: no visibility check (the index
//!   only ever points at rows that should be reachable).
//! * Zoned scan and full-table scan: visibility is enforced through
//!   `entity_visible_under_current_snapshot` so AS OF / RLS continue
//!   to work as before.
//!
//! The Interface is intentionally tiny: a constructor plus
//! `find_target_ids`. UPDATE (#52) will consume the same shape.

use std::collections::HashMap;

use super::{query_exec, RedDBRuntime};
use crate::api::{RedDBError, RedDBResult};
use crate::storage::query::ast::{Filter, UpdateTarget};
use crate::storage::schema::Value;
use crate::storage::{EntityData, EntityId};

pub(super) struct DmlTargetScan<'a> {
    runtime: &'a RedDBRuntime,
    table: &'a str,
    filter: Option<&'a Filter>,
    limit: Option<usize>,
    target: Option<UpdateTarget>,
}

impl<'a> DmlTargetScan<'a> {
    pub(super) fn new(
        runtime: &'a RedDBRuntime,
        table: &'a str,
        filter: Option<&'a Filter>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            runtime,
            table,
            filter,
            limit,
            target: None,
        }
    }

    pub(super) fn with_update_target(
        runtime: &'a RedDBRuntime,
        table: &'a str,
        filter: Option<&'a Filter>,
        limit: Option<usize>,
        target: UpdateTarget,
    ) -> Self {
        Self {
            runtime,
            table,
            filter,
            limit,
            target: Some(target),
        }
    }

    /// Walk the source collection, apply the (compiled) WHERE clause,
    /// and yield the entity ids that match.
    ///
    /// Returns `Err(NotFound)` when the table does not exist —
    /// callers can treat this the same way the inline DELETE path
    /// did.
    pub(super) fn find_target_ids(&self) -> RedDBResult<Vec<EntityId>> {
        let db = self.runtime.db();
        let store = db.store();
        let manager = store
            .get_collection(self.table)
            .ok_or_else(|| RedDBError::NotFound(self.table.to_string()))?;
        let compiled_filter = self.compiled_filter();

        // Fast path: WHERE _entity_id = N. Mirrors the inline DELETE
        // behaviour, which skips snapshot visibility filtering on this
        // direct-lookup path (the row was named explicitly).
        if let Some(entity_id) = query_exec::extract_entity_id_from_filter(&self.filter.cloned()) {
            if let Some(entity) =
                store.get_table_row_by_logical_id(self.table, EntityId::new(entity_id))
            {
                return Ok(self.target_ids_from_entities([entity]));
            }
            if let Some(entity) = store.get(self.table, EntityId::new(entity_id)) {
                return Ok(self.target_ids_from_entities([entity]));
            }
            return Ok(Vec::new());
        }

        if let Some(filter) = self.filter {
            if let Some(ids) =
                query_exec::try_hash_eq_lookup(filter, self.table, self.runtime.index_store_ref())
            {
                return Ok(self.recheck_index_candidates(ids, compiled_filter.as_ref()));
            }

            if let Some(ids) = query_exec::try_sorted_index_lookup(
                filter,
                self.table,
                self.runtime.index_store_ref(),
                None,
            ) {
                return Ok(self.recheck_index_candidates(ids, compiled_filter.as_ref()));
            }
        }

        let mut ids = Vec::new();
        if let Some(filter) = self.filter {
            let mut owned_zone_preds = Vec::new();
            query_exec::extract_zone_predicates(filter, &mut owned_zone_preds);
            let zone_preds: Vec<_> = owned_zone_preds
                .iter()
                .map(|(column, value, kind)| {
                    (
                        column.as_str(),
                        match kind {
                            crate::storage::unified::segment::ZoneColPredKind::Eq => {
                                crate::storage::unified::segment::ZoneColPred::Eq(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gt => {
                                crate::storage::unified::segment::ZoneColPred::Gt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gte => {
                                crate::storage::unified::segment::ZoneColPred::Gte(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lt => {
                                crate::storage::unified::segment::ZoneColPred::Lt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lte => {
                                crate::storage::unified::segment::ZoneColPred::Lte(value)
                            }
                        },
                    )
                })
                .collect();

            manager.for_each_entity_zoned(&zone_preds, |entity| {
                if !crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
                    return true;
                }
                if self.matches_update_target(entity)
                    && self.matches_filter(entity, compiled_filter.as_ref())
                {
                    ids.push(entity.id);
                    if self.limit.map(|limit| ids.len() >= limit).unwrap_or(false) {
                        return false;
                    }
                }
                true
            });
        } else {
            manager.for_each_entity(|entity| {
                if crate::runtime::impl_core::entity_visible_under_current_snapshot(entity)
                    && self.matches_update_target(entity)
                {
                    ids.push(entity.id);
                    if self.limit.map(|limit| ids.len() >= limit).unwrap_or(false) {
                        return false;
                    }
                }
                true
            });
        }

        Ok(ids)
    }

    pub(super) fn row_snapshots(&self, entity_ids: &[EntityId]) -> Vec<Vec<(String, Value)>> {
        if entity_ids.is_empty() {
            return Vec::new();
        }
        let db = self.runtime.db();
        let store = db.store();
        let Some(manager) = store.get_collection(self.table) else {
            return Vec::new();
        };
        manager
            .get_many(entity_ids)
            .into_iter()
            .flatten()
            .filter_map(|entity| entity_row_snapshot(&entity))
            .collect()
    }

    pub(super) fn row_json_pre_images(
        &self,
        entity_ids: &[EntityId],
    ) -> HashMap<u64, crate::json::Value> {
        if entity_ids.is_empty() {
            return HashMap::new();
        }
        let db = self.runtime.db();
        let store = db.store();
        entity_ids
            .iter()
            .filter_map(|&id| {
                store
                    .get(self.table, id)
                    .map(|entity| (id.raw(), crate::runtime::mutation::entity_row_json(&entity)))
            })
            .collect()
    }

    fn compiled_filter(&self) -> Option<query_exec::CompiledEntityFilter> {
        let filter = self.filter?;
        let db = self.runtime.db();
        let store = db.store();
        let manager = store.get_collection(self.table)?;
        let compiled = match manager.column_schema() {
            Some(schema) => query_exec::CompiledEntityFilter::compile_with_schema(
                filter, self.table, self.table, &schema,
            ),
            None => query_exec::CompiledEntityFilter::compile(filter, self.table, self.table),
        };
        (!compiled.has_fallback()).then_some(compiled)
    }

    fn recheck_index_candidates(
        &self,
        entity_ids: Vec<EntityId>,
        compiled_filter: Option<&query_exec::CompiledEntityFilter>,
    ) -> Vec<EntityId> {
        let db = self.runtime.db();
        let store = db.store();
        let Some(manager) = store.get_collection(self.table) else {
            return Vec::new();
        };

        // Note: the inline DELETE path did not apply
        // `entity_visible_under_current_snapshot` to candidates that
        // came from an index lookup, so we don't either. Preserves
        // existing AS-OF semantics on indexed predicates.
        let mut ids = Vec::with_capacity(entity_ids.len());
        for entity in manager.get_many(&entity_ids).into_iter().flatten() {
            if self.matches_update_target(&entity) && self.matches_filter(&entity, compiled_filter)
            {
                ids.push(entity.id);
                if self.limit.map(|limit| ids.len() >= limit).unwrap_or(false) {
                    break;
                }
            }
        }
        ids
    }

    fn target_ids_from_entities<I>(&self, entities: I) -> Vec<EntityId>
    where
        I: IntoIterator<Item = crate::storage::UnifiedEntity>,
    {
        entities
            .into_iter()
            .filter(|entity| self.matches_update_target(entity))
            .map(|entity| entity.id)
            .collect()
    }

    fn matches_update_target(&self, entity: &crate::storage::UnifiedEntity) -> bool {
        let Some(target) = self.target else {
            return true;
        };
        match target {
            UpdateTarget::Rows => matches!(row_item_kind(entity), Some(RowItemKind::Row)),
            UpdateTarget::Documents => matches!(row_item_kind(entity), Some(RowItemKind::Document)),
            UpdateTarget::Kv => matches!(row_item_kind(entity), Some(RowItemKind::Kv)),
            UpdateTarget::Nodes => {
                matches!(
                    (&entity.kind, &entity.data),
                    (
                        crate::storage::EntityKind::GraphNode(_),
                        crate::storage::EntityData::Node(_)
                    )
                )
            }
            UpdateTarget::Edges => {
                matches!(
                    (&entity.kind, &entity.data),
                    (
                        crate::storage::EntityKind::GraphEdge(_),
                        crate::storage::EntityData::Edge(_)
                    )
                )
            }
        }
    }

    fn matches_filter(
        &self,
        entity: &crate::storage::UnifiedEntity,
        compiled_filter: Option<&query_exec::CompiledEntityFilter>,
    ) -> bool {
        match (self.filter, compiled_filter) {
            (_, Some(compiled)) => compiled.evaluate(entity),
            (Some(filter), None) => query_exec::evaluate_entity_filter_with_db(
                Some(self.runtime.db().as_ref()),
                entity,
                filter,
                self.table,
                self.table,
            ),
            (None, None) => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowItemKind {
    Row,
    Document,
    Kv,
}

fn row_item_kind(entity: &crate::storage::UnifiedEntity) -> Option<RowItemKind> {
    let row = entity.data.as_row()?;
    let is_kv = row.named.as_ref().is_some_and(|named| {
        (named.len() == 2 && named.contains_key("key") && named.contains_key("value"))
            || (named.len() == 1 && (named.contains_key("key") || named.contains_key("value")))
    });
    if is_kv {
        return Some(RowItemKind::Kv);
    }
    let is_document = row
        .named
        .as_ref()
        .is_some_and(|named| named.values().any(row_value_is_documentish))
        || row.columns.iter().any(row_value_is_documentish);
    if is_document {
        Some(RowItemKind::Document)
    } else {
        Some(RowItemKind::Row)
    }
}

fn row_value_is_documentish(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

fn entity_row_snapshot(entity: &crate::storage::UnifiedEntity) -> Option<Vec<(String, Value)>> {
    match &entity.data {
        EntityData::Row(row) => {
            let mut snapshot: Vec<(String, Value)> = if let Some(named) = &row.named {
                named.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            } else {
                row.schema.as_ref().map(|schema| {
                    schema
                        .iter()
                        .cloned()
                        .zip(row.columns.iter().cloned())
                        .collect()
                })?
            };
            let tenant = row.get_field("tenant_id").cloned().unwrap_or(Value::Null);
            snapshot.extend([
                (
                    "rid".to_string(),
                    Value::UnsignedInteger(entity.logical_id().raw()),
                ),
                (
                    "collection".to_string(),
                    Value::text(entity.kind.collection().to_string()),
                ),
                ("kind".to_string(), Value::text("row".to_string())),
                ("tenant".to_string(), tenant),
                (
                    "created_at".to_string(),
                    Value::UnsignedInteger(entity.created_at),
                ),
                (
                    "updated_at".to_string(),
                    Value::UnsignedInteger(entity.updated_at),
                ),
            ]);
            Some(snapshot)
        }
        _ => None,
    }
}
