//! Bounded physical table-history and aborted-queue reclamation.
use super::*;
use crate::storage::engine::BTreeCursor;
use crate::storage::transaction::SnapshotManager;
use crate::storage::unified::manager::PhysicalScanCursor;
use reddb_engine::storage::secondary_index_read::{SecondaryRangeCursor, SECONDARY_KEY_BYTES_MAX};

// These generation pins exist only within one fenced tick.
enum ColdInventory {
    Payloads(BTreeCursor),
    Visibility(SecondaryRangeCursor<Pager>),
}

pub(crate) struct MvccReclaimCursor {
    pub(crate) collection: String,
    manager: Arc<SegmentManager>,
    scan: PhysicalScanCursor,
    cold_after: Option<Vec<u8>>,
    cold_is_visibility: Option<bool>,
    cold_finished: bool,
    invalidated: bool,
}

impl MvccReclaimCursor {
    pub(crate) fn finished(&self) -> bool {
        self.invalidated || (self.scan.finished() && self.cold_finished)
    }
}

impl UnifiedStore {
    pub(crate) fn mvcc_reclaim_cursor(&self, collection: String) -> Option<MvccReclaimCursor> {
        let manager = self.get_collection(&collection)?;
        let scan = manager.physical_scan_cursor();
        Some(MvccReclaimCursor {
            collection,
            manager,
            scan,
            cold_after: None,
            cold_is_visibility: None,
            cold_finished: false,
            invalidated: false,
        })
    }

