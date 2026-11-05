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
        let session =
            if let Some(path) = self.config.embedded_wal_path.as_deref() {
                Some(self.embedded_commit_session.get_or_init(|| {
                    crate::storage::EmbeddedRdbArtifact::group_commit_session(path)
                }))
            } else {
                self.embedded_commit_session.get()
            };
        if let Some(session) = session {
            session
                .check()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
        }
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
        let capture = StoreWalCaptureGuard::begin(session);
        let staged = (|| {
            self.insert_auto(collection, entity)?;
            if let Some(metadata) = metadata {
                self.set_metadata(collection, id, metadata)?;
            }
            if let Some(previous) = previous {
                self.delete(collection, previous)?;
                self.resident.context_index.remove_entity(previous);
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
                // Retain each backend's durability contract, including the
                // page flush after a committed WAL batch in paged Strict mode.
                self.finish_paged_write(actions.actions)
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

    // Paged opening hits the pre-existing Windows mandatory-lock limitation;
    // the baseline reproduction is retained with the architecture evidence.
    #[cfg(unix)]
    #[test]
    fn strict_paged_keyed_writes_flush_pages_before_acknowledgement() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("paged.rdb");
        let store = UnifiedStore::open_with_config(
            &path,
            UnifiedStoreConfig::default().with_durability_mode(DurabilityMode::Strict),
        )
        .expect("paged store");
        let previous = store
            .write_keyed_row("keys", row("original"), None, None)
            .expect("insert");
        assert_eq!(store.pager().expect("pager").dirty_page_count(), 0);
        store
            .write_keyed_row("keys", row("replacement"), None, Some(previous))
            .expect("replace");
        assert_eq!(store.pager().expect("pager").dirty_page_count(), 0);
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
            // An IO failure fences this session even after in-memory rollback.
            store.config.embedded_wal_path = None;
            assert!(store
                .write_keyed_row("keys", row("retry"), None, Some(previous))
                .is_err());
            let reloaded = UnifiedStore::load_from_bytes_with_config(
                &store.to_binary_dump_bytes(),
                UnifiedStoreConfig::default(),
            )
            .expect("reload restored image");
            reloaded
                .write_keyed_row("keys", row("retry"), None, Some(previous))
                .expect("new session");
            assert_eq!(reloaded.get_collection("keys").unwrap().count(), 1);
        }
    }

    #[test]
    fn interrupted_keyed_staging_fences_writes_and_keeps_the_durable_original() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("interrupted.rdb");
        crate::storage::EmbeddedRdbArtifact::create(&path).expect("artifact");
        let mut config = UnifiedStoreConfig::default();
        config.embedded_wal_path = Some(path.clone());
        config.durability_mode = DurabilityMode::Strict;
        let store = UnifiedStore::with_config(config);
        let previous = store
            .write_keyed_row("keys", row("original"), None, None)
            .expect("seed");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _publication = store.vault_publication_lock.lock();
            let session = store.embedded_commit_session.get().expect("session");
            let _capture = StoreWalCaptureGuard::begin(Some(session));
            store
                .insert_auto("keys", row("unpublished"))
                .expect("stage new");
            store.delete("keys", previous).expect("stage deletion");
            panic!("injected interruption before WAL publication");
        }));
        assert!(panic.is_err());
        assert!(!UnifiedStore::deferred_store_wal_capture_active());
        assert!(store
            .write_keyed_row("keys", row("must not ACK"), None, None)
            .is_err());
        assert!(crate::storage::EmbeddedRdbArtifact::write_snapshot(
            &path,
            &store.to_binary_dump_bytes()
        )
        .is_err());
        let open = crate::storage::EmbeddedRdbArtifact::open(&path).expect("durable outcome");
        let recovered = UnifiedStore::new();
        for payload in crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&open).expect("wal") {
            recovered
                .apply_encoded_store_wal_action(&payload)
                .expect("replay");
        }
        assert_eq!(recovered.get_collection("keys").unwrap().count(), 1);
        assert_eq!(
            recovered
                .get("keys", previous)
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .get_field("value"),
            Some(&Value::text("original"))
        );
    }
}
