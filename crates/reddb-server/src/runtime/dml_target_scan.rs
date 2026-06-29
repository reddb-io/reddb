//! DML target scan: locate the entity ids a DML statement should
//! mutate.
//!
//! Lifts the "find the rows that match WHERE" loop out of the inline
//! DELETE path into a small testable module. The same Interface will
//! be reused by UPDATE in a follow-up; this commit covers DELETE only.
//!
//! Candidate discovery stays with each scan path. Table-row visibility
//! is centralized through `TableRowMvccReadResolver` before any
//! candidate id is returned to UPDATE or DELETE.

use std::collections::HashMap;

use super::{query_exec, RedDBRuntime};
use crate::api::{RedDBError, RedDBResult};
use crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver;
use crate::storage::query::ast::{Filter, UpdateTarget};
use crate::storage::schema::Value;
use crate::storage::unified::entity::EntityKind;
use crate::storage::{EntityData, EntityId};

pub(super) struct DmlTargetScan<'a> {
    runtime: &'a RedDBRuntime,
    table: &'a str,
    filter: Option<&'a Filter>,
    limit: Option<usize>,
    target: Option<UpdateTarget>,
    table_row_resolver: TableRowMvccReadResolver,
    live_table_rows: bool,
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
            table_row_resolver: TableRowMvccReadResolver::current_statement(),
            live_table_rows: false,
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
            table_row_resolver: TableRowMvccReadResolver::current_statement(),
            live_table_rows: false,
        }
    }

    pub(super) fn with_live_table_rows(mut self) -> Self {
        self.live_table_rows = true;
        self
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

        // Fast path: WHERE _entity_id = N. The resolver maps stable
        // table-row identity to the version visible to this statement.
        // Skip for graph node/edge targets — the table-row resolver only
        // knows TableRow entities, so a graph node whose logical_id == N
        // would be lost and yield zero matches.
        if !matches!(
            self.target,
            Some(UpdateTarget::Nodes) | Some(UpdateTarget::Edges)
        ) {
            let entity_id = if matches!(self.target, Some(UpdateTarget::Documents)) {
                extract_document_entity_id_from_filter(self.filter)
            } else {
                query_exec::extract_entity_id_from_filter(&self.filter.cloned())
            };
            if let Some(entity_id) = entity_id {
                let logical_id = EntityId::new(entity_id);
                let entity = if self.live_table_rows {
                    store.get_table_row_by_logical_id(self.table, logical_id)
                } else {
                    self.table_row_resolver
                        .resolve_logical_id(&store, self.table, logical_id)
                };
                if let Some(entity) = entity {
                    return Ok(self.target_ids_from_entities([entity]));
                }
                // Non-table-row entities (e.g. vectors) carry their
                // physical id as their identity and are invisible to the
                // table-row logical-id resolver above. Fall back to a
                // direct physical-id lookup so `DELETE FROM <vector>
                // WHERE rid = N` actually targets the vector instead of
                // silently matching nothing.
                if let Some(entity) = store.get(self.table, logical_id) {
                    if !matches!(entity.kind, EntityKind::TableRow { .. }) {
                        return Ok(self.target_ids_from_entities([entity]));
                    }
                }
                return Ok(Vec::new());
            }
        }

        if !matches!(self.target, Some(UpdateTarget::Documents))
            && !crate::runtime::impl_core::current_snapshot_requires_index_fallback()
        {
            if let Some(filter) = self.filter {
                if let Some(ids) = query_exec::try_hash_eq_lookup(
                    filter,
                    self.table,
                    self.runtime.index_store_ref(),
                ) {
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
        }

        let mut ids = Vec::new();
        if let Some(filter) = self.filter {
            let mut owned_zone_preds = Vec::new();
            if !matches!(self.target, Some(UpdateTarget::Documents)) {
                query_exec::extract_zone_predicates(filter, &mut owned_zone_preds);
            }
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
                if !self.visible_candidate(entity) {
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
                if self.visible_candidate(entity) && self.matches_update_target(entity) {
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

        let mut ids = Vec::with_capacity(entity_ids.len());
        for entity in manager.get_many(&entity_ids).into_iter().flatten() {
            if self.visible_candidate(&entity)
                && self.matches_update_target(&entity)
                && self.matches_filter(&entity, compiled_filter)
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
            .filter(|entity| self.visible_candidate(entity))
            .filter(|entity| self.matches_update_target(entity))
            .map(|entity| entity.id)
            .collect()
    }

    fn visible_candidate(&self, entity: &crate::storage::UnifiedEntity) -> bool {
        // Moderation visibility gate (#1274): quarantine-pending and
        // rejected-tombstone rows are excluded from DML targeting too, so
        // an UPDATE/DELETE without an explicit moderation override never
        // re-touches a hidden row. The resolver path below routes through
        // `entity_visible_under_current_snapshot`, which already applies
        // the same check; the `live_table_rows` fast path bypasses that
        // helper, so it must apply the marker check here.
        if crate::runtime::ai::moderation::entity_moderation_hidden(entity) {
            return false;
        }
        if matches!(entity.kind, EntityKind::TableRow { .. }) {
            if self.live_table_rows {
                return entity.xmax == 0;
            }
            return self.table_row_resolver.resolve_candidate(entity).is_some();
        }
        crate::runtime::impl_core::entity_visible_under_current_snapshot(entity)
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

fn extract_document_entity_id_from_filter(filter: Option<&Filter>) -> Option<u64> {
    use crate::storage::query::ast::{CompareOp, FieldRef};

    let filter = filter?;
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if field_name != "id" && field_name != "rid" && field_name != "entity_id" {
                return None;
            }
            match value {
                Value::Integer(n) if *n >= 0 => Some(*n as u64),
                Value::UnsignedInteger(n) => Some(*n),
                Value::Text(s) => s.parse::<u64>().ok(),
                _ => None,
            }
        }
        Filter::And(left, right) => extract_document_entity_id_from_filter(Some(left))
            .or_else(|| extract_document_entity_id_from_filter(Some(right))),
        _ => None,
    }
}

fn entity_row_snapshot(entity: &crate::storage::UnifiedEntity) -> Option<Vec<(String, Value)>> {
    match &entity.data {
        EntityData::Row(row) => {
            let mut snapshot = crate::application::ports::entity_row_fields_snapshot(entity);
            if snapshot.is_empty() {
                return None;
            }
            let tenant = row.get_field("tenant_id").cloned().unwrap_or(Value::Null);
            let kind = match row_item_kind(entity) {
                Some(RowItemKind::Kv) => "kv",
                Some(RowItemKind::Document) => "document",
                _ => "row",
            };
            snapshot.extend([
                (
                    "rid".to_string(),
                    Value::UnsignedInteger(entity.logical_id().raw()),
                ),
                (
                    "collection".to_string(),
                    Value::text(entity.kind.collection().to_string()),
                ),
                ("kind".to_string(), Value::text(kind.to_string())),
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
