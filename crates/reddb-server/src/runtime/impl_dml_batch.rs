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

#[derive(Default)]
pub(super) struct DeleteBatchResult {
    pub(super) affected: u64,
    pub(super) changes: Vec<(EntityId, u64)>,
    pub(super) pre_images: HashMap<u64, crate::json::Value>,
    // Event snapshots and rollback images share admission until both owners end.
    _reservations: Vec<std::rc::Rc<super::memory_admission::OwnedMemoryReservation>>,
}

impl RedDBRuntime {
    /// Correlate CDC with actual physical identities, including skipped inputs.
    /// Transactional tombstones defer their CDC until commit.
    pub(super) fn delete_entities_batch(
        &self,
        collection: &str,
        ids: &[EntityId],
        _topology_guard: parking_lot::RwLockReadGuard<'_, ()>,
        filter: Option<&Filter>,
        capture_pre_images: bool,
    ) -> RedDBResult<DeleteBatchResult> {
        if ids.is_empty() {
            return Ok(DeleteBatchResult::default());
        }

        let store = self.db().store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(DeleteBatchResult::default());
        };

        let row_locks =
            super::delete_publication::lock_delete_rows(self, collection, &manager, ids)?;
        // Lock order remains topology, row RMW, then store publication.
        let _publication = store.vault_publication_lock.lock();
        let active_xid = self.current_xid();
        let conn_id = crate::runtime::impl_core::current_connection_id();
        let mut preimages = Vec::new();
        let mut reservations = super::memory_admission::MutationMemoryReservations::new(self);
        let mut physical_delete_ids = Vec::new();
        let mut index_delete_fields = Vec::new();
        let mut pre_images = HashMap::new();
        let snapshot = super::impl_core::capture_current_snapshot().map(|mut context| {
            // A waited autocommit writer must recheck what its predecessor
            // committed, rather than overwriting an older snapshot's xmax.
            if active_xid.is_none() {
                context.snapshot = context.manager.fresh_read_snapshot();
            }
            context
        });
        let resolver = super::table_row_mvcc_resolver::TableRowMvccReadResolver::captured(snapshot);
        let versioned_collection = self.vcs_is_versioned(collection)?;

        // Finish every fallible source read before allocating a writer or
        // publishing a tombstone. A later read cannot turn into a partial delete.
        for &id in ids {
            let Some(mut entity) = manager.try_get(id, usize::MAX)? else {
                continue;
            };
            if active_xid.is_none()
                && matches!(entity.kind, crate::storage::EntityKind::TableRow { .. })
                && resolver.resolve_candidate(&entity).is_none()
            {
                let Some(current) =
                    resolver.try_resolve_logical_id(&store, collection, entity.logical_id())?
                else {
                    continue;
                };
                entity = current;
            }
            let versioned_graph = versioned_collection
                && matches!(entity.data, EntityData::Node(_) | EntityData::Edge(_));
            let versioned_vector =
                versioned_collection && matches!(entity.data, EntityData::Vector(_));
            if matches!(entity.data, EntityData::Row(_)) || versioned_graph || versioned_vector {
                let visible = if matches!(entity.kind, crate::storage::EntityKind::TableRow { .. })
                {
                    resolver.resolve_candidate(&entity).is_some()
                } else if versioned_vector {
                    resolver.resolve_read_candidate(&entity).is_some()
                } else {
                    entity.xmax == 0
                };
                if !visible || !self.delete_candidate_matches_filter(collection, &entity, filter)? {
                    continue;
                }
                // Retained preimages, postimages and the resident replacement
                // overlap through WAL publication. Keep their admission owned
                // by the rollback journal, including a captured event callback.
                let bytes = crate::storage::unified::memory_size::entity_bytes(&entity)
                    .saturating_add(
                        crate::storage::unified::memory_size::entity_zone_growth_bytes(&entity),
                    )
                    .saturating_mul(if capture_pre_images { 4 } else { 3 })
                    as u64;
                reservations.admit_growth(bytes, "delete")?;
                if capture_pre_images {
                    pre_images.insert(entity.id.raw(), super::mutation::entity_row_json(&entity));
                }
                preimages.push(entity);
            } else {
                if !self.delete_candidate_matches_filter(collection, &entity, filter)? {
                    continue;
                }
                if capture_pre_images {
                    reservations.admit_growth(
                        crate::storage::unified::memory_size::entity_bytes(&entity) as u64,
                        "delete event",
                    )?;
                    pre_images.insert(entity.id.raw(), super::mutation::entity_row_json(&entity));
                }
                let fields = entity_row_fields_snapshot(&entity);
                if !fields.is_empty() {
                    index_delete_fields.push((id, fields));
                }
                physical_delete_ids.push(id);
            }
        }

