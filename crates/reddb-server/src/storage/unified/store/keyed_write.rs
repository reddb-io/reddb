//! Publish an unversioned keyed replacement and its metadata in one WAL batch.

use super::commit::{StoreWalAction, StoreWalCaptureGuard};
use super::*;

impl UnifiedStore {
    pub(crate) fn write_keyed_row(
        &self,
        collection: &str,
        mut entity: UnifiedEntity,
        metadata: Option<Metadata>,
        previous: Option<EntityId>,
    ) -> Result<EntityId, StoreError> {
        assert!(
            !Self::deferred_store_wal_capture_active(),
            "keyed writes cannot nest WAL captures"
        );
        let mut publication = Some(self.vault_publication_lock.lock());
        let before = previous
            .and_then(|id| self.get(collection, id))
            .map(|entity| {
                let metadata = self.get_metadata(collection, entity.id);
                StoreWalAction::upsert_entity(
                    collection,
                    &entity,
                    metadata.as_ref(),
                    self.format_version(),
                )
            });
        entity.id = self.next_entity_id();
        let id = entity.id;
        let capture = StoreWalCaptureGuard::begin();
        let staged = (|| {
            self.insert_auto(collection, entity)?;
            if let Some(metadata) = metadata {
                self.set_metadata(collection, id, metadata)?;
            }
            if let Some(previous) = previous {
                self.delete(collection, previous)?;
                self.context_index.remove_entity(previous);
            }
            Ok(())
        })();
        let actions = capture.finish();
        // Keep a failed ticket alive through rollback: checkpoint's FIFO fence
        // must still observe the failure while staged rows are being restored.
        let mut pending = None;
        let committed = staged.and_then(|()| {
            pending = self.enqueue_grouped_actions(&actions.actions)?;
            if let Some(pending) = &pending {
                // Checkpoint drains these queued actions before replacing the WAL.
                drop(publication.take());
                self.complete_grouped_actions(pending, &actions.actions)
            } else {
                self.append_deferred_store_wal_actions(actions)
            }
        });
        if let Err(error) = committed {
            let _publication = self.vault_publication_lock.lock();
            self.apply_replayed_action(
                &StoreWalAction::DeleteEntityRecord {
                    collection: collection.to_string(),
                    entity_id: id.raw(),
                },
                true,
            )?;
            if let Some(before) = before {
                // A deleted flat slot retains its tombstone. Restore into a
                // fresh growing segment so it cannot hide the restored row.
                self.get_collection(collection)
                    .expect("staged keyed collection")
                    .force_seal()?;
                self.apply_replayed_action(&before, true)?;
            }
            return Err(error);
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(value: &str) -> UnifiedEntity {
        let mut entity = UnifiedEntity::table_row(EntityId::new(0), "keys", 0, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![Value::text("item"), Value::text(value)],
            vec!["key".into(), "value".into()],
        ));
        entity
    }

    #[test]
    fn failed_keyed_publication_restores_value_metadata_and_capture_state() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let mut store = UnifiedStore::new();
            let metadata = Metadata::with_fields(HashMap::from([(
                "label".into(),
                MetadataValue::String("original".into()),
            )]));
            let previous = store
                .write_keyed_row("keys", row("original"), Some(metadata.clone()), None)
                .expect("seed");
            let before = store.get("keys", previous).expect("previous");
            let directory = tempfile::tempdir().expect("directory");
            // Opening a directory as the artifact deterministically fails on both platforms.
            store.config.embedded_wal_path = Some(directory.path().to_path_buf());
            store.config.durability_mode = mode;
            assert!(store
                .write_keyed_row("keys", row("replacement"), None, Some(previous))
                .is_err());
            assert!(!UnifiedStore::deferred_store_wal_capture_active());
            let restored = store.get("keys", previous).expect("restored previous");
            assert_eq!(
                restored.data.as_row().unwrap().get_field("value"),
                before.data.as_row().unwrap().get_field("value")
            );
            assert_eq!(
                store.get_metadata("keys", previous).unwrap().fields,
                metadata.fields
            );
            assert_eq!(store.get_collection("keys").unwrap().count(), 1);
            // No deferred write may leak into an unrelated subsequent write.
            store.config.embedded_wal_path = None;
            store
                .write_keyed_row("keys", row("retry"), None, Some(previous))
                .expect("retry");
            assert_eq!(store.get_collection("keys").unwrap().count(), 1);
        }
    }
}
