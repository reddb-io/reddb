//! Turbo vectors recover from the database without automatic `.tv` sidecars.
//! Old derived snapshots are ignored and never replace authoritative rows.

use std::path::{Path, PathBuf};

use reddb_server::storage::engine::turboquant::storage::BLOCK_LANES;
use reddb_server::storage::layout::{LayoutOverrides, StorageLayout, TieredLayoutPaths};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Auto-cleaning DB directory: the returned [`tempfile::TempDir`] guard removes
/// the directory and all `.rdb`/`.tv` artifacts on drop (incl. panic). Callers
/// keep the binding alive for the whole test and read paths via `dir.path()`.
fn db_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-turbo-snapshot-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn synth_vector(i: usize) -> Vec<f32> {
    let axis = i % 8;
    let off = ((i / 8) as f32) * 0.05;
    let mut v = vec![off; 8];
    v[axis] = 1.0 + (i as f32) * 0.001;
    v
}

fn insert_vector(rt: &RedDBRuntime, collection: &str, idx: usize, vector: &[f32]) {
    let lit = vector
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    rt.execute_query(&format!(
        "INSERT INTO {collection} VECTOR (embedding, content) VALUES ([{lit}], 'v{idx}')"
    ))
    .unwrap_or_else(|err| panic!("insert v{idx}: {err:?}"));
}

fn snapshot_path_for(dir: &Path, layout: StorageLayout, collection: &str) -> Option<PathBuf> {
    let data = dir.join("data.rdb");
    let paths = TieredLayoutPaths::new(&data, layout, LayoutOverrides::default());
    paths.turbo_snapshot_path(collection)
}

#[test]
fn standard_layout_keeps_turbo_snapshot_inside_database() {
    let dir = db_dir("standard-writes");
    let path = dir.path().join("data.rdb");
    let snap = snapshot_path_for(dir.path(), StorageLayout::Standard, "turbo_snap")
        .expect("standard layout produces a snapshot path");

    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
        )
        .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_snap KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for i in 0..(BLOCK_LANES + 3) {
            insert_vector(&rt, "turbo_snap", i, &synth_vector(i));
        }
        // Touch the collection so the runtime allocates its turbo state
        // before we trigger the checkpoint that dumps the .tv.
        rt.execute_query("VECTOR SEARCH turbo_snap SIMILAR TO [1,0,0,0,0,0,0,0] LIMIT 1")
            .expect("warm up turbo state");
        rt.checkpoint().expect("checkpoint succeeds");
    }

    assert!(
        !snap.exists(),
        "checkpoint must not publish an external .tv at {}",
        snap.display()
    );
}

#[test]
fn minimal_layout_writes_no_tv_snapshot() {
    let dir = db_dir("minimal-omits");
    let path = dir.path().join("data.rdb");

    let standard_path = snapshot_path_for(dir.path(), StorageLayout::Standard, "turbo_min")
        .expect("standard layout has a snapshot path");
    let minimal_path = snapshot_path_for(dir.path(), StorageLayout::Minimal, "turbo_min");
    assert!(
        minimal_path.is_none(),
        "Minimal layout must not advertise a snapshot path"
    );

    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Minimal),
        )
        .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_min KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for i in 0..(BLOCK_LANES + 1) {
            insert_vector(&rt, "turbo_min", i, &synth_vector(i));
        }
        rt.execute_query("VECTOR SEARCH turbo_min SIMILAR TO [1,0,0,0,0,0,0,0] LIMIT 1")
            .expect("warm up turbo state");
        rt.checkpoint().expect("checkpoint succeeds");
    }

    assert!(
        !standard_path.exists(),
        "Minimal layout must not write a .tv anywhere — found {} on disk",
        standard_path.display()
    );
}

#[test]
fn turbo_vectors_recover_without_external_snapshot() {
    let dir = db_dir("delete-fallback");
    let path = dir.path().join("data.rdb");
    let n = BLOCK_LANES + 5;
    let vectors: Vec<Vec<f32>> = (0..n).map(synth_vector).collect();

    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
        )
        .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_del KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for (i, v) in vectors.iter().enumerate() {
            insert_vector(&rt, "turbo_del", i, v);
        }
        rt.execute_query("VECTOR SEARCH turbo_del SIMILAR TO [1,0,0,0,0,0,0,0] LIMIT 1")
            .expect("warm up turbo state");
        rt.checkpoint().expect("checkpoint succeeds");
    }

    let snap = snapshot_path_for(dir.path(), StorageLayout::Standard, "turbo_del").unwrap();
    assert!(!snap.exists(), "checkpoint must not create a .tv sidecar");

    let rt = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
    )
    .expect("runtime reopens persistent");
    let query_lit = vectors[n - 1]
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let hits = rt
        .execute_query(&format!(
            "VECTOR SEARCH turbo_del SIMILAR TO [{query_lit}] LIMIT 3"
        ))
        .expect("search after rebuild fallback");
    assert!(
        !hits.result.records.is_empty(),
        "rebuild fallback path must surface the inserted vectors"
    );
}

