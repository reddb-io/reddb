//! Combine independent row publications before entering the checkpoint fence.
//!
//! N concurrent publications cost one durable WAL batch instead of N fsyncs.
//! No staged mutation outlives the publication lock, including on failure.

use super::commit::{StoreWalAction, StoreWalCaptureGuard};
use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct RowPublication {
    pub inserts: Vec<(String, UnifiedEntity, Option<Metadata>)>,
    /// Existing physical versions whose MVCC stamps change in this publication.
    pub replacements: Vec<(String, UnifiedEntity, Option<Metadata>)>,
    pub deletes: Vec<(String, EntityId)>,
}

struct Request {
    prepare: RowPreparation,
    bytes: u64,
    result: PublicationResult,
}

type PublicationResult = Arc<parking_lot::Mutex<Option<Result<(), String>>>>;

// Preparation is read-only and runs under the publication fence. Earlier plans
// have not mutated the store yet; a planner must account for their reservations.
type RowPreparation =
    Box<dyn FnOnce(&UnifiedStore, &[RowPublication]) -> Result<RowPublication, StoreError> + Send>;

#[derive(Default)]
struct QueueState {
    active: bool,
    requests: VecDeque<Request>,
}

#[derive(Default)]
pub(super) struct RowPublicationQueue {
    state: parking_lot::Mutex<QueueState>,
    changed: parking_lot::Condvar,
}

impl UnifiedStore {
    pub(crate) fn publish_rows(&self, rows: RowPublication) -> Result<(), StoreError> {
        assert!(!Self::deferred_store_wal_capture_active());
        Self::validate_row_publication(&rows)?;
        if self.commit.is_none()
            || self.config.durability_mode != DurabilityMode::WalDurableGrouped
            || self.vault_publication_lock.is_owned_by_current_thread()
        {
            return self.publish_row_group(vec![rows]);
        }
        let bytes = self.row_publication_bytes(&rows);
        self.enqueue_row_preparation(Box::new(move |_, _| Ok(rows)), bytes)
    }