    /// Inventory at most `budget` physical positions for retention. The caller
    /// must recheck the current row and retention predicate before deletion.
    pub(crate) fn retention_scan_tick(
        &self,
        cursor: &mut MvccReclaimCursor,
        budget: usize,
    ) -> Result<(Vec<EntityId>, usize), StoreError> {
        let Some(_publication) = self.vault_publication_lock.try_lock() else {
            return Ok((Vec::new(), 0));
        };
        let Some(manager) = self.get_collection(&cursor.collection) else {
            cursor.invalidated = true;
            return Ok((Vec::new(), 0));
        };
        if !Arc::ptr_eq(&manager, &cursor.manager) {
            cursor.invalidated = true;
            return Ok((Vec::new(), 0));
        }
        let mut work_left = budget;
        let mut ids = Vec::with_capacity(budget.min(128));
        while work_left > 0 && !cursor.scan.finished() {
            ids.extend(manager.physical_scan_batch(&mut cursor.scan, &mut work_left));
        }
        if !manager.has_backing() {
            cursor.cold_finished = true;
        }
        if work_left > 0 && !cursor.cold_finished {
            let tree = self
                .btree_indices
                .read()
                .get(&cursor.collection)
                .cloned()
                .ok_or_else(|| {
                    StoreError::Serialization("retention base has no published tree".into())
                })?;
            let start = match cursor.cold_after.as_deref() {
                Some(after) => {
                    let raw: [u8; 8] = after.try_into().map_err(|_| {
                        StoreError::Serialization("invalid retention cursor identity".into())
                    })?;
                    let Some(next) = u64::from_be_bytes(raw).checked_add(1) else {
                        cursor.cold_finished = true;
                        ids.sort_unstable();
                        ids.dedup();
                        return Ok((ids, budget - work_left));
                    };
                    next.to_be_bytes()
                }
                None => 0_u64.to_be_bytes(),
            };
            let mut native = tree
                .cursor_seek(&start)
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            while work_left > 0 {
                let Some(key) = native
                    .peek_key()
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                else {
                    cursor.cold_finished = true;
                    break;
                };
                let raw: [u8; 8] = key.as_slice().try_into().map_err(|_| {
                    StoreError::Serialization("invalid retention physical identity".into())
                })?;
                native
                    .skip_next()
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                cursor.cold_after = Some(key);
                ids.push(EntityId::new(u64::from_be_bytes(raw)));
                work_left -= 1;
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok((ids, budget - work_left))
    }

    /// A busy checkpoint leaves the cursor untouched. Work is bounded by
    /// inspected positions, reclaimed versions and decoded bytes. Generation
    /// pins never survive the tick and force unrelated appends to copy history.
    pub(crate) fn reclaim_mvcc_tick(
        &self,
        cursor: &mut MvccReclaimCursor,
        snapshots: &SnapshotManager,
        cutoff: u64,
        scan_budget: usize,
        reclaim_budget: usize,
    ) -> Result<usize, StoreError> {
        let Some(_publication) = self.vault_publication_lock.try_lock() else {
            return Ok(0);
        };
        let Some(manager) = self.get_collection(&cursor.collection) else {
            cursor.invalidated = true;
            return Ok(0);
        };
        if !Arc::ptr_eq(&manager, &cursor.manager) {
            cursor.invalidated = true;
            return Ok(0);
        }
        let mut scan_left = scan_budget;
        let mut reclaim = Vec::with_capacity(reclaim_budget);
        while scan_left > 0 && reclaim.len() < reclaim_budget && !cursor.scan.finished() {
            let work = scan_left.min(reclaim_budget - reclaim.len());
            let mut work_left = work;
            let ids = manager.physical_scan_batch(&mut cursor.scan, &mut work_left);
            scan_left -= work - work_left;
            manager.for_each_id(&ids, |_, entity| {
                if matches!(
                    entity.kind,
                    EntityKind::TableRow { .. } | EntityKind::QueueMessage { .. }
                ) && reclaimable(snapshots, cutoff, entity.xmin, entity.xmax)
                {
                    reclaim.push(entity.id);
                }
            });
        }
        if !manager.has_backing() {
            cursor.cold_finished = true;
        }
        if scan_left > 0 && reclaim.len() < reclaim_budget && !cursor.cold_finished {
            let previous_visibility = cursor.cold_is_visibility;
            let after = (previous_visibility == Some(true))
                .then_some(cursor.cold_after.as_deref())
                .flatten();
            let mut inventory = if let Some(index) =
                self.count_visibility_cursor(&cursor.collection, &manager, after)?
            {
                cursor.cold_is_visibility = Some(true);
                ColdInventory::Visibility(index)
            } else {
                let after = (previous_visibility == Some(false))
                    .then_some(cursor.cold_after.as_deref())
                    .flatten();
                let tree = self
                    .btree_indices
                    .read()
                    .get(&cursor.collection)
                    .cloned()
                    .ok_or_else(|| {
                        StoreError::Serialization("native GC base has no published tree".into())
                    })?;
                let mut native = tree
                    .cursor_seek(after.unwrap_or_default())
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                if let Some(after) = after {
                    if native
                        .peek_key()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                        .as_deref()
                        == Some(after)
                    {
                        native
                            .skip_next()
                            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                    }
                }
                cursor.cold_is_visibility = Some(false);
                ColdInventory::Payloads(native)
            };
            let read_limit = self.mvcc_reclaim_read_limit();
            let mut decoded_bytes = 0_usize;
            let mut projection = [0; SECONDARY_KEY_BYTES_MAX];
            while scan_left > 0 && reclaim.len() < reclaim_budget && decoded_bytes < read_limit {
                scan_left -= 1;
                let (id, candidate) = match &mut inventory {
                    ColdInventory::Payloads(native) => {
                        let Some(key) = native
                            .peek_key()
                            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                        else {
                            cursor.cold_finished = true;
                            break;
                        };
                        let raw: [u8; 8] = key.as_slice().try_into().map_err(|_| {
                            StoreError::Serialization("invalid GC physical identity".into())
                        })?;
                        native
                            .skip_next()
                            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                        cursor.cold_after = Some(key);
                        (EntityId::new(u64::from_be_bytes(raw)), true)
                    }
                    ColdInventory::Visibility(index) => {
                        let Some((length, id)) = index.next_posting(&mut projection)? else {
                            cursor.cold_finished = true;
                            break;
                        };
                        if length != 17 || projection[16] > 1 {
                            return Err(StoreError::Serialization(
                                "invalid GC visibility projection".into(),
                            ));
                        }
                        let position = cursor
                            .cold_after
                            .get_or_insert_with(|| Vec::with_capacity(25));
                        position.clear();
                        position.extend_from_slice(&projection[..length]);
                        position.extend_from_slice(&id.raw().to_be_bytes());
                        let xmin = u64::from_be_bytes(
                            projection[..8].try_into().expect("eight-byte xmin"),
                        );
                        let xmax = u64::from_be_bytes(
                            projection[8..16].try_into().expect("eight-byte xmax"),
                        );
                        (id, reclaimable(snapshots, cutoff, xmin, xmax))
                    }
                };
                if !candidate {
                    continue;
                }
                // Positions inventory a possibly superseded generation. Only
                // the current base/overlay can authorize physical reclamation.
                if let Some(entity) = manager.try_get(id, read_limit)? {
                    decoded_bytes = decoded_bytes.saturating_add(
                        crate::storage::unified::memory_size::entity_bytes(&entity),
                    );
                    if matches!(
                        entity.kind,
                        EntityKind::TableRow { .. } | EntityKind::QueueMessage { .. }
                    ) && reclaimable(snapshots, cutoff, entity.xmin, entity.xmax)
                    {
                        reclaim.push(id);
                    }
                }
            }
        } // Release primary and visibility generation pins before publication.
        reclaim.sort_unstable();
        reclaim.dedup();
        Ok(self.delete_batch(&cursor.collection, &reclaim)?.len())
    }

    pub(crate) fn mvcc_reclaim_read_limit(&self) -> usize {
        usize::try_from(self.config.resident_memory_bytes_max.unwrap_or(0) / 8)
            .unwrap_or(usize::MAX)
            .clamp(16 * 1024, 16 * 1024 * 1024)
    }
}

fn reclaimable(snapshots: &SnapshotManager, cutoff: u64, xmin: u64, xmax: u64) -> bool {
    (xmin != 0 && snapshots.is_aborted(xmin))
        || (xmax != 0 && xmax < cutoff && !snapshots.is_aborted(xmax))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    #[test]
    fn cold_gc_rechecks_overlays_and_preserves_active_creators() {
        for projected in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("gc.rdb");
            let mut config = UnifiedStoreConfig::default().with_page_cache_slots(8);
            config.resident_memory_bytes_max = Some(4 * 1024 * 1024);
            let store = UnifiedStore::open_with_config(&path, config.clone()).unwrap();
            store.create_collection("rows").unwrap();
            if projected {
                store.register_count_index("rows").unwrap();
            }
            let snapshots = SnapshotManager::new();
            let active = snapshots.begin();
            let aborted = snapshots.begin();
            snapshots.rollback(aborted);
            for id in 1..=5 {
                let mut entity = UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::Integer(id as i64)],
                );
                match id {
                    1 | 2 | 5 => entity.xmax = 5,
                    3 => entity.xmin = active,
                    4 => entity.xmin = aborted,
                    _ => unreachable!(),
                }
                store.insert("rows", entity).unwrap();
            }
            store.persist().unwrap();
            let manager = store.get_collection("rows").unwrap();
            assert!(manager.has_backing());
            let mut revived = manager
                .try_get(EntityId::new(2), usize::MAX)
                .unwrap()
                .unwrap();
            revived.xmax = 0;
            manager.update(revived).unwrap();
            let mut cursor = store.mvcc_reclaim_cursor("rows".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..32 {
                let tick = store
                    .reclaim_mvcc_tick(&mut cursor, &snapshots, 10, 1, 1)
                    .unwrap();
                assert!(tick <= 1);
                reclaimed += tick;
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished());
            assert_eq!(reclaimed, 3);
            assert!(manager
                .try_get(EntityId::new(1), usize::MAX)
                .unwrap()
                .is_none());
            assert!(manager
                .try_get(EntityId::new(4), usize::MAX)
                .unwrap()
                .is_none());
            assert!(
                manager
                    .try_get(EntityId::new(5), usize::MAX)
                    .unwrap()
                    .is_none(),
                "resume must preserve IDs sharing the same projected XIDs"
            );
            for id in [2, 3] {
                assert!(manager
                    .try_get(EntityId::new(id), usize::MAX)
                    .unwrap()
                    .is_some());
            }
            drop(cursor);
            store.persist().unwrap();
            drop(manager);
            drop(store);
            let reopened = UnifiedStore::open_with_config(&path, config).unwrap();
            assert_eq!(reopened.get_collection("rows").unwrap().count(), 2);
        }
    }

