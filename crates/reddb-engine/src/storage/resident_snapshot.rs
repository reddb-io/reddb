//! Private resident reconstruction using the canonical native snapshot semantics.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::entity::{EntityData, EntityId, EntityKind, RefType, UnifiedEntity};
use super::error::StoreError;
use super::manager::ManagerConfig;
use super::metadata::Metadata;
use super::resident_store::ResidentStore;
use super::snapshot::{visit_snapshot, SnapshotVisitor};
use std::collections::HashSet;

impl ResidentStore {
    /// Return a complete recovery target only after decoding succeeds. Hosts
    /// retain format upgrades, writer fencing and durable publication policy.
    pub fn from_snapshot(
        bytes: &[u8],
        manager_config: &ManagerConfig,
        auto_index_refs: bool,
        max_cross_refs: usize,
    ) -> Result<(Self, u32), String> {
        struct Restore<'a> {
            store: &'a ResidentStore,
            manager_config: &'a ManagerConfig,
            auto_index_refs: bool,
            max_cross_refs: usize,
        }
        impl SnapshotVisitor for Restore<'_> {
            fn collection(&mut self, name: &str) -> Result<(), String> {
                self.store
                    .get_or_create_collection(name, self.manager_config);
                Ok(())
            }

            fn entity(
                &mut self,
                collection: &str,
                entity: UnifiedEntity,
                metadata: Option<Metadata>,
            ) -> Result<(), String> {
                let id = entity.id;
                self.store
                    .restore_snapshot_entity(
                        collection,
                        entity,
                        self.manager_config,
                        self.auto_index_refs,
                    )
                    .map_err(|err| err.to_string())?;
                if let Some(metadata) = metadata {
                    if let Some(manager) = self.store.get_collection(collection) {
                        let _ = manager.set_metadata(id, metadata);
                    }
                }
                Ok(())
            }

            fn cross_ref(
                &mut self,
                cross_ref: reddb_file::NativeDumpCrossRef,
            ) -> Result<(), String> {
                let source_collection = self
                    .store
                    .get_any(EntityId::new(cross_ref.source_id))
                    .map(|(name, _)| name)
                    .unwrap_or_else(|| cross_ref.target_collection.clone());
                let _ = self.store.add_cross_ref_in_memory(
                    &source_collection,
                    EntityId::new(cross_ref.source_id),
                    &cross_ref.target_collection,
                    EntityId::new(cross_ref.target_id),
                    RefType::from_byte(cross_ref.ref_type),
                    1.0,
                    self.max_cross_refs,
                    |_| Ok(()),
                );
                Ok(())
            }

            fn auxiliary(&mut self, bytes: &[u8]) -> Result<(), String> {
                self.store.set_aux_metadata(bytes.to_vec());
                Ok(())
            }
        }

        let store = Self::new();
        let version = visit_snapshot(
            bytes,
            &mut Restore {
                store: &store,
                manager_config,
                auto_index_refs,
                max_cross_refs,
            },
        )?;
        Ok((store, version))
    }

    /// Load a persisted image and rebuild its derived indexes, retaining the
    /// stored sequence instead of assigning one from snapshot iteration order.
    fn restore_snapshot_entity(
        &self,
        collection: &str,
        mut entity: UnifiedEntity,
        manager_config: &ManagerConfig,
        auto_index_refs: bool,
    ) -> Result<EntityId, StoreError> {
        let manager = self.get_or_create_collection(collection, manager_config);
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        } else {
            self.register_entity_id(entity.id);
        }
        if let EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
            if *row_id == 0 {
                *row_id = manager.next_row_id();
            } else {
                manager.register_row_id(*row_id);
            }
        }
        entity.ensure_table_logical_id();
        self.context_index.index_entity(collection, &entity);
        if auto_index_refs {
            self.index_cross_refs(&entity, collection)?;
        }
        let graph_label = match &entity.kind {
            EntityKind::GraphNode(node) => Some(node.label.clone()),
            _ => None,
        };
        let id = manager.restore_snapshot_entity(entity)?;
        if let Some(label) = graph_label {
            self.update_graph_label_index(collection, &label, id);
        }
        Ok(id)
    }
}

impl ResidentStore {
    /// Serialize the native committed snapshot projection. The host holds its
    /// publication fence and supplies pending/aborted xid exclusions. The
    /// format-publication hook retains the native position before the footer,
    /// while collection/reference read guards still protect the captured image.
    #[allow(clippy::cast_possible_truncation)] // Preserve native count encoding.
    pub fn snapshot_bytes(
        &self,
        pending_versions: &HashSet<EntityId>,
        excluded_xids: &HashSet<u64>,
        before_footer: impl FnOnce(),
    ) -> Vec<u8> {
        let mut buf = Vec::new();

        // Version 9 includes explicit table-row logical identity plus MVCC
        // xmin/xmax alongside the V7 metadata envelope.
        buf.extend_from_slice(&reddb_file::encode_native_store_header(
            reddb_file::STORE_VERSION_CURRENT,
        ));

        // Get all collections
        let collections = self.collections.read();
        reddb_file::encode_native_dump_count(&mut buf, collections.len() as u32);

        let fv = reddb_file::STORE_VERSION_CURRENT;
        for (name, manager) in collections.iter() {
            // Get all entities from this collection
            let entities = manager.query_all(|entity| {
                if pending_versions.contains(&entity.id) {
                    return false;
                }
                !matches!(
                    entity.data,
                    EntityData::Row(_) | EntityData::Vector(_) | EntityData::QueueMessage(_)
                ) || !excluded_xids.contains(&entity.xmin)
            });
            reddb_file::encode_native_dump_collection_header(&mut buf, name, entities.len() as u32);

            // V7+: serialize entity+metadata as a length-prefixed record.
            // Each record: [u32 len][serialize_entity_record bytes]
            for mut entity in entities {
                // Unfinished and rolled-back deletions cannot become committed
                // tombstones when recovery reconstructs the xid floor.
                if matches!(
                    entity.data,
                    EntityData::Row(_) | EntityData::QueueMessage(_)
                ) && excluded_xids.contains(&entity.xmax)
                {
                    entity.xmax = 0;
                }
                let metadata = manager.get_metadata(entity.id);
                let record =
                    super::metadata_codec::serialize_entity_record(&entity, metadata.as_ref(), fv);
                reddb_file::encode_native_dump_entity_record(&mut buf, &record);
            }
        }

        // Write cross-references
        let cross_refs = self.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        reddb_file::encode_native_dump_count(&mut buf, total_refs as u32);

        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_dump_cross_ref(
                    &mut buf,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        // V10+: trailing opaque auxiliary-metadata blob (collection contracts
        // for the single-file artifact). Always written — empty when unused —
        // so the read side can unconditionally expect it for V10 dumps.
        {
            let aux = self.aux_metadata.read();
            reddb_file::encode_native_dump_entity_record(&mut buf, &aux);
        }

        before_footer();

        reddb_file::append_native_store_crc32_footer(&mut buf);
        buf
    }
}
