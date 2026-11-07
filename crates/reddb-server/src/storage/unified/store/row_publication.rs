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
    pub deletes: Vec<(String, EntityId)>,
}

struct Request {
    rows: RowPublication,
    bytes: u64,
    result: Arc<parking_lot::Mutex<Option<Result<(), String>>>>,
}

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
        // A reentrant caller must not wait for another thread to take its lock.
        if self.commit.is_none()
            || self.config.durability_mode != DurabilityMode::WalDurableGrouped
            || self.vault_publication_lock.is_owned_by_current_thread()
        {
            return self.publish_row_group(vec![rows]);
        }
        let bytes = rows
            .inserts
            .iter()
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
            );
        let result = Arc::new(parking_lot::Mutex::new(None));
        let queue = &self.row_publications;
        let mut state = queue.state.lock();
        state.requests.push_back(Request {
            rows,
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
            let (rows, results): (Vec<_>, Vec<_>) = requests
                .into_iter()
                .map(|request| (request.rows, request.result))
                .unzip();
            let published = self
                .publish_row_group(rows)
                .map_err(|error| error.to_string());
            state = queue.state.lock();
            for result in results {
                *result.lock() = Some(published.clone());
            }
            state.active = false;
            queue.changed.notify_all();
        }
    }

    fn publish_row_group(&self, rows: Vec<RowPublication>) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
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
            for (collection, id) in &request.deletes {
                if !deleted_ids.insert((collection.as_str(), *id)) {
                    return Err(StoreError::Io(std::io::Error::other(
                        "overlapping row publications",
                    )));
                }
                let entity = self
                    .try_get(collection, *id, usize::MAX)?
                    .ok_or(StoreError::EntityNotFound(*id))?;
                let metadata = self.try_get_metadata(collection, *id, usize::MAX)?;
                before.push((
                    collection.clone(),
                    *id,
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
        let actions = capture.finish();
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
            self.finish_paged_write(actions.actions)
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

    #[cfg(unix)]
    #[test]
    fn native_publications_share_fsync_and_survive_checkpoint_and_cold_boot() {
        const WRITERS: usize = 8;
        for bounded in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("group.rdb");
            let mut config = UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::WalDurableGrouped);
            config.group_commit.window_ms = 200;
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
                            .data,
                        row(key).data
                    );
                }
                store.persist().expect("repeat checkpoint");
            }
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
                inserts: vec![("keys".into(), row(3), None)],
                deletes: vec![("keys".into(), first), ("keys".into(), second)],
            })
            .is_err());
        assert!(!UnifiedStore::deferred_store_wal_capture_active());
        assert_eq!(store.get_collection("keys").expect("keys").count(), 2);
        assert_eq!(
            store.get("keys", first).expect("restored first").data,
            row(1).data
        );
        assert_eq!(
            store.get("keys", second).expect("restored second").data,
            row(2).data
        );
    }
}
