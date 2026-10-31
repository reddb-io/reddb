//! Process-death regressions: fixed logical data must not consume space per restart.
//! These tests preserve the OS page cache; power-loss coverage lives in SimVfs/DWB tests.
use super::*;
use crate::api::DurabilityMode;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

const CHILD: &str = "REDDB_STORAGE_APOCALYPSE_PATH";
const TEST: &str =
    "storage::unified::store::commit::storage_apocalypse::checkpoint_and_recovery_crash_matrix";
const ROWS: u64 = 48;
const COLLECTIONS: [&str; 3] = ["inline", "overflow", "compressed"];
const CHECKPOINT_FAULTS: [(&str, i32); 4] = [
    ("REDDB_EXPERIMENTAL_EXIT_DURING_CHECKPOINT_REBUILD", 183),
    ("REDDB_EXPERIMENTAL_EXIT_BEFORE_RETIRED_FREE", 184),
    ("REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_ENQUEUE", 186),
    ("REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_FREE", 185),
];
const RECOVERY_FAULTS: [(&str, i32); 3] = [
    ("REDDB_EXPERIMENTAL_EXIT_DURING_OWNERSHIP_RECOVERY", 187),
    ("REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_SYNC", 188),
    ("REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_FINISH", 189),
];

fn config(grouped: bool) -> UnifiedStoreConfig {
    UnifiedStoreConfig::default()
        .with_durability_mode(if grouped {
            DurabilityMode::WalDurableGrouped
        } else {
            DurabilityMode::Strict
        })
        .with_page_cache_slots(4)
}

fn config_for_profile(grouped: bool, disk_residency: bool) -> UnifiedStoreConfig {
    let mut config = config(grouped);
    if disk_residency {
        config.resident_memory_bytes_max = Some(128 * 1024);
    }
    config
}

// Independent deterministic payload oracle; do not derive expected values from storage.
fn payload(collection: &str, id: u64, generation: u64) -> String {
    let len = match collection {
        "inline" => 180,
        "overflow" => 12 * 1024,
        "compressed" => 24 * 1024,
        _ => unreachable!(),
    };
    if collection == "compressed" {
        return format!("{id}:{generation}:") + &"z".repeat(len);
    }
    let mut state = id.wrapping_mul(31) + generation * 17 + 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect()
}

fn entity(collection: &str, id: u64, generation: u64) -> UnifiedEntity {
    UnifiedEntity::table_row(
        EntityId::new(id),
        collection,
        id,
        vec![Value::text(payload(collection, id, generation))],
    )
}

fn seed_with_config(path: &Path, config: UnifiedStoreConfig) {
    let store = UnifiedStore::open_with_config(path, config).expect("seed configured profile");
    for collection in COLLECTIONS {
        store.create_collection(collection).unwrap();
        for id in 1..=ROWS {
            store.insert(collection, entity(collection, id, 0)).unwrap();
        }
    }
    store.persist().unwrap();
}

fn replace_with_durable_wal(store: &UnifiedStore, generation: u64) {
    let actions: Vec<_> = COLLECTIONS
        .into_iter()
        .flat_map(|collection| {
            (1..=ROWS).map(move |id| {
                StoreWalAction::upsert_entity(
                    collection,
                    &entity(collection, id, generation),
                    None,
                    STORE_VERSION_V11,
                )
            })
        })
        .collect();
    // No incremental checkpoint proof: exercise replacement/retirement and overflow.
    store
        .commit
        .as_ref()
        .unwrap()
        .append_actions(&actions)
        .unwrap();
    for action in &actions {
        store.apply_replayed_action(action, false).unwrap();
    }
}

