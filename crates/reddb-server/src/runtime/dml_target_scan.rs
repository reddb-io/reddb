use super::{query_exec, RedDBRuntime};
use crate::api::{RedDBError, RedDBResult};
use crate::storage::query::ast::Filter;
use crate::storage::EntityId;

pub(super) struct DmlTargetScan<'a> {
    runtime: &'a RedDBRuntime,
    table: &'a str,
    filter: Option<&'a Filter>,
    limit: Option<usize>,
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
        }
    }

    pub(super) fn collect_ids(&self) -> RedDBResult<Vec<EntityId>> {
        let db = self.runtime.db();
        let store = db.store();
        let manager = store
            .get_collection(self.table)
            .ok_or_else(|| RedDBError::NotFound(self.table.to_string()))?;
        let compiled_filter = self.compiled_filter();

        if let Some(entity_id) = query_exec::extract_entity_id_from_filter(&self.filter.cloned()) {
            let Some(entity) = manager.get(EntityId::new(entity_id)) else {
                return Ok(Vec::new());
            };
            if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                return Ok(Vec::new());
            }
            if self.matches_filter(&entity, compiled_filter.as_ref()) {
                return Ok(vec![entity.id]);
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
                if self.matches_filter(entity, compiled_filter.as_ref()) {
                    ids.push(entity.id);
                    if self.limit.map(|limit| ids.len() >= limit).unwrap_or(false) {
                        return false;
                    }
                }
                true
            });
        } else {
            manager.for_each_entity(|entity| {
                if crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
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
        mut entity_ids: Vec<EntityId>,
        compiled_filter: Option<&query_exec::CompiledEntityFilter>,
    ) -> Vec<EntityId> {
        if let Some(limit) = self.limit {
            entity_ids.truncate(limit);
        }

        let db = self.runtime.db();
        let store = db.store();
        let Some(manager) = store.get_collection(self.table) else {
            return Vec::new();
        };

        let mut ids = Vec::with_capacity(entity_ids.len());
        for entity in manager.get_many(&entity_ids).into_iter().flatten() {
            if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                continue;
            }
            if self.matches_filter(&entity, compiled_filter) {
                ids.push(entity.id);
            }
        }
        ids
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
