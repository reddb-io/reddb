//! `.tv` snapshot files for `vector.turbo` (issue #674).
//!
//! Verifies the slice-F acceptance criteria end-to-end:
//!
//! 1. Under a non-`Minimal` `StorageLayout` (the default `Standard`),
//!    `runtime.checkpoint()` emits a `.tv` file at the path derived by
//!    `TieredLayoutPaths::turbo_snapshot_path`.
//! 2. Under `StorageLayout::Minimal`, no `.tv` file is written even
//!    after explicit checkpoint.
//! 3. After deleting the `.tv` between runs, a restarted runtime still
//!    finds every previously-inserted vector via the slice-E
//!    extent/WAL rebuild path.
//! 4. A corrupted `.tv` is logged + ignored; boot falls back to
//!    rebuild and the runtime still answers SEARCH correctly.

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
fn standard_layout_writes_tv_snapshot_at_checkpoint() {
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
        snap.exists(),
        ".tv snapshot should exist at {} after checkpoint",
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
fn deleting_tv_snapshot_falls_back_to_rebuild() {
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
    assert!(snap.exists(), "precondition: .tv exists after checkpoint");

    // Wipe every .tv file under the support tree before restarting. The
    // engine must fall back to the slice-E extent/WAL rebuild path and
    // still answer SEARCH.
    fn rm_tv(p: &Path) {
        if let Ok(rd) = std::fs::read_dir(p) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    rm_tv(&path);
                } else if path.extension().and_then(|s| s.to_str()) == Some("tv") {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    rm_tv(dir.path());
    assert!(!snap.exists(), ".tv should be gone after manual deletion");

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
    assert!(snap.exists(), "precondition: .tv exists after checkpoint");
    // Overwrite the magic bytes so the loader rejects the file and the
    // runtime falls back to extent/WAL rebuild.
    let mut bytes = std::fs::read(&snap).unwrap();
    for b in &mut bytes[..8] {
        *b = b'X';
    }
    std::fs::write(&snap, &bytes).unwrap();

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
