//! DML batch-flush / delete / secondary-index refresh helpers extracted from
//! `impl_dml`.
//!
//! Behaviour-preserving move (issue #1634). Names and behaviour are unchanged
//! from `impl_dml`; the only adjustment is `pub(super)` visibility on the
//! methods still called from the sibling `impl_dml` UPDATE/DELETE paths
//! (`delete_entities_batch`, `flush_update_chunk`, `persist_update_chunk`) so
//! they keep calling them by bare name.

use super::impl_dml_update_analysis::*;
use super::*;
use crate::application::entity::AppliedEntityMutation;
use crate::application::ports::entity_row_fields_snapshot;

impl RedDBRuntime {
    /// Returns `(affected_count, lsns)`. For the txn (xmax-stamp) path,
    /// `lsns` is empty because events fire at commit time.
    pub(super) fn delete_entities_batch(
        &self,
        collection: &str,
        ids: &[EntityId],
    ) -> RedDBResult<(u64, Vec<u64>)> {
        if ids.is_empty() {
            return Ok((0, vec![]));
        }

        let store = self.db().store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok((0, vec![]));
        };

        let active_xid = self.current_xid();
        let conn_id = crate::runtime::impl_core::current_connection_id();
        let mut autocommit_xid = None;
        let mut tombstoned_ids = Vec::new();
        let mut tombstoned_entities = Vec::new();
        let mut physical_delete_ids = Vec::new();
        let table_row_resolver =
            crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver::current_statement();
        // Phase 3: versioned graph collections tombstone node/edge
        // deletes (xmax stamp) instead of physically removing them, so
        // `AS OF` time-travel can still resolve the pre-delete version.
        // Non-versioned graph collections keep the legacy physical
        // delete (no history). Row entities (Phase 1/2) always tombstone
        // under universal MVCC and are unaffected by this flag.
        //
        // Vectors (Phase 3b): versioned vector deletes tombstone (xmax
        // stamp) instead of physically dropping, so history is retained.
        // The `VECTOR SEARCH` read path post-filters every candidate
        // through the current-snapshot visibility gate (which hides
        // `xmax != 0` even in autocommit), so a tombstoned versioned
        // vector never reaches live search. Non-versioned vectors keep
        // the legacy physical delete.
        let versioned_collection = self.vcs_is_versioned(collection).unwrap_or(false);

        for &id in ids {
            let Some(mut entity) = manager.get(id) else {
                continue;
            };
            let is_versioned_graph = versioned_collection
                && matches!(entity.data, EntityData::Node(_) | EntityData::Edge(_));
            let is_versioned_vector =
                versioned_collection && matches!(entity.data, EntityData::Vector(_));
            if matches!(entity.data, EntityData::Row(_))
                || is_versioned_graph
                || is_versioned_vector
            {
                let previous_xmax = entity.xmax;
                if matches!(entity.kind, crate::storage::EntityKind::TableRow { .. }) {
                    if table_row_resolver.resolve_candidate(&entity).is_none() {
                        continue;
                    }
                } else if is_versioned_vector {
                    // Versioned vectors gate the delete on the statement
                    // snapshot (not a crude `xmax != 0`) so a concurrent
                    // delete whose snapshot predates a still-uncommitted
                    // tombstone still targets the row, records its own
                    // pending tombstone, and conflicts at commit
                    // (first-committer-wins).
                    if table_row_resolver.resolve_read_candidate(&entity).is_none() {
                        continue;
                    }
                } else if entity.xmax != 0 {
                    continue;
                }

                let xid = match active_xid {
                    Some(xid) => xid,
                    None => match autocommit_xid {
                        Some(xid) => xid,
                        None => {
                            let mgr = self.snapshot_manager();
                            let xid = mgr.begin();
                            autocommit_xid = Some(xid);
                            xid
                        }
                    },
                };
                entity.set_xmax(xid);
                if manager.update(entity.clone()).is_ok() {
                    if active_xid.is_some() {
                        self.record_pending_tombstone(conn_id, collection, id, xid, previous_xmax);
                    }
                    tombstoned_entities.push(entity);
                    tombstoned_ids.push(id);
                }
            } else {
                physical_delete_ids.push(id);
            }
        }

        if let Some(xid) = autocommit_xid {
            self.snapshot_manager().commit(xid);
        }