fn verify(store: &UnifiedStore, generation: u64) {
    for collection in COLLECTIONS {
        assert_eq!(
            store.get_collection(collection).unwrap().count(),
            ROWS as usize
        );
        let trees = store.btree_indices.read();
        let tree = trees.get(collection).expect("persisted tree");
        let scan = tree
            .range(&1u64.to_be_bytes(), &ROWS.to_be_bytes())
            .unwrap();
        assert_eq!(scan.len(), ROWS as usize, "leaf chain count: {collection}");
        for (offset, (key, value)) in scan.iter().enumerate() {
            let id = offset as u64 + 1;
            assert_eq!(*key, id.to_be_bytes(), "leaf chain order: {collection}");
            assert_eq!(
                tree.get(key).unwrap().as_ref(),
                Some(value),
                "parent traversal disagrees with leaf chain: {collection} id={id}"
            );
            assert_eq!(
                store
                    .get(collection, EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                [Value::text(payload(collection, id, generation))],
                "payload: {collection} id={id} generation={generation}"
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Space {
    data: u64,
    wal: u64,
    allocated: u64,
}

fn space(path: &Path) -> Space {
    let data = std::fs::metadata(path).unwrap();
    let wal = std::fs::metadata(reddb_file::layout::unified_wal_path(path)).unwrap();
    Space {
        data: data.len(),
        wal: wal.len(),
        allocated: (data.blocks() + wal.blocks()) * 512,
    }
}

fn assert_plateau(baseline: Space, now: Space, context: &str) {
    // Fixed working set, four warmup generations; only allocator/metadata rounding is allowed.
    let slack = 2 * crate::storage::engine::PAGE_SIZE as u64;
    assert!(
        now.data <= baseline.data + slack,
        "{context}: data grows: {baseline:?} -> {now:?}"
    );
    assert!(
        now.wal <= baseline.wal + slack,
        "{context}: WAL grows: {baseline:?} -> {now:?}"
    );
    // Filesystems may reserve blocks in larger extents than a database page.
    assert!(
        now.allocated <= baseline.allocated + 128 * 1024,
        "{context}: physical allocation grows: {baseline:?} -> {now:?}"
    );
}

fn crash(
    path: &Path,
    grouped: bool,
    generation: u64,
    recovery: bool,
    disk_residency: bool,
    hook: (&str, i32),
) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, path)
        .env(
            "REDDB_STORAGE_APOCALYPSE_DISK_RESIDENCY",
            if disk_residency { "1" } else { "0" },
        )
        .env(
            "REDDB_STORAGE_APOCALYPSE_GROUPED",
            if grouped { "1" } else { "0" },
        )
        .env(
            "REDDB_STORAGE_APOCALYPSE_GENERATION",
            generation.to_string(),
        )
        .env(
            "REDDB_STORAGE_APOCALYPSE_RECOVERY",
            if recovery { "1" } else { "0" },
        )
        .env(hook.0, "1")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(hook.1),
        "hook={} generation={generation} grouped={grouped}\nstdout={}\nstderr={}",
        hook.0,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checkpoint_and_recovery_crash_matrix() {
    if let Some(path) = std::env::var_os(CHILD) {
        let grouped = std::env::var("REDDB_STORAGE_APOCALYPSE_GROUPED").unwrap() == "1";
        let disk_residency =
            std::env::var("REDDB_STORAGE_APOCALYPSE_DISK_RESIDENCY").as_deref() == Ok("1");
        let store =
            UnifiedStore::open_with_config(path, config_for_profile(grouped, disk_residency))
                .expect("recover configured profile");
        assert_ne!(
            std::env::var("REDDB_STORAGE_APOCALYPSE_RECOVERY").unwrap(),
            "1",
            "recovery failpoint was not reached"
        );
        let generation = std::env::var("REDDB_STORAGE_APOCALYPSE_GENERATION")
            .unwrap()
            .parse()
            .unwrap();
        replace_with_durable_wal(&store, generation);
        store.persist().unwrap();
        panic!("checkpoint failpoint was not reached");
    }
    run_checkpoint_crash_matrix(false);
}

#[test]
fn cold_checkpoint_and_recovery_crash_matrix() {
    run_checkpoint_crash_matrix(true);
}

fn run_checkpoint_crash_matrix(disk_residency: bool) {
    for grouped in [false, true] {
        for checkpoint in CHECKPOINT_FAULTS {
            for recovery in RECOVERY_FAULTS {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("data.rdb");
                seed_with_config(&path, config_for_profile(grouped, disk_residency));
                let mut baseline = None;
                for round in 1..=8 {
                    let generation = round * 2;
                    // Recovery can already drain the retirement ledger. Publish
                    // a distinct generation before EVERY attempt so all three
                    // retirement hooks have actual old pages to reclaim.
                    let preparation = UnifiedStore::open_with_config(
                        &path,
                        config_for_profile(grouped, disk_residency),
                    )
                    .unwrap();
                    replace_with_durable_wal(&preparation, generation - 1);
                    preparation.persist().unwrap();
                    drop(preparation);
                    crash(
                        &path,
                        grouped,
                        generation,
                        false,
                        disk_residency,
                        checkpoint,
                    );
                    // Fail recovery twice at the same boundary, except Finish (already durable).
                    for _ in 0..if recovery.1 == 189 { 1 } else { 2 } {
                        crash(&path, grouped, generation, true, disk_residency, recovery);
                    }
                    let store = UnifiedStore::open_with_config(
                        &path,
                        config_for_profile(grouped, disk_residency),
                    )
                    .unwrap();
                    verify(&store, generation);
                    store.persist().unwrap();
                    drop(store);
                    let store = UnifiedStore::open_with_config(
                        &path,
                        config_for_profile(grouped, disk_residency),
                    )
                    .unwrap();
                    verify(&store, generation);
                    drop(store);
                    let sample = space(&path);
                    let context = format!(
                        "grouped={grouped} checkpoint={} recovery={} generation={generation}",
                        checkpoint.0, recovery.0
                    );
                    eprintln!("{context} {sample:?}");
                    if round == 4 {
                        baseline = Some(sample);
                    }
                    if let Some(baseline) = baseline {
                        assert_plateau(baseline, sample, &context);
                    }
                }
            }
        }
    }
}

#[test]
fn delete_refill_and_overflow_resize_plateau() {
    run_delete_refill_plateau(false, 16);
}

#[test]
fn cold_delete_refill_and_overflow_resize_plateau_100_cycles() {
    run_delete_refill_plateau(true, 100);
}

fn run_delete_refill_plateau(disk_residency: bool, cycles: u64) {
    for grouped in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        seed_with_config(&path, config_for_profile(grouped, disk_residency));
        let mut baselines = BTreeMap::new();
        for cycle in 0..cycles {
            let store =
                UnifiedStore::open_with_config(&path, config_for_profile(grouped, disk_residency))
                    .unwrap();
            for collection in COLLECTIONS {
                let ids: Vec<_> = (1..=ROWS).map(EntityId::new).collect();
                assert_eq!(
                    store.delete_batch(collection, &ids).unwrap().len(),
                    ROWS as usize
                );
            }
            store.persist().unwrap();
            drop(store);
            let store =
                UnifiedStore::open_with_config(&path, config_for_profile(grouped, disk_residency))
                    .unwrap();
            for collection in COLLECTIONS {
                assert_eq!(store.get_collection(collection).unwrap().count(), 0);
                for id in 1..=ROWS {
                    assert!(store.get(collection, EntityId::new(id)).is_none());
                    // Alternate short values and overflow chains at exactly the same keys.
                    let row = if cycle % 2 == 0 {
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            collection,
                            id,
                            vec![Value::text("short")],
                        )
                    } else {
                        entity(collection, id, cycle)
                    };
                    store.insert(collection, row).unwrap();
                }
            }
            store.persist().unwrap();
            drop(store);
            let store =
                UnifiedStore::open_with_config(&path, config_for_profile(grouped, disk_residency))
                    .unwrap();
            if cycle % 2 == 1 {
                verify(&store, cycle);
            } else {
                for collection in COLLECTIONS {
                    assert_eq!(
                        store.get_collection(collection).unwrap().count(),
                        ROWS as usize
                    );
                    for id in 1..=ROWS {
                        assert_eq!(
                            store
                                .get(collection, EntityId::new(id))
                                .unwrap()
                                .data
                                .as_row()
                                .unwrap()
                                .columns,
                            [Value::text("short")]
                        );
                    }
                }
            }
            drop(store);
            let sample = space(&path);
            eprintln!("delete_refill grouped={grouped} cycle={cycle} {sample:?}");
            if cycle == 6 || cycle == 7 {
                baselines.insert(cycle % 2, sample);
            }
            if let Some(baseline) = baselines.get(&(cycle % 2)) {
                assert_plateau(
                    *baseline,
                    sample,
                    &format!("delete_refill grouped={grouped} cycle={cycle}"),
                );
            }
        }
    }
}