        let mut affected = preimages.len() as u64;
        let mut changes = Vec::with_capacity(preimages.len() + physical_delete_ids.len());
        let owned_reservations = reservations.into_owned();
        if !preimages.is_empty() {
            let mut publication = super::delete_publication::DeletePublication::new(
                self,
                collection,
                Arc::clone(&manager),
                preimages,
                owned_reservations.clone(),
                active_xid,
                row_locks,
            );
            let persistence = (|| -> RedDBResult<Vec<UnifiedEntity>> {
                let entities = publication.apply()?;
                // Never dirty tree pages before this batch has durable WAL.
                // Checkpoint and WAL replay merge the resident replacements.
                store
                    .persist_entities_to_pager_wal_only(collection, &entities)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?;
                Ok(entities)
            })();
            let entities = match persistence {
                Ok(entities) => entities,
                Err(error) => {
                    publication.abort()?;
                    return Err(error);
                }
            };
            let tombstoned_ids: Vec<_> = entities.into_iter().map(|entity| entity.id).collect();
            if active_xid.is_some() {
                publication.finish_transaction(conn_id);
            } else {
                // With event capture, visibility and index removal wait for
                // the outer WAL append. Failure there restores the preimages.
                publication.finish_autocommit()?;
                for id in tombstoned_ids {
                    changes.push((
                        id,
                        self.cdc_emit(
                            crate::replication::cdc::ChangeOperation::Delete,
                            collection,
                            id.raw(),
                            "entity",
                        ),
                    ));
                }
            }
        }

        let deleted_ids = store
            .delete_batch(collection, &physical_delete_ids)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        #[cfg(test)]
        if !deleted_ids.is_empty() {
            self.index_store_ref().mutation_test_hook(
                collection,
                super::index_store::MutationTestPhase::StoragePublished,
            );
        }
        affected += deleted_ids.len() as u64;
        for id in &deleted_ids {
            if let Some((_, fields)) = index_delete_fields
                .iter()
                .find(|(indexed_id, _)| indexed_id == id)
            {
                self.index_store_ref()
                    .index_entity_delete(collection, *id, fields)
                    .map_err(RedDBError::Internal)?;
            }
            store.context_index().remove_entity(*id);
            changes.push((
                *id,
                self.cdc_emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    collection,
                    id.raw(),
                    "entity",
                ),
            ));
        }

        Ok(DeleteBatchResult {
            affected,
            changes,
            pre_images,
            _reservations: owned_reservations,
        })
    }

    fn delete_candidate_matches_filter(
        &self,
        collection: &str,
        entity: &UnifiedEntity,
        filter: Option<&Filter>,
    ) -> RedDBResult<bool> {
        let Some(filter) = filter else {
            return Ok(true);
        };
        crate::runtime::query_exec::evaluate_entity_filter_result_with_db(
            Some(self.db().as_ref()),
            entity,
            filter,
            collection,
            collection,
        )
    }

    /// Flushes context-index updates and CDC for each applied mutation.
    /// Returns one LSN per entity in the same order as `applied`.
    pub(super) fn flush_update_chunk(
        &self,
        applied: &[AppliedEntityMutation],
        topology_guard: parking_lot::RwLockReadGuard<'_, ()>,
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

        drop(topology_guard);
        #[cfg(test)]
        self.index_store_ref().mutation_test_hook(
            &applied[0].collection,
            super::index_store::MutationTestPhase::BeforeEvents,
        );
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
        // One item cannot amortize a WAL append. Transactions and event-enabled
        // statements already own their capture. Otherwise publish this bounded
        // chunk with one durable WAL append,
        // instead of synchronizing each old/new row-version pair separately.
        // The wrapper also appends completed writes if a later item fails.
        if applied.len() <= 1 || UnifiedStore::deferred_store_wal_capture_active() {
            self.persist_applied_entity_mutations(applied)?;
        } else {
            self.with_deferred_store_wal_for_dml(true, || {
                self.persist_applied_entity_mutations(applied)
            })?;
        }
        #[cfg(test)]
        if let Some(first) = applied.first() {
            self.index_store_ref().mutation_test_hook(
                &first.collection,
                super::index_store::MutationTestPhase::StoragePublished,
            );
        }
        Ok(())
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
            let old_index_fields: Vec<(String, reddb_types::Value)> = pre
                .iter()
                .filter(|(col, _)| indexed_cols.contains(col))
                .cloned()
                .collect();
            let new_index_fields: Vec<(String, reddb_types::Value)> = post
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