        let mut affected = tombstoned_ids.len() as u64;
        let mut lsns = Vec::with_capacity(tombstoned_ids.len() + physical_delete_ids.len());
        if active_xid.is_some() {
            store
                .persist_entities_to_pager(collection, &tombstoned_entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        } else {
            store
                .persist_entities_to_pager(collection, &tombstoned_entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            for id in &tombstoned_ids {
                store.context_index().remove_entity(*id);
                let lsn = self.cdc_emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    collection,
                    id.raw(),
                    "entity",
                );
                lsns.push(lsn);
            }
        }

        let deleted_ids = store
            .delete_batch(collection, &physical_delete_ids)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        affected += deleted_ids.len() as u64;
        for id in &deleted_ids {
            store.context_index().remove_entity(*id);
            let lsn = self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
            lsns.push(lsn);
        }

        Ok((affected, lsns))
    }

    /// Flushes context-index updates and CDC for each applied mutation.
    /// Returns one LSN per entity in the same order as `applied`.
    pub(super) fn flush_update_chunk(
        &self,
        applied: &[AppliedEntityMutation],
    ) -> RedDBResult<Vec<u64>> {
        if applied.is_empty() {
            return Ok(Vec::new());
        }

        let store = self.db().store();
        if applied.iter().any(|item| item.context_index_dirty) {
            store.context_index().index_entities(
                &applied[0].collection,
                applied
                    .iter()
                    .filter(|item| item.context_index_dirty)
                    .map(|item| &item.entity),
            );
        }

        for item in applied {
            self.refresh_update_secondary_indexes(item)?;
        }

        let mut lsns = Vec::with_capacity(applied.len());
        for item in applied {
            let lsn = self.cdc_emit_prebuilt(
                crate::replication::cdc::ChangeOperation::Update,
                &item.collection,
                &item.entity,
                update_cdc_item_kind(self, &item.collection, &item.entity),
                item.metadata.as_ref(),
                false,
            );
            lsns.push(lsn);
        }
        Ok(lsns)
    }

    pub(super) fn persist_update_chunk(
        &self,
        applied: &[AppliedEntityMutation],
    ) -> RedDBResult<()> {
        self.persist_applied_entity_mutations(applied)
    }

    fn refresh_update_secondary_indexes(&self, applied: &AppliedEntityMutation) -> RedDBResult<()> {
        if applied.pre_mutation_fields.is_empty() {
            return Ok(());
        }
        let post = entity_row_fields_snapshot(&applied.entity);
        if post.is_empty() {
            return Ok(());
        }

        // Use the parent-expanded set so that dot-path indexes (e.g.
        // "body.service.tier") are triggered when the root field ("body")
        // is modified.
        let indexed_cols = self
            .index_store_ref()
            .indexed_columns_set_with_parents(&applied.collection);
        if indexed_cols.is_empty() {
            return Ok(());
        }

        // Single-source documents keep promoted index columns (`score`) in the
        // body, not as stored fields. Resolve each indexed column from the body
        // and fold it into the field snapshots so the diff below sees the value
        // move — for an ordinary stored column this adds nothing.
        let pre = self
            .index_store_ref()
            .augment_body_derived_index_fields(&applied.pre_mutation_fields, &indexed_cols);
        let post = self
            .index_store_ref()
            .augment_body_derived_index_fields(&post, &indexed_cols);

        if let Some(old_version) = applied.replaced_entity.as_ref() {
            let old_index_fields: Vec<(String, crate::storage::schema::Value)> = pre
                .iter()
                .filter(|(col, _)| indexed_cols.contains(col))
                .cloned()
                .collect();
            let new_index_fields: Vec<(String, crate::storage::schema::Value)> = post
                .iter()
                .filter(|(col, _)| indexed_cols.contains(col))
                .cloned()
                .collect();
            if !old_index_fields.is_empty() {
                self.index_store_ref()
                    .index_entity_delete(&applied.collection, old_version.id, &old_index_fields)
                    .map_err(crate::RedDBError::Internal)?;
            }
            if !new_index_fields.is_empty() {
                self.index_store_ref()
                    .index_entity_insert(&applied.collection, applied.entity.id, &new_index_fields)
                    .map_err(crate::RedDBError::Internal)?;
            }
            return Ok(());
        }

        let damage = crate::application::entity::row_damage_vector(&pre, &post);
        if damage
            .touched_columns()
            .into_iter()
            .any(|col| indexed_cols.contains(col))
        {
            self.index_store_ref()
                .index_entity_update(&applied.collection, applied.id, &pre, &post)
                .map_err(crate::RedDBError::Internal)?;
        }
        Ok(())
    }
}