#[test]
fn corrupt_tv_snapshot_is_ignored_on_boot() {
    let dir = db_dir("corrupt-fallback");
    let path = dir.path().join("data.rdb");
    let n = BLOCK_LANES + 2;
    let vectors: Vec<Vec<f32>> = (0..n).map(synth_vector).collect();

    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
        )
        .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_corrupt KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for (i, v) in vectors.iter().enumerate() {
            insert_vector(&rt, "turbo_corrupt", i, v);
        }
        rt.execute_query("VECTOR SEARCH turbo_corrupt SIMILAR TO [1,0,0,0,0,0,0,0] LIMIT 1")
            .expect("warm up turbo state");
        rt.checkpoint().expect("checkpoint succeeds");
    }

    let snap = snapshot_path_for(dir.path(), StorageLayout::Standard, "turbo_corrupt").unwrap();
    assert!(!snap.exists());
    std::fs::create_dir_all(snap.parent().unwrap()).unwrap();
    std::fs::write(&snap, b"corrupt legacy turbo snapshot").unwrap();

    let rt = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
    )
    .expect("runtime reopens persistent after .tv corruption");
    let query_lit = vectors[0]
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let hits = rt
        .execute_query(&format!(
            "VECTOR SEARCH turbo_corrupt SIMILAR TO [{query_lit}] LIMIT 3"
        ))
        .expect("search after corrupt-snapshot fallback");
    assert!(
        !hits.result.records.is_empty(),
        "boot must succeed and rebuild from extent when .tv is corrupt"
    );
}

#[test]
fn stale_snapshot_includes_newer_persisted_vectors_after_restart() {
    let dir = db_dir("stale-snapshot");
    let path = dir.path().join("data.rdb");
    let snapshot = snapshot_path_for(dir.path(), StorageLayout::Standard, "turbo_stale").unwrap();
    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
        )
        .unwrap();
        rt.execute_query("CREATE COLLECTION turbo_stale KIND vector.turbo DIM 8 METRIC cosine")
            .unwrap();
        rt.execute_query(
            "INSERT INTO turbo_stale VECTOR (embedding, content) VALUES ([1,0,0,0,0,0,0,0], 'old')",
        )
        .unwrap();
        rt.checkpoint().unwrap();
        assert!(!snapshot.exists());
        let manager = rt.db().store().get_collection("turbo_stale").unwrap();
        let vectors = manager
            .query_all(|_| true)
            .into_iter()
            .filter_map(|entity| match entity.data {
                reddb_server::storage::EntityData::Vector(data) => {
                    Some((entity.id.raw(), data.dense.to_vec()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        reddb_file::write_turboquant_snapshot(&snapshot, 8, 0x7155_8807_FED4_2913, 0, &vectors)
            .unwrap();
    }
    assert_eq!(
        reddb_file::read_turboquant_snapshot(&snapshot, 8, 0x7155_8807_FED4_2913)
            .unwrap()
            .vectors
            .len(),
        1
    );
    {
        let rt = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
        )
        .unwrap();
        // Persist a vector without updating the existing derived snapshot.
        rt.db()
            .vector("turbo_stale")
            .dense(vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
            .content("new")
            .save()
            .unwrap();
        rt.checkpoint().unwrap();
    }
    assert_eq!(
        reddb_file::read_turboquant_snapshot(&snapshot, 8, 0x7155_8807_FED4_2913)
            .unwrap()
            .vectors
            .len(),
        1
    );
    let rt = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path).with_layout(StorageLayout::Standard),
    )
    .unwrap();
    let result = rt
        .execute_query(
            "VECTOR SEARCH turbo_stale SIMILAR TO [0,1,0,0,0,0,0,0] MODE APPROXIMATE LIMIT 1",
        )
        .unwrap();
    assert!(
        matches!(result.result.records[0].get("content"), Some(reddb_types::Value::Text(content)) if content.as_ref() == "new")
    );
}
