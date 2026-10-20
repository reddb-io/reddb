//! Bounded physical table-history reclamation; payloads stay borrowed.
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
                if !matches!(entity.kind, EntityKind::TableRow { .. }) {
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
}
