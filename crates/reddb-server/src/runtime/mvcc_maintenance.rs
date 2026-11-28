//! Paced table-history maintenance for standalone embedded runtimes.
use std::time::{Duration, Instant};

use super::RedDBRuntime;
use crate::storage::profile::DeployProfile;
use crate::storage::unified::store::MvccReclaimCursor;
use crate::storage::StoreError;

#[derive(Default)]
pub(super) struct MvccMaintenance {
    current: Option<MvccReclaimCursor>,
    after_collection: Option<String>,
    retry_after: Option<Instant>,
    last_warning: Option<Instant>,
}

impl RedDBRuntime {
    /// Foreground pressure and the periodic worker share one paced cursor.
    /// Never wait behind another collector while a mutation holds row locks.
    pub(super) fn try_reclaim_history_tick(&self) -> bool {
        let Some(mut state) = self.inner.mvcc_maintenance.try_lock() else {
            return false;
        };
        self.reclaim_history_tick(&mut state)
    }

    fn reclaim_history_tick(&self, state: &mut MvccMaintenance) -> bool {
        let options = self.inner.db.options();
        // Replicated profiles need a distributed reader/replica low-water
        // mark before local physical history can be reclaimed automatically.
        if options.read_only
            || options.storage_profile.deploy_profile != DeployProfile::Embedded
            || options.replication.role != crate::replication::ReplicationRole::Standalone
            || self.inner.lifecycle.phase() != super::lifecycle::Phase::Ready
            || !self.config_bool("runtime.mvcc.autovacuum.enabled", true)
        {
            state.current = None;
            return false;
        }
        // A collector must not append physical GC to an unrelated, possibly
        // aborted DML event capture. The periodic worker can resume this cursor.
        if crate::storage::UnifiedStore::deferred_store_wal_capture_active() {
            return false;
        }
        if state.retry_after.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        let store = self.inner.db.store();
        if state
            .current
            .as_ref()
            .is_some_and(|cursor| store.get_collection(&cursor.collection).is_none())
        {
            state.current = None;
        }
        if state.current.is_none() {
            let mut collections = store.list_collections();
            collections.sort_unstable();
            if collections.is_empty() {
                return false;
            }
            let index = state.after_collection.as_ref().map_or(0, |after| {
                collections.partition_point(|name| name <= after) % collections.len()
            });
            state.current = store.mvcc_reclaim_cursor(collections[index].clone());
        }
        let Some(cursor) = state.current.as_mut() else {
            return false;
        };
        // Native GC retains one leaf and decodes one current row at a time.
        // Charge encoded/decoded workspace before taking the publication fence.
        let _workspace = if store.config().resident_memory_bytes_max.is_some() {
            let Some(reservation) =
                self.try_reserve_query_buffer(store.mvcc_reclaim_read_limit().saturating_mul(2))
            else {
                return false;
            };
            Some(reservation)
        } else {
            None
        };
        let scan_budget = self
            .config_u64("runtime.mvcc.autovacuum.scan_rows_per_tick", 4096)
            .clamp(1, 4096) as usize;
        let reclaim_budget = self
            .config_u64("runtime.mvcc.autovacuum.reclaim_rows_per_tick", 256)
            .clamp(1, 256) as usize;
        let result = store.reclaim_mvcc_tick_with(
            cursor,
            &self.snapshot_manager(),
            |collection| {
                self.mvcc_vacuum_cutoff_xid_for(collection)
                    .map_err(|error| StoreError::Serialization(error.to_string()))
            },
            scan_budget,
            reclaim_budget,
        );
        match result {
            Ok(reclaimed) => {
                state.retry_after = None;
                if reclaimed > 0 {
                    tracing::debug!(collection = %cursor.collection, reclaimed_versions = reclaimed,
                        "paced MVCC history reclamation");
                }
                if cursor.finished() {
                    state.after_collection = Some(cursor.collection.clone());
                    state.current = None;
                }
                true
            }
            Err(error) => {
                state.retry_after = Some(Instant::now() + Duration::from_secs(5));
                if state
                    .last_warning
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
                {
                    tracing::warn!(%error, "MVCC history reclamation deferred");
                    state.last_warning = Some(Instant::now());
                }
                // An unsuccessful WAL-backed batch is not recorded as progress.
                state.current = None;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{RedDBOptions, RedDBRuntime};

    /// Reclaim took 512 rows a second. Over a large backlog it changed more
    /// rows between two checkpoints than a delta takes, and every checkpoint
    /// copied the collection again with the writers waiting.
    #[test]
    fn reclaim_takes_a_share_of_each_checkpoint_and_the_checkpoint_stays_a_delta() {
        use crate::storage::unified::store::{
            take_republished_collections, MAINTENANCE_PACED_FROM_ROWS,
            MAINTENANCE_ROWS_PER_CHECKPOINT,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("paced.rdb"))
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE paced_rows (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        // A large table whose every row is dead: two shares above the size
        // from which a table is paced.
        let rows = MAINTENANCE_PACED_FROM_ROWS as usize + 2 * MAINTENANCE_ROWS_PER_CHECKPOINT + 500;
        for start in (0..rows).step_by(512) {
            let values = (start..(start + 512).min(rows))
                .map(|id| format!("({id}, 'payload-{id}')"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO paced_rows (id, payload) VALUES {values}"
                ))
                .unwrap();
        }
        runtime.checkpoint().unwrap();
        runtime
            .execute_query("DELETE FROM paced_rows WHERE id >= 0")
            .unwrap();
        // The statement changed every row; this checkpoint is the baseline.
        runtime.checkpoint().unwrap();

        let store = runtime.db().store();
        let manager = store.get_collection("paced_rows").unwrap();
        let mut total = 0;
        let mut paced_periods = 0;
        for period in 1.. {
            assert!(period <= 12, "reclaim stopped at {total} of {rows} rows");
            // Large for the whole period: a table that shrinks below the
            // size from which it is paced is reclaimed at once from there.
            let large = manager.count() as u64
                > MAINTENANCE_PACED_FROM_ROWS + MAINTENANCE_ROWS_PER_CHECKPOINT as u64;
            let mut cursor = store.mvcc_reclaim_cursor("paced_rows".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..1_000 {
                reclaimed += store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        4096,
                        256,
                    )
                    .unwrap();
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished(), "the pass over the table did not end");
            drop(cursor);
            total += reclaimed;
            take_republished_collections();
            runtime.checkpoint().unwrap();
            if large {
                paced_periods += 1;
                assert_eq!(
                    reclaimed, MAINTENANCE_ROWS_PER_CHECKPOINT,
                    "rows reclaimed from the large table before one checkpoint"
                );
                let copied: Vec<_> = take_republished_collections()
                    .into_iter()
                    .filter(|collection| collection.starts_with("paced_rows:"))
                    .collect();
                assert!(
                    copied.is_empty(),
                    "after {reclaimed} reclaimed rows the checkpoint copied {copied:?}"
                );
            }
            if total == rows {
                break;
            }
        }
        assert!(paced_periods >= 2, "{paced_periods} paced periods");
        let result = runtime
            .execute_query("SELECT COUNT(*) AS n FROM paced_rows")
            .unwrap();
        assert_eq!(
            result.result.records[0].get("n").unwrap().display_string(),
            "0"
        );
    }

    /// A load passes the rows a delta takes long before a checkpoint is due
    /// by time or memory, and that checkpoint then copied the whole table with
    /// every writer waiting.
    #[test]
    fn a_load_into_a_large_table_checkpoints_by_volume_and_never_copies_it() {
        use crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX;
        use crate::storage::unified::store::{
            take_republished_collections, DELTA_CHECKPOINT_ROWS, MAINTENANCE_PACED_FROM_ROWS,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("loaded.rdb"))
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE loaded (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        let load = |from: usize, to: usize| {
            // Statements smaller than the trigger's margin, as a client sends them.
            for start in (from..to).step_by(DELTA_CHECKPOINT_ROWS / 8) {
                let values = (start..(start + DELTA_CHECKPOINT_ROWS / 8).min(to))
                    .map(|id| format!("({id}, 'payload-{id}')"))
                    .collect::<Vec<_>>()
                    .join(",");
                runtime
                    .execute_query(&format!("INSERT INTO loaded (id, payload) VALUES {values}"))
                    .unwrap();
            }
        };
        let large = MAINTENANCE_PACED_FROM_ROWS as usize + 1_000;
        load(0, large);
        runtime.checkpoint().unwrap();
        take_republished_collections();

        // Three deltas' worth of new rows.
        let total = large + 3 * CHECKPOINT_MUTATIONS_MAX;
        load(large, total);
        runtime.checkpoint().unwrap();
        let copied: Vec<_> = take_republished_collections()
            .into_iter()
            .filter(|collection| collection.starts_with("loaded:"))
            .collect();
        assert!(copied.is_empty(), "the load copied the table: {copied:?}");
        let result = runtime
            .execute_query("SELECT COUNT(*) AS n FROM loaded")
            .unwrap();
        assert_eq!(
            result.result.records[0].get("n").unwrap().display_string(),
            total.to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn constant_live_data_churn_reuses_pages_after_cold_gc() {
        use reddb_types::Value;
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("churn.rdb");
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        // Scheduling is part of this fixture: no sleeps or races against the
        // periodic worker. Every cycle drives the same bounded production GC.
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE churn_rows (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        let payload = |generation: usize, id: usize| {
            let mut state = (generation * 32 + id) as u64 + 1;
            // Exercise small inline, near-inline and overflow values in every
            // fixed cycle without making CI depend on bulk-throughput speed.
            let bytes = [180, 4 * 1024, 64 * 1024][id % 3];
            (0..bytes)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (b'a' + (state % 26) as u8) as char
                })
                .collect::<String>()
        };
        let allocated = || {
            [&path, &reddb_file::layout::unified_wal_path(&path)]
                .iter()
                .map(|p| std::fs::metadata(p).map_or(0, |m| m.blocks() * 512))
                .sum::<u64>()
        };
        let mut warmup_allocated = 0;
        let mut peak_after_warmup = 0;
        for generation in 0..120 {
            for start in (0..32).step_by(8) {
                runtime
                    .execute_query(&format!(
                        "DELETE FROM churn_rows WHERE id >= {start} AND id < {}",
                        start + 8,
                    ))
                    .unwrap();
            }
            for start in (0..32).step_by(8) {
                let values = (start..start + 8)
                    .map(|id| format!("({id}, '{}')", payload(generation, id)))
                    .collect::<Vec<_>>()
                    .join(",");
                runtime
                    .execute_query(&format!(
                        "INSERT INTO churn_rows (id, payload) VALUES {values}"
                    ))
                    .unwrap();
            }
            // Force the regression's hard case: obsolete versions are cold,
            // and the cleanup must find them without a resident-only scan.
            runtime.checkpoint().unwrap();
            let store = runtime.db().store();
            let mut cursor = store.mvcc_reclaim_cursor("churn_rows".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..128 {
                let count = store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        64,
                        8,
                    )
                    .unwrap();
                assert!(count <= 8, "bounded deletion work");
                reclaimed += count;
                if cursor.finished() {
                    break;
                }
            }
            assert!(
                cursor.finished(),
                "bounded sweep must converge at cycle {generation}"
            );
            assert_eq!(
                reclaimed,
                if generation == 0 { 0 } else { 32 },
                "cycle {generation}"
            );
            drop(cursor);
            runtime.checkpoint().unwrap();
            let result = runtime
                .execute_query("SELECT COUNT(*) AS n FROM churn_rows")
                .unwrap();
            assert_eq!(
                result.result.records[0].get("n").unwrap().display_string(),
                "32"
            );
            if generation == 19 {
                warmup_allocated = allocated();
            }
            if generation >= 20 {
                peak_after_warmup = peak_after_warmup.max(allocated());
            }
            if generation % 20 == 19 {
                eprintln!(
                    "churn_cycles={} allocated_bytes={} reclaimed_versions={reclaimed}",
                    generation + 1,
                    allocated()
                );
            }
        }
        let growth = peak_after_warmup.saturating_sub(warmup_allocated);
        eprintln!("churn_cycles=120 warmup_allocated={warmup_allocated} peak_after_warmup={peak_after_warmup} growth_bytes={growth}");
        assert!(
            growth <= 64 * 1024 * 1024,
            "storage grew {growth} bytes with constant live data"
        );
        drop(runtime);
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        for id in 0..32 {
            let result = runtime
                .execute_query(&format!("SELECT payload FROM churn_rows WHERE id = {id}"))
                .unwrap();
            assert_eq!(result.result.records.len(), 1);
            assert_eq!(
                result.result.records[0].get("payload"),
                Some(&Value::text(payload(119, id).as_str()))
            );
        }
    }

    /// #2895: heartbeat-style `UPDATE ... WHERE id = $1` on a TEXT primary
    /// key must not retain every superseded version, and the primary-key
    /// lookup and a full scan must agree on exactly one live row.
    #[test]
    fn repeated_pk_updates_reclaim_superseded_versions() {
        use reddb_types::Value;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("heartbeat.rdb");
        let open = || {
            let mut runtime = RedDBRuntime::with_options(
                RedDBOptions::persistent(&path)
                    .with_memory_budget(32 * 1024 * 1024)
                    .with_bounded_disk_residency(true),
            )
            .unwrap();
            drop(runtime.background_tasks.take());
            runtime
        };
        let mut runtime = open();
        // Control stores create their schema through applied migrations,
        // and every applied migration records a VCS commit.
        runtime
            .execute_query(
                "CREATE MIGRATION heartbeats_v1 AS CREATE TABLE heartbeats \
                 (id TEXT PRIMARY KEY, observed_at INTEGER, owner TEXT)",
            )
            .unwrap();
        runtime
            .execute_query("APPLY MIGRATION heartbeats_v1")
            .unwrap();
        runtime
            .execute_query(
                "INSERT INTO heartbeats (id, observed_at, owner) VALUES ('node-a', 0, 'w0')",
            )
            .unwrap();
        let physical = |runtime: &RedDBRuntime| {
            runtime
                .db()
                .store()
                .get_collection("heartbeats")
                .unwrap()
                .count()
        };
        let reported_entities = |runtime: &RedDBRuntime| {
            let result = runtime
                .execute_query("SELECT entities FROM red.collections WHERE name = 'heartbeats'")
                .unwrap();
            result.result.records[0]
                .get("entities")
                .unwrap()
                .display_string()
                .parse::<usize>()
                .unwrap()
        };
        // Drive the same paced collector the maintenance thread runs.
        let collect = |runtime: &RedDBRuntime| {
            for _ in 0..16 {
                runtime.try_reclaim_history_tick();
            }
        };
        const UPDATES: usize = 2000;
        let mut peak = 0;
        for generation in 1..=UPDATES {
            runtime
                .execute_query_with_params(
                    "UPDATE heartbeats SET observed_at = $1, owner = $2 WHERE id = $3",
                    &[
                        Value::Integer(generation as i64),
                        Value::text(format!("w{generation}")),
                        Value::text("node-a"),
                    ],
                )
                .unwrap();
            collect(&runtime);
            if generation % 500 == 0 {
                runtime.checkpoint().unwrap();
                collect(&runtime);
            }
            if generation == UPDATES / 2 {
                // Restarts rehydrate persisted commit horizons.
                drop(runtime);
                runtime = open();
            }
            peak = peak.max(physical(&runtime));
            assert!(
                peak <= 64,
                "superseded versions accumulated after {generation} updates: \
                 {peak} physical versions for one live row"
            );
        }
        runtime.checkpoint().unwrap();
        collect(&runtime);
        let count = runtime
            .execute_query("SELECT COUNT(*) AS n FROM heartbeats")
            .unwrap();
        assert_eq!(
            count.result.records[0].get("n").unwrap().display_string(),
            "1"
        );
        let retained = reported_entities(&runtime);
        eprintln!(
            "updates={UPDATES} peak_physical={peak} retained_physical={} reported_entities={retained}",
            physical(&runtime)
        );
        assert!(
            retained <= 4,
            "SHOW COLLECTIONS reports {retained} entities for one live row"
        );
        let point = runtime
            .execute_query_with_params(
                "SELECT id, observed_at FROM heartbeats WHERE id = $1",
                &[Value::text("node-a")],
            )
            .unwrap();
        assert_eq!(point.result.records.len(), 1, "PK lookup");
        assert_eq!(
            point.result.records[0].get("observed_at"),
            Some(&Value::Integer(UPDATES as i64))
        );
        let scan = runtime
            .execute_query("SELECT id, observed_at FROM heartbeats")
            .unwrap();
        assert_eq!(scan.result.records.len(), 1, "full scan");
        drop(runtime);
        let runtime = open();
        let point = runtime
            .execute_query("SELECT id, observed_at FROM heartbeats WHERE id = 'node-a'")
            .unwrap();
        assert_eq!(point.result.records.len(), 1, "PK lookup after reopen");
        let scan = runtime
            .execute_query("SELECT id, observed_at FROM heartbeats")
            .unwrap();
        assert_eq!(scan.result.records.len(), 1, "full scan after reopen");
    }

    /// #2895: a VCS commit pin protects only history that `AS OF` can read.
    /// Open snapshots still protect every collection.
    /// #2895: after heavy churn of one key, an indexed `WHERE id = $1` reads
    /// O(1) entries instead of every retained version, and EXPLAIN does not
    /// estimate the whole collection for the seek.
    #[test]
    fn indexed_pk_lookup_stays_bounded_under_update_churn() {
        use crate::storage::{EntityId, StoreError};
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord,
        };
        use reddb_types::Value;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        struct CountReads {
            inner: Arc<dyn CollectionRead + Send + Sync>,
            scans: Arc<AtomicUsize>,
            gets: Arc<AtomicUsize>,
        }
        impl CollectionRead for CountReads {
            fn get_record(
                &self,
                id: EntityId,
                bytes: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                self.gets.fetch_add(1, Ordering::Relaxed);
                self.inner.get_record(id, bytes)
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                self.scans.fetch_add(1, Ordering::Relaxed);
                self.inner.cursor()
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                self.scans.fetch_add(1, Ordering::Relaxed);
                self.inner.owned_cursor()
            }
        }
        const LIVE_ROWS: usize = 50;
        const UPDATES: i64 = 300;
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("lookup.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query(
                "CREATE MIGRATION windows_v1 AS CREATE TABLE windows \
                 (id TEXT PRIMARY KEY, observed_at INTEGER)",
            )
            .unwrap();
        runtime.execute_query("APPLY MIGRATION windows_v1").unwrap();
        let values = (0..LIVE_ROWS)
            .map(|key| format!("('k{key}', 0)"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO windows (id, observed_at) VALUES {values}"
            ))
            .unwrap();
        runtime.checkpoint().unwrap();
        let collect = |runtime: &RedDBRuntime| {
            for _ in 0..16 {
                runtime.try_reclaim_history_tick();
            }
        };
        for generation in 1..=UPDATES {
            runtime
                .execute_query_with_params(
                    "UPDATE windows SET observed_at = $1 WHERE id = $2",
                    &[Value::Integer(generation), Value::text("k0")],
                )
                .unwrap();
            collect(&runtime);
        }
        runtime.checkpoint().unwrap();
        collect(&runtime);
        let store = runtime.db().store();
        let manager = store.get_collection("windows").unwrap();
        assert!(
            manager.count() <= LIVE_ROWS + 2,
            "{} physical entities for {LIVE_ROWS} live rows",
            manager.count()
        );
        let mut resident = 0;
        manager.for_each_entity(|_| {
            resident += 1;
            true
        });
        assert!(
            resident <= 4,
            "{resident} resident versions after reclamation"
        );
        let root = manager.backing_root_page().unwrap();
        let count = manager.count() as u64;
        let (source, _) = manager.current_backing_overlay().unwrap();
        let scans = Arc::new(AtomicUsize::new(0));
        let gets = Arc::new(AtomicUsize::new(0));
        manager.install_backing(
            Arc::new(CountReads {
                inner: Arc::clone(&source),
                scans: Arc::clone(&scans),
                gets: Arc::clone(&gets),
            }),
            root,
            store.format_version(),
            1024 * 1024,
            count,
        );
        let point = runtime
            .execute_query_with_params(
                "SELECT observed_at FROM windows WHERE id = $1",
                &[Value::text("k0")],
            )
            .unwrap();
        assert_eq!(point.result.records.len(), 1);
        assert_eq!(
            point.result.records[0].get("observed_at"),
            Some(&Value::Integer(UPDATES))
        );
        assert_eq!(
            scans.load(Ordering::Relaxed),
            0,
            "an indexed point lookup must not scan the collection"
        );
        assert!(
            gets.load(Ordering::Relaxed) <= 4,
            "point lookup read {} stored records",
            gets.load(Ordering::Relaxed)
        );
        manager.install_backing(source, root, store.format_version(), 1024 * 1024, count);
        let explain = runtime
            .execute_query("EXPLAIN SELECT observed_at FROM windows WHERE id = 'k0'")
            .unwrap();
        let seek = explain
            .result
            .records
            .iter()
            .find(|record| {
                matches!(record.get("op"), Some(Value::Text(op)) if op.as_ref() == "index_seek")
            })
            .expect("indexed point lookup plans an index seek");
        let estimated = match seek.get("estimated_rows") {
            Some(Value::Float(rows)) => *rows,
            other => panic!("estimated_rows: {other:?}"),
        };
        assert!(
            estimated < LIVE_ROWS as f64,
            "index seek estimated {estimated} rows, the whole collection"
        );
    }

    #[test]
    fn commit_pins_protect_only_versioned_history() {
        use crate::application::vcs::{Author, CreateCommitInput};
        use reddb_types::Value;
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("pins.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        for table in ["plain", "tracked"] {
            runtime
                .execute_query(&format!(
                    "CREATE TABLE {table} (id TEXT PRIMARY KEY, v INTEGER)"
                ))
                .unwrap();
        }
        runtime.vcs_set_versioned("tracked", true).unwrap();
        for table in ["plain", "tracked"] {
            runtime
                .execute_query(&format!("INSERT INTO {table} (id, v) VALUES ('a', 1)"))
                .unwrap();
        }
        let commit = runtime
            .vcs_commit(CreateCommitInput {
                connection_id: 0,
                message: "before update".into(),
                author: Author {
                    name: "test".into(),
                    email: "test@reddb.io".into(),
                },
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .unwrap();
        let (_, reader) = runtime.snapshot_manager().fresh_retained_snapshot();
        for table in ["plain", "tracked"] {
            runtime
                .execute_query(&format!("UPDATE {table} SET v = 2 WHERE id = 'a'"))
                .unwrap();
        }
        let physical = |table: &str| runtime.db().store().get_collection(table).unwrap().count();
        let collect = || {
            for _ in 0..256 {
                runtime.try_reclaim_history_tick();
            }
        };
        collect();
        assert_eq!(physical("plain"), 2, "an open reader keeps the preimage");
        assert_eq!(physical("tracked"), 2);
        drop(reader);
        collect();
        assert_eq!(
            physical("plain"),
            1,
            "no AS OF read can address this history"
        );
        assert_eq!(physical("tracked"), 2, "the commit still reads its version");
        let historical = runtime
            .execute_query(&format!(
                "SELECT v FROM tracked AS OF COMMIT '{}' WHERE id = 'a'",
                commit.hash
            ))
            .unwrap();
        assert_eq!(historical.result.records.len(), 1);
        assert_eq!(
            historical.result.records[0].get("v"),
            Some(&Value::Integer(1))
        );
        let current = runtime
            .execute_query("SELECT v FROM plain WHERE id = 'a'")
            .unwrap();
        assert_eq!(current.result.records.len(), 1);
        assert_eq!(current.result.records[0].get("v"), Some(&Value::Integer(2)));
    }

    #[test]
    fn cold_history_reclamation_waits_for_a_retained_reader() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("reader.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE cold_history (id INTEGER)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO cold_history (id) VALUES (1)")
            .unwrap();
        let (_, retention) = runtime.snapshot_manager().fresh_retained_snapshot();
        runtime
            .execute_query("DELETE FROM cold_history WHERE id = 1")
            .unwrap();
        runtime.checkpoint().unwrap();
        let store = runtime.db().store();
        let manager = store.get_collection("cold_history").unwrap();
        assert!(manager.has_backing());
        let sweep = || {
            let mut cursor = store.mvcc_reclaim_cursor("cold_history".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..8 {
                reclaimed += store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        1,
                        1,
                    )
                    .unwrap();
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished());
            reclaimed
        };
        assert_eq!(sweep(), 0);
        assert_eq!(
            manager.count(),
            1,
            "old reader still needs the cold preimage"
        );
        drop(retention);
        assert_eq!(sweep(), 1);
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn cold_gc_skips_live_payloads_using_the_visibility_projection() {
        use crate::storage::{EntityId, StoreError};
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord,
        };
        use std::sync::Arc;
        struct RefusePayload;
        impl CollectionRead for RefusePayload {
            fn get_record(
                &self,
                _: EntityId,
                _: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                Err(StoreError::Serialization(
                    "GC read an unneeded live payload".into(),
                ))
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                Err(StoreError::Serialization("GC scanned live payloads".into()))
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                Err(StoreError::Serialization(
                    "GC retained live payloads".into(),
                ))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("inventory.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE live_rows (id INTEGER, payload TEXT)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO live_rows (id, payload) VALUES (1, 'retained')")
            .unwrap();
        runtime.checkpoint().unwrap();
        let store = runtime.db().store();
        assert_eq!(
            store.try_count_from_index("live_rows", None).unwrap(),
            Some(1)
        );
        let manager = store.get_collection("live_rows").unwrap();
        let root = manager.backing_root_page().unwrap();
        let count = manager.count() as u64;
        let (source, _) = manager.current_backing_overlay().unwrap();
        manager.install_backing(
            Arc::new(RefusePayload),
            root,
            store.format_version(),
            1024,
            count,
        );
        let mut cursor = store.mvcc_reclaim_cursor("live_rows".into()).unwrap();
        for _ in 0..8 {
            assert_eq!(
                store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        1,
                        1
                    )
                    .unwrap(),
                0
            );
            if cursor.finished() {
                break;
            }
        }
        assert!(cursor.finished());
        assert_eq!(manager.count(), 1);
        manager.install_backing(source, root, store.format_version(), 1024, count);
    }

    #[test]
    fn paused_cold_gc_does_not_force_a_checkpoint_to_copy_live_history() {
        use crate::storage::{EntityId, StoreError};
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct CountScans {
            inner: Arc<dyn CollectionRead + Send + Sync>,
            scans: Arc<AtomicUsize>,
        }
        impl CollectionRead for CountScans {
            fn get_record(
                &self,
                id: EntityId,
                bytes: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                self.inner.get_record(id, bytes)
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                self.scans.fetch_add(1, Ordering::Relaxed);
                self.inner.cursor()
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                self.scans.fetch_add(1, Ordering::Relaxed);
                self.inner.owned_cursor()
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("pins.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE append_rows (id INTEGER, payload TEXT)")
            .unwrap();
        runtime
            .execute_query(
                "INSERT INTO append_rows (id, payload) VALUES (1, 'old one'), (2, 'old two')",
            )
            .unwrap();
        runtime.checkpoint().unwrap();
        let store = runtime.db().store();
        let manager = store.get_collection("append_rows").unwrap();
        let root = manager.backing_root_page().unwrap();
        let count = manager.count() as u64;
        let (source, _) = manager.current_backing_overlay().unwrap();
        let scans = Arc::new(AtomicUsize::new(0));
        manager.install_backing(
            Arc::new(CountScans {
                inner: source,
                scans: Arc::clone(&scans),
            }),
            root,
            store.format_version(),
            1024 * 1024,
            count,
        );
        let mut cursor = store.mvcc_reclaim_cursor("append_rows".into()).unwrap();
        assert_eq!(
            store
                .reclaim_mvcc_tick(
                    &mut cursor,
                    &runtime.snapshot_manager(),
                    runtime.mvcc_vacuum_cutoff_xid(),
                    1,
                    1
                )
                .unwrap(),
            0
        );
        assert!(
            !cursor.finished(),
            "maintenance is deliberately paused mid-inventory"
        );
        runtime
            .execute_query("INSERT INTO append_rows (id, payload) VALUES (3, 'new')")
            .unwrap();
        runtime.checkpoint().unwrap();
        assert_eq!(
            scans.load(Ordering::Relaxed),
            0,
            "a paused GC tick must not force live history through full replacement"
        );
        for _ in 0..8 {
            assert_eq!(
                store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        1,
                        1
                    )
                    .unwrap(),
                0
            );
            if cursor.finished() {
                break;
            }
        }
        assert!(
            cursor.finished(),
            "resume from the current generation after checkpoint"
        );
        assert_eq!(manager.count(), 3);
        drop(cursor);
    }

    fn fixture() -> RedDBRuntime {
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(2 * 1024 * 1024),
        )
        .expect("bounded runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE pressure_history (id INTEGER)")
            .expect("table");
        runtime
            .vcs_set_versioned("pressure_history", false)
            .expect("no VCS history");
        runtime
            .execute_query("INSERT INTO pressure_history (id) VALUES (1)")
            .expect("seed row");
        runtime
    }

    fn physical_count(runtime: &RedDBRuntime) -> usize {
        runtime
            .db()
            .store()
            .get_collection("pressure_history")
            .expect("manager")
            .count()
    }

    fn collect(runtime: &RedDBRuntime) {
        for _ in 0..64 {
            runtime.try_reclaim_history_tick();
        }
    }

    #[test]
    fn foreground_history_reclamation_waits_for_a_retained_reader() {
        let runtime = fixture();
        let (_snapshot, retention) = runtime.snapshot_manager().fresh_retained_snapshot();
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        collect(&runtime);
        assert_eq!(
            physical_count(&runtime),
            1,
            "old reader still needs the preimage"
        );
        drop(retention);
        collect(&runtime);
        assert_eq!(
            physical_count(&runtime),
            0,
            "released reader permits reclamation"
        );
    }

    #[test]
    fn foreground_history_reclamation_does_not_join_a_deferred_wal_capture() {
        use crate::storage::UnifiedStore;
        struct CaptureGuard;
        impl Drop for CaptureGuard {
            fn drop(&mut self) {
                if UnifiedStore::deferred_store_wal_capture_active() {
                    drop(UnifiedStore::take_deferred_store_wal_capture());
                }
            }
        }
        let runtime = fixture();
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        UnifiedStore::begin_deferred_store_wal_capture();
        let guard = CaptureGuard;
        for _ in 0..64 {
            assert!(!runtime.try_reclaim_history_tick());
        }
        assert_eq!(physical_count(&runtime), 1);
        let actions = UnifiedStore::take_deferred_store_wal_capture();
        assert!(
            actions.is_empty(),
            "physical GC cannot join a caller's rollback journal"
        );
        drop(guard);
        collect(&runtime);
        assert_eq!(physical_count(&runtime), 0);
    }

    #[test]
    fn foreground_history_reclamation_respects_autovacuum_disable() {
        let runtime = fixture();
        runtime
            .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
            .expect("disable automatic GC");
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        for _ in 0..64 {
            assert!(!runtime.try_reclaim_history_tick());
        }
        assert_eq!(physical_count(&runtime), 1);
    }
}