    fn fixture() -> RedDBRuntime {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime opens");
        runtime
            .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
            .expect("disable automatic ticks");
        runtime
            .execute_query("CREATE TABLE reclaim_rows (id INTEGER, n INTEGER)")
            .expect("create table");
        runtime
    }

    #[test]
    fn collection_replacement_finishes_its_stale_cursor() {
        let runtime = fixture();
        runtime
            .execute_query("INSERT INTO reclaim_rows (id, n) VALUES (1, 0)")
            .expect("seed row");
        let store = runtime.db().store();
        let mut cursor = store
            .mvcc_reclaim_cursor("reclaim_rows".to_string())
            .expect("open cursor");
        runtime
            .execute_query("DROP TABLE reclaim_rows")
            .expect("drop source");
        runtime
            .execute_query("CREATE TABLE reclaim_rows (id INTEGER, n INTEGER)")
            .expect("replace source");
        runtime
            .execute_query("INSERT INTO reclaim_rows (id, n) VALUES (2, 1)")
            .expect("seed replacement");
        let reclaimed = store
            .reclaim_mvcc_tick(
                &mut cursor,
                &runtime.snapshot_manager(),
                runtime.mvcc_vacuum_cutoff_xid(),
                4096,
                256,
            )
            .expect("invalidate stale cursor");
        assert_eq!(reclaimed, 0);
        assert!(
            cursor.finished(),
            "collector would remain stuck on the replaced collection"
        );
        assert_eq!(
            runtime
                .execute_query("SELECT n FROM reclaim_rows WHERE id = 2")
                .expect("replacement survives")
                .result
                .records
                .len(),
            1
        );
    }

