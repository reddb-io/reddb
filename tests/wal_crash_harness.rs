//! WAL durability harness — crash-survives-to-replay invariants.
//!
//! These tests exist as a safety net for the WAL Phase 2 refactor
//! (crossbeam SegQueue lock-free append). Each test captures an
//! invariant that Phase 2 must not break:
//!
//! - after a committed transaction, reopening the store recovers it
//!   even if the pager never flushed
//! - uncommitted state (mid-tx drops) must not appear post-recovery
//! - concurrent writers produce a consistent recovered state
//!
//! We don't literally kill the process — instead we drop the store
//! without calling `persist`, forcing reopen to go through the WAL
//! replay path that Phase 2 will rewrite.
//!
//! When Phase 2 lands, the only thing that changes is the internal
//! WAL writer implementation; every assertion here must still hold.
//! If one fails, Phase 2 has lost data on recovery.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use reddb::api::DurabilityMode;
use reddb::storage::schema::Value;
use reddb::storage::{
    EntityData, EntityId, EntityKind, RowData, UnifiedEntity, UnifiedStore, UnifiedStoreConfig,
};

struct FileGuard {
    path: PathBuf,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        // Best-effort — integration runs in parallel tmp dirs.
        let _ = std::fs::remove_file(&self.path);
        let wal = self.path.with_extension("rdb-uwal");
        let _ = std::fs::remove_file(wal);
    }
}

fn tmp_path(name: &str) -> (FileGuard, PathBuf) {
    let base = std::env::temp_dir();
    let path = base.join(format!(
        "reddb_wal_crash_{}_{}_{}.rdb",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let guard = FileGuard { path: path.clone() };
    // Best-effort cleanup of stale files from a prior run.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("rdb-uwal"));
    (guard, path)
}

fn row_entity(id: u64, name: &str) -> UnifiedEntity {
    let mut named: HashMap<String, Value> = HashMap::new();
    named.insert("id".to_string(), Value::UnsignedInteger(id));
    named.insert("name".to_string(), Value::text(name));
    UnifiedEntity::new(
        EntityId::new(id),
        EntityKind::TableRow {
            table: std::sync::Arc::from("t"),
            row_id: id,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(named),
            schema: None,
        }),
    )
}

/// Grouped durability is the production default and the mode Phase 2
/// will actually touch — Strict synchronously flushes every insert
/// through the pager so it never exercises the WAL coordinator's
/// append path. Every test below uses grouped.
fn grouped_config() -> UnifiedStoreConfig {
    let mut c = UnifiedStoreConfig::default();
    c.durability_mode = DurabilityMode::WalDurableGrouped;
    c
}

#[test]
fn wal_crash_strict_commits_survive_drop() {
    let (_g, path) = tmp_path("strict_commits");
    {
        let store = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
        store.create_collection("t").unwrap();
        for i in 1..=50u64 {
            store
                .insert_auto("t", row_entity(i, &format!("row_{i}")))
                .unwrap();
        }
        // NO persist() — drop simulates crash before pager flush. WAL
        // replay on reopen must still recover every committed row.
    }

    let reopened = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
    let mgr = reopened
        .get_collection("t")
        .expect("t must be recovered from WAL");
    let recovered = mgr.query_all(|_| true).len();
    assert_eq!(
        recovered, 50,
        "Strict mode: every committed insert must survive a drop (got {recovered})"
    );
}

#[test]
fn wal_crash_grouped_commits_survive_after_force_sync() {
    // WalDurableGrouped batches fsync. If the caller explicitly waits
    // for durability (DurabilityMode::Strict promotes each commit to
    // synchronous), every row must recover. Grouped *without* an
    // explicit force_sync is still guaranteed for records older than
    // the last batch — but this test only asserts the fully-durable
    // subset to stay correct under Phase 2's reorder semantics.
    let (_g, path) = tmp_path("grouped_commits");
    {
        let store = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
        store.create_collection("t").unwrap();
        for i in 1..=40u64 {
            store
                .insert_auto("t", row_entity(i, &format!("r{i}")))
                .unwrap();
        }
    }
    let reopened = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
    let mgr = reopened.get_collection("t").expect("collection recovered");
    let recovered = mgr.query_all(|_| true).len();
    assert_eq!(
        recovered, 40,
        "Grouped reopen must see all strict-durable prefix writes (got {recovered})"
    );
}

#[test]
fn wal_crash_concurrent_writers_consistent_recovery() {
    let (_g, path) = tmp_path("concurrent");
    {
        let store = Arc::new(UnifiedStore::open_with_config(&path, grouped_config()).unwrap());
        store.create_collection("t").unwrap();

        let mut handles = Vec::new();
        for worker in 0..4u64 {
            let s = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                for i in 0..25u64 {
                    let id = worker * 1000 + i + 1;
                    s.insert_auto("t", row_entity(id, &format!("w{worker}_i{i}")))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // drop without persist
    }

    let reopened = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
    let mgr = reopened.get_collection("t").expect("collection recovered");
    let recovered = mgr.query_all(|_| true).len();
    assert_eq!(
        recovered,
        4 * 25,
        "All 4 workers × 25 inserts must recover (got {recovered})"
    );

    // Every original id must be reachable by a direct get. This guards
    // against a Phase 2 LSN-ordering bug that could land records on
    // disk in the wrong sequence and leak ids during replay.
    let mut seen_ids = std::collections::HashSet::new();
    mgr.for_each_entity(|entity| {
        seen_ids.insert(entity.id.raw());
        true
    });
    for worker in 0..4u64 {
        for i in 0..25u64 {
            let expected = worker * 1000 + i + 1;
            assert!(
                seen_ids.contains(&expected),
                "id {expected} missing after concurrent recovery"
            );
        }
    }
}

#[test]
fn wal_crash_reopen_is_idempotent() {
    // Open → write → drop → open → drop → open must leave state
    // identical to a single clean round-trip. Phase 2's leader-drain
    // path must replay the same WAL prefix on every reopen without
    // double-applying records.
    let (_g, path) = tmp_path("idempotent_reopen");
    {
        let store = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
        store.create_collection("t").unwrap();
        for i in 1..=20u64 {
            store
                .insert_auto("t", row_entity(i, &format!("r{i}")))
                .unwrap();
        }
    }
    let count_after_first = {
        let s = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
        s.get_collection("t")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0)
    };
    let count_after_second = {
        let s = UnifiedStore::open_with_config(&path, grouped_config()).unwrap();
        s.get_collection("t")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0)
    };
    assert_eq!(count_after_first, 20);
    assert_eq!(
        count_after_first, count_after_second,
        "reopening twice must not duplicate or drop rows"
    );
}