    /// Prepare independent rows under the checkpoint fence and return the
    /// associated result only after their publication is durable. The closure
    /// may run on a different caller's thread; it must not use ambient TLS.
    pub(crate) fn prepare_and_publish_rows<T: Send + 'static>(
        &self,
        prepare: impl FnOnce(&UnifiedStore, &[RowPublication]) -> Result<(RowPublication, T), StoreError>
            + Send
            + 'static,
    ) -> Result<T, StoreError> {
        let output = Arc::new(parking_lot::Mutex::new(None));
        let prepared_output = Arc::clone(&output);
        self.enqueue_row_preparation(
            Box::new(move |store, previous| {
                let (rows, value) = prepare(store, previous)?;
                *prepared_output.lock() = Some(value);
                Ok(rows)
            }),
            0,
        )?;
        let value = output
            .lock()
            .take()
            .expect("successful preparation produces a result");
        Ok(value)
    }

    fn row_publication_bytes(&self, rows: &RowPublication) -> u64 {
        rows.inserts
            .iter()
            .chain(&rows.replacements)
            .fold(0u64, |sum, (collection, entity, metadata)| {
                sum.saturating_add(
                    StoreWalAction::upsert_entity(
                        collection,
                        entity,
                        metadata.as_ref(),
                        self.format_version(),
                    )
                    .encode()
                    .len() as u64,
                )
            })
            .saturating_add(
                rows.deletes
                    .iter()
                    .map(|(name, _)| name.len() as u64 + 16)
                    .sum::<u64>(),
            )
    }

    fn validate_row_publication(rows: &RowPublication) -> Result<(), StoreError> {
        for (collection, entity, _) in rows.inserts.iter().chain(&rows.replacements) {
            super::config_keys::validate_config_entity(collection, entity)?;
        }
        Ok(())
    }

    fn enqueue_row_preparation(
        &self,
        prepare: RowPreparation,
        bytes: u64,
    ) -> Result<(), StoreError> {
        assert!(!Self::deferred_store_wal_capture_active());
        // A reentrant caller must not wait for another thread to take its lock.
        if self.commit.is_none()
            || self.config.durability_mode != DurabilityMode::WalDurableGrouped
            || self.vault_publication_lock.is_owned_by_current_thread()
        {
            let _publication = self.vault_publication_lock.lock();
            return self.publish_row_group(vec![prepare(self, &[])?]);
        }
        let result = Arc::new(parking_lot::Mutex::new(None));
        let queue = &self.row_publications;
        let mut state = queue.state.lock();
        state.requests.push_back(Request {
            prepare,
            bytes,
            result: Arc::clone(&result),
        });
        queue.changed.notify_all();
        loop {
            if let Some(result) = result.lock().take() {
                return result.map_err(|error| StoreError::Io(std::io::Error::other(error)));
            }
            if state.active {
                queue.changed.wait(&mut state);
                continue;
            }
            state.active = true;
            let options = self.config.group_commit;
            let deadline =
                std::time::Instant::now() + StoreCommitCoordinator::resolve_window(&options);
            let limit = options.max_statements.max(1);
            while state.requests.len() < limit
                && state
                    .requests
                    .iter()
                    .map(|request| request.bytes)
                    .sum::<u64>()
                    < options.max_wal_bytes
                && std::time::Instant::now() < deadline
            {
                queue.changed.wait_until(&mut state, deadline);
            }
            let mut requests = Vec::new();
            let mut bytes = 0u64;
            while requests.len() < limit {
                let Some(next) = state.requests.front() else {
                    break;
                };
                if !requests.is_empty() && bytes.saturating_add(next.bytes) > options.max_wal_bytes
                {
                    break;
                }
                let request = state.requests.pop_front().expect("front exists");
                bytes = bytes.saturating_add(request.bytes);
                requests.push(request);
            }
            drop(state);
            self.publish_prepared_group(requests);
            state = queue.state.lock();
            state.active = false;
            queue.changed.notify_all();
        }
    }

    fn publish_prepared_group(&self, requests: Vec<Request>) {
        let _publication = self.vault_publication_lock.lock();
        let mut rows = Vec::new();
        let mut results = Vec::new();
        let mut bytes = 0u64;
        let flush = |rows: &mut Vec<RowPublication>, results: &mut Vec<PublicationResult>| {
            let published = self
                .publish_row_group(std::mem::take(rows))
                .map_err(|error| error.to_string());
            for result in results.drain(..) {
                *result.lock() = Some(published.clone());
            }
            published
        };
        for request in requests {
            match (request.prepare)(self, &rows) {
                Ok(prepared) => {
                    let prepared_bytes = if request.bytes == 0 {
                        self.row_publication_bytes(&prepared)
                    } else {
                        request.bytes
                    };
                    // Enforce the same byte budget as prebuilt requests. One
                    // oversized request is allowed only in its own publication.
                    if !rows.is_empty()
                        && bytes.saturating_add(prepared_bytes)
                            > self.config.group_commit.max_wal_bytes
                    {
                        if let Err(error) = flush(&mut rows, &mut results) {
                            *request.result.lock() = Some(Err(error));
                            bytes = 0;
                            continue;
                        }
                        bytes = 0;
                    }
                    bytes = bytes.saturating_add(prepared_bytes);
                    rows.push(prepared);
                    results.push(request.result);
                }
                Err(error) => *request.result.lock() = Some(Err(error.to_string())),
            }
        }
        let _ = flush(&mut rows, &mut results);
    }

    fn publish_row_group(&self, rows: Vec<RowPublication>) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        for request in &rows {
            Self::validate_row_publication(request)?;
        }
        if rows.iter().all(|rows| {
            rows.inserts.is_empty() && rows.replacements.is_empty() && rows.deletes.is_empty()
        }) {
            return Ok(());
        }
        let session = self
            .config
            .embedded_wal_path
            .as_deref()
            .map(|path| {
                self.embedded_commit_session
                    .get_or_init(|| crate::storage::EmbeddedRdbArtifact::group_commit_session(path))
            })
            .or_else(|| self.embedded_commit_session.get());
        if let Some(session) = session {
            session
                .check()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
        }
        // Read every preimage before changing anything. Failed cold reads must
        // not turn a partial view into a successful delete or replacement.
        let mut before = Vec::new();
        let mut inserted = Vec::new();
        let mut deleted_ids = std::collections::HashSet::new();
        for request in &rows {
            for (collection, id) in request.deletes.iter().map(|(name, id)| (name, *id)).chain(
                request
                    .replacements
                    .iter()
                    .map(|(name, entity, _)| (name, entity.id)),
            ) {
                if !deleted_ids.insert((collection.as_str(), id)) {
                    return Err(StoreError::Io(std::io::Error::other(
                        "overlapping row publications",
                    )));
                }
                let entity = self
                    .try_get(collection, id, usize::MAX)?
                    .ok_or(StoreError::EntityNotFound(id))?;
                let metadata = self.try_get_metadata(collection, id, usize::MAX)?;
                before.push((
                    collection.clone(),
                    id,
                    StoreWalAction::upsert_entity(
                        collection,
                        &entity,
                        metadata.as_ref(),
                        self.format_version(),
                    ),
                ));
            }
        }
        let capture = StoreWalCaptureGuard::begin(session);
        let staged = (|| {
            for request in rows {
                for (collection, entity, metadata) in request.replacements {
                    let manager = self
                        .get_collection(&collection)
                        .ok_or_else(|| StoreError::CollectionNotFound(collection.clone()))?;
                    self.resident.entity_cache.remove(entity.id.raw());
                    manager.update_with_metadata(entity.clone(), metadata.as_ref())?;
                    self.persist_entities_to_pager_wal_only(&collection, &[entity])?;
                }
                for (collection, mut entity, metadata) in request.inserts {
                    if entity.id.raw() == 0 {
                        entity.id = self.next_entity_id();
                    }
                    let id = entity.id;
                    inserted.push((collection.clone(), id));
                    self.insert_auto(&collection, entity)?;
                    if let Some(metadata) = metadata {
                        self.set_metadata(&collection, id, metadata)?;
                    }
                }
                for (collection, id) in request.deletes {
                    self.delete(&collection, id)?;
                    self.resident.context_index.remove_entity(id);
                }
            }
            Ok(())
        })();
        let mut actions = capture.finish();
        let committed = staged.and_then(|()| {
            #[cfg(test)]
            if self
                .test_fail_deferred_wal_append
                .swap(false, Ordering::AcqRel)
            {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected row publication failure",
                )));
            }
            if let Some(commit) = self.commit.as_ref().filter(|_| {
                self.config.durability_mode == DurabilityMode::WalDurableGrouped
                    && !self.is_read_only()
            }) {
                let track = self.config.resident_memory_bytes_max.is_some();
                let epoch_guard = self.append_result_cache_epoch_action(&mut actions.actions)?;
                let references = commit
                    .append_ready_actions(&actions.actions, track)
                    .map_err(StoreError::Io)?;
                if track {
                    self.remember_checkpoint_rows(actions.actions, references);
                }
                if let Some(guard) = epoch_guard {
                    guard.commit();
                }
                Ok(())
            } else {
                self.finish_paged_write(actions.actions)
            }
        });
        if let Err(error) = committed {
            for (collection, id) in inserted {
                self.apply_replayed_action(
                    &StoreWalAction::DeleteEntityRecord {
                        collection,
                        entity_id: id.raw(),
                    },
                    true,
                )?;
            }
            for (collection, id, action) in before {
                self.apply_replayed_action(
                    &StoreWalAction::DeleteEntityRecord {
                        collection: collection.clone(),
                        entity_id: id.raw(),
                    },
                    true,
                )?;
                self.get_collection(&collection)
                    .expect("preimage collection")
                    .force_seal()?;
                self.apply_replayed_action(&action, true)?;
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: usize) -> UnifiedEntity {
        UnifiedEntity::table_row(
            EntityId::new(0),
            "keys",
            0,
            vec![Value::Integer(key as i64)],
        )
    }

    #[test]
    fn prepared_publication_failure_restores_every_accepted_request() {
        let store = UnifiedStore::new();
        let original = store.insert_auto("keys", row(1)).expect("source");
        let first = Arc::new(parking_lot::Mutex::new(None));
        let second = Arc::new(parking_lot::Mutex::new(None));
        let invalid = Arc::new(parking_lot::Mutex::new(None));
        store
            .test_fail_deferred_wal_append
            .store(true, Ordering::Release);
        store.publish_prepared_group(vec![
            Request {
                prepare: Box::new(move |_, previous| {
                    assert!(previous.is_empty());
                    Ok(RowPublication {
                        replacements: Vec::new(),
                        inserts: vec![("keys".into(), row(2), None)],
                        deletes: vec![("keys".into(), original)],
                    })
                }),
                bytes: 0,
                result: Arc::clone(&first),
            },
            Request {
                prepare: Box::new(|_, previous| {
                    assert_eq!(previous.len(), 1, "earlier reservation is visible");
                    Err(StoreError::Io(std::io::Error::other("invalid request")))
                }),
                bytes: 0,
                result: Arc::clone(&invalid),
            },
            Request {
                prepare: Box::new(|_, previous| {
                    assert_eq!(previous.len(), 1, "rejected request has no plan");
                    Ok(RowPublication {
                        replacements: Vec::new(),
                        inserts: vec![("keys".into(), row(3), None)],
                        deletes: Vec::new(),
                    })
                }),
                bytes: 0,
                result: Arc::clone(&second),
            },
        ]);
        assert!(first.lock().as_ref().expect("first result").is_err());
        assert!(second.lock().as_ref().expect("second result").is_err());
        assert!(invalid
            .lock()
            .as_ref()
            .expect("invalid result")
            .as_ref()
            .expect_err("rejected")
            .contains("invalid request"));
        assert_eq!(store.get_collection("keys").expect("keys").count(), 1);
        assert_eq!(
            store
                .get("keys", original)
                .expect("restored source")
                .data
                .as_row()
                .expect("row")
                .columns,
            vec![Value::Integer(1)]
        );
        assert!(!UnifiedStore::deferred_store_wal_capture_active());
    }

    #[test]
    fn rejected_preparation_does_not_abort_valid_publications() {
        let store = UnifiedStore::new();
        let invalid = Arc::new(parking_lot::Mutex::new(None));
        let valid = Arc::new(parking_lot::Mutex::new(None));
        store.publish_prepared_group(vec![
            Request {
                prepare: Box::new(|_, _| Err(StoreError::Io(std::io::Error::other("rejected")))),
                bytes: 0,
                result: Arc::clone(&invalid),
            },
            Request {
                prepare: Box::new(|_, previous| {
                    assert!(previous.is_empty());
                    Ok(RowPublication {
                        replacements: Vec::new(),
                        inserts: vec![("keys".into(), row(7), None)],
                        deletes: Vec::new(),
                    })
                }),
                bytes: 0,
                result: Arc::clone(&valid),
            },
        ]);
        assert!(invalid.lock().as_ref().expect("invalid result").is_err());
        assert!(valid.lock().as_ref().expect("valid result").is_ok());
        assert_eq!(store.get_collection("keys").expect("keys").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn prepared_publications_obey_byte_limit_and_recover() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("prepared.rdb");
        let mut config =
            UnifiedStoreConfig::default().with_durability_mode(DurabilityMode::WalDurableGrouped);
        config.group_commit.max_wal_bytes = 1;
        let store = UnifiedStore::open_with_config(&path, config.clone()).expect("open");
        store.create_collection("keys").expect("collection");
        let results: Vec<_> = (0..4)
            .map(|_| Arc::new(parking_lot::Mutex::new(None)))
            .collect();
        let before = store.commit.as_ref().expect("coordinator").fsync_count();
        store.publish_prepared_group(
            results
                .iter()
                .enumerate()
                .map(|(key, result)| Request {
                    prepare: Box::new(move |_, _| {
                        Ok(RowPublication {
                            replacements: Vec::new(),
                            inserts: vec![("keys".into(), row(key), None)],
                            deletes: Vec::new(),
                        })
                    }),
                    bytes: 0,
                    result: Arc::clone(result),
                })
                .collect(),
        );
        assert_eq!(
            store.commit.as_ref().expect("coordinator").fsync_count() - before,
            4,
            "each oversized request must publish alone"
        );
        assert!(results
            .iter()
            .all(|result| result.lock().as_ref().expect("result").is_ok()));
        drop(store);
        for _ in 0..3 {
            let store = UnifiedStore::open_with_config(&path, config.clone()).expect("cold boot");
            assert_eq!(store.get_collection("keys").expect("keys").count(), 4);
            store.persist().expect("checkpoint");
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_publications_share_fsync_and_survive_checkpoint_and_cold_boot() {
        const WRITERS: usize = 8;
        for bounded in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("group.rdb");
            let mut config = UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::WalDurableGrouped);
            config.group_commit.window_ms = 10;
            config.group_commit.max_statements = WRITERS;
            if bounded {
                config.resident_memory_bytes_max = Some(64 * 1024 * 1024);
            }
            let store =
                Arc::new(UnifiedStore::open_with_config(&path, config.clone()).expect("open"));
            store.create_collection("keys").expect("collection");
            let before = store.commit.as_ref().expect("coordinator").fsync_count();
            let start = Arc::new(std::sync::Barrier::new(WRITERS));
            let writers: Vec<_> = (0..WRITERS)
                .map(|key| {
                    let store = Arc::clone(&store);
                    let start = Arc::clone(&start);
                    std::thread::spawn(move || {
                        start.wait();
                        store
                            .write_keyed_row("keys", row(key), None, None)
                            .expect("durable row")
                    })
                })
                .collect();
            let ids: Vec<_> = writers
                .into_iter()
                .map(|writer| writer.join().expect("writer"))
                .collect();
            let fsyncs = store.commit.as_ref().expect("coordinator").fsync_count() - before;
            assert!(fsyncs > 0 && fsyncs < WRITERS as u64, "{fsyncs} fsyncs");
            store.persist().expect("checkpoint");
            drop(store);
            for _ in 0..3 {
                let store =
                    UnifiedStore::open_with_config(&path, config.clone()).expect("cold boot");
                for (key, id) in ids.iter().enumerate() {
                    assert_eq!(
                        store
                            .try_get("keys", *id, usize::MAX)
                            .expect("read")
                            .expect("row")
                            .data
                            .as_row()
                            .expect("row data")
                            .columns,
                        row(key).data.as_row().expect("expected row").columns
                    );
                }
                store.persist().expect("repeat checkpoint");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publication_checkpoint_and_failed_replacement_recover() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("concurrent.rdb");
        let mut config =
            UnifiedStoreConfig::default().with_durability_mode(DurabilityMode::WalDurableGrouped);
        config.resident_memory_bytes_max = Some(64 * 1024 * 1024);
        let store = UnifiedStore::open_with_config(&path, config.clone()).expect("open");
        store.create_collection("keys").expect("keys");
        let ids = std::thread::scope(|scope| {
            let checkpoint = scope.spawn(|| {
                for _ in 0..4 {
                    store.persist().expect("concurrent checkpoint");
                }
            });
            let writers: Vec<_> = (0..8)
                .map(|key| {
                    let store = &store;
                    scope.spawn(move || {
                        store
                            .write_keyed_row("keys", row(key), None, None)
                            .expect("publish")
                    })
                })
                .collect();
            let ids: Vec<_> = writers
                .into_iter()
                .map(|writer| writer.join().expect("writer"))
                .collect();
            checkpoint.join().expect("checkpoint");
            ids
        });
        store
            .test_fail_deferred_wal_append
            .store(true, Ordering::Release);
        assert!(store
            .write_keyed_row("keys", row(99), None, Some(ids[0]))
            .is_err());
        store.persist().expect("checkpoint restored preimage");
        drop(store);
        for _ in 0..3 {
            let store = UnifiedStore::open_with_config(&path, config.clone()).expect("cold boot");
            for (key, id) in ids.iter().enumerate() {
                let value = store
                    .try_get("keys", *id, usize::MAX)
                    .expect("read")
                    .expect("row");
                assert_eq!(
                    value.data.as_row().expect("row").columns,
                    vec![Value::Integer(key as i64)]
                );
            }
            store.persist().expect("checkpoint");
        }
    }

    #[test]
    fn failed_compound_publication_restores_all_rows() {
        let store = UnifiedStore::new();
        let first = store.insert_auto("keys", row(1)).expect("first");
        let second = store.insert_auto("keys", row(2)).expect("second");
        store
            .test_fail_deferred_wal_append
            .store(true, Ordering::Release);
        assert!(store
            .publish_rows(RowPublication {
                replacements: Vec::new(),
                inserts: vec![("keys".into(), row(3), None)],
                deletes: vec![("keys".into(), first), ("keys".into(), second)],
            })
            .is_err());
        assert!(!UnifiedStore::deferred_store_wal_capture_active());
        assert_eq!(store.get_collection("keys").expect("keys").count(), 2);
        assert_eq!(
            store
                .get("keys", first)
                .expect("restored first")
                .data
                .as_row()
                .expect("row")
                .columns,
            row(1).data.as_row().expect("row").columns
        );
        assert_eq!(
            store
                .get("keys", second)
                .expect("restored second")
                .data
                .as_row()
                .expect("row")
                .columns,
            row(2).data.as_row().expect("row").columns
        );
    }
}