    #[test]
    fn reclamation_resumes_without_exceeding_its_delete_budget() {
        let runtime = fixture();
        let values = (0..128)
            .map(|id| format!("({id}, 0)"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!("INSERT INTO reclaim_rows (id, n) VALUES {values}"))
            .expect("seed rows");
        runtime
            .execute_query("UPDATE reclaim_rows SET n = 1")
            .expect("create history");
        let store = runtime.db().store();
        let mut cursor = store
            .mvcc_reclaim_cursor("reclaim_rows".to_string())
            .expect("open cursor");
        let mut total = 0;
        for _ in 0..64 {
            let reclaimed = store
                .reclaim_mvcc_tick(
                    &mut cursor,
                    &runtime.snapshot_manager(),
                    runtime.mvcc_vacuum_cutoff_xid(),
                    32,
                    8,
                )
                .expect("bounded tick succeeds");
            assert!(reclaimed <= 8);
            total += reclaimed;
            if cursor.finished() {
                break;
            }
        }
        assert!(cursor.finished());
        assert_eq!(total, 128);
        assert_eq!(
            runtime
                .execute_query("SELECT COUNT(*) AS n FROM reclaim_rows WHERE n = 1")
                .expect("live rows survive")
                .result
                .records[0]
                .get("n")
                .expect("count exists")
                .to_string(),
            "128"
        );
    }

    #[test]
    fn aborted_event_messages_are_reclaimed_in_bounded_ticks_without_touching_readers() {
        use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
        let runtime = fixture();
        runtime.execute_query("CREATE TABLE reclaim_event_source (id INT, payload TEXT) WITH EVENTS (DELETE) TO reclaim_events").expect("event source");
        let payload = "x".repeat(8192);
        runtime.execute_query(&format!("INSERT INTO reclaim_event_source (id, payload) VALUES (1, '{payload}'), (2, '{payload}'), (3, '{payload}')")).expect("source rows");
        runtime
            .enqueue_event_payload("reclaim_events", Value::text("committed"))
            .expect("committed message");
        set_current_connection_id(873);
        runtime.execute_query("BEGIN").expect("pending producer");
        runtime
            .enqueue_event_payload("reclaim_events", Value::text("pending"))
            .expect("pending message");
        clear_current_connection_id();
        let store = runtime.db().store();
        let queue = store.get_collection("reclaim_events").expect("outbox");
        let drain = || {
            let mut cursor = store
                .mvcc_reclaim_cursor("reclaim_events".into())
                .expect("queue cursor");
            let mut total = 0;
            for _ in 0..128 {
                let reclaimed = store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        2,
                        1,
                    )
                    .expect("paced reclaim");
                assert!(reclaimed <= 1, "one tick exceeded its reclaim allowance");
                total += reclaimed;
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished(), "bounded scan failed to finish");
            total
        };
        for cycle in 0..100 {
            store
                .test_fail_deferred_wal_append
                .store(true, Ordering::SeqCst);
            let error = runtime
                .execute_query("DELETE FROM reclaim_event_source")
                .expect_err("injected failed DELETE");
            assert!(error
                .to_string()
                .contains("injected deferred WAL append failure"));
            assert_eq!(drain(), 3, "aborted messages retained at cycle {cycle}");
            assert_eq!(
                queue.count(),
                2,
                "GC must keep committed and active creators"
            );
            queue.force_seal().expect("seal maintenance fixture");
            for _ in 0..16 {
                if !queue.pressure_consolidation_tick() {
                    break;
                }
            }
            assert!(
                queue.resident_bytes() < 64 * 1024,
                "failed outbox payloads accumulated at cycle {cycle}: {} bytes",
                queue.resident_bytes()
            );
            assert_eq!(
                runtime
                    .execute_query("QUEUE LEN reclaim_events")
                    .expect("visible message count")
                    .result
                    .records[0]
                    .get("len"),
                Some(&Value::UnsignedInteger(1))
            );
        }
        set_current_connection_id(873);
        runtime
            .execute_query("ROLLBACK")
            .expect("abort pending message");
        clear_current_connection_id();
        assert_eq!(
            drain(),
            1,
            "only the newly aborted producer becomes reclaimable"
        );
        assert_eq!(queue.count(), 1);
        assert_eq!(
            runtime
                .execute_query("DELETE FROM reclaim_event_source")
                .expect("successful source retry")
                .affected_rows,
            3
        );
        assert_eq!(drain(), 0, "committed event messages are not garbage");
        assert_eq!(
            runtime
                .execute_query("QUEUE LEN reclaim_events")
                .expect("committed event count")
                .result
                .records[0]
                .get("len"),
            Some(&Value::UnsignedInteger(4))
        );
    }

