//! Bounded physical table-history and aborted-queue reclamation; payloads stay borrowed.
use super::*;
use crate::storage::transaction::SnapshotManager;
use crate::storage::unified::manager::PhysicalScanCursor;

pub(crate) struct MvccReclaimCursor {
    pub(crate) collection: String,
    manager: Arc<SegmentManager>,
    scan: PhysicalScanCursor,
    invalidated: bool,
}

impl MvccReclaimCursor {
    pub(crate) fn finished(&self) -> bool {
        self.invalidated || self.scan.finished()
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
            invalidated: false,
        })
    }

    /// A busy checkpoint leaves the cursor untouched. Each successful tick
    /// inspects at most `scan_budget` positions and deletes at most
    /// `reclaim_budget` physical versions through the ordinary WAL-backed path.
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
        while scan_left > 0 && reclaim.len() < reclaim_budget && !cursor.finished() {
            let work = scan_left.min(reclaim_budget - reclaim.len());
            let mut work_left = work;
            let ids = manager.physical_scan_batch(&mut cursor.scan, &mut work_left);
            scan_left -= work - work_left;
            manager.for_each_id(&ids, |_, entity| {
                if !matches!(
                    entity.kind,
                    EntityKind::TableRow { .. } | EntityKind::QueueMessage { .. }
                ) {
                    return;
                }
                let aborted_insert = entity.xmin != 0 && snapshots.is_aborted(entity.xmin);
                let obsolete =
                    entity.xmax != 0 && entity.xmax < cutoff && !snapshots.is_aborted(entity.xmax);
                if aborted_insert || obsolete {
                    reclaim.push(entity.id);
                }
            });
        }
        Ok(self.delete_batch(&cursor.collection, &reclaim)?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

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