    #[test]
    fn queue_reclamation_preserves_old_readers_and_unfinished_deletions() {
        let runtime = fixture();
        runtime
            .execute_query("CREATE QUEUE retained_queue")
            .expect("queue");
        let store = runtime.db().store();
        let queue = store
            .get_collection("retained_queue")
            .expect("message store");
        let snapshots = runtime.snapshot_manager();
        let old_reader = snapshots.begin();
        let committed_delete = snapshots.begin();
        snapshots.commit(committed_delete);
        let pending_delete = snapshots.begin();
        let aborted_delete = snapshots.begin();
        snapshots.rollback(aborted_delete);
        for stamp in [0, committed_delete, pending_delete, aborted_delete] {
            let id = runtime
                .enqueue_event_payload("retained_queue", Value::text("retained"))
                .expect("message");
            let mut entity = queue.get(id).expect("message preimage");
            entity.set_xmax(stamp);
            queue.update(entity).expect("retirement stamp");
        }
        let reclaim = || {
            let mut cursor = store
                .mvcc_reclaim_cursor("retained_queue".into())
                .expect("cursor");
            let mut total = 0;
            for _ in 0..32 {
                let count = store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &snapshots,
                        runtime.mvcc_vacuum_cutoff_xid(),
                        2,
                        1,
                    )
                    .expect("reclaim");
                assert!(count <= 1);
                total += count;
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished());
            total
        };
        assert_eq!(
            reclaim(),
            0,
            "the old reader still needs the committed preimage"
        );
        assert_eq!(queue.count(), 4);
        snapshots.rollback(old_reader);
        assert_eq!(
            reclaim(),
            1,
            "only the committed, obsolete deletion is reclaimable"
        );
        assert_eq!(queue.count(), 3);
        snapshots.rollback(pending_delete);
        assert_eq!(
            reclaim(),
            0,
            "rolled-back deletions must preserve their messages"
        );
        assert_eq!(
            runtime
                .execute_query("QUEUE LEN retained_queue")
                .expect("retained messages")
                .result
                .records[0]
                .get("len"),
            Some(&Value::UnsignedInteger(3))
        );
    }
}
