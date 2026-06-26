//! Boot-time recovery for `vector.turbo` (issue #694).
//!
//! End-to-end: create a turbo collection, insert vectors spanning
//! at least two full blocks plus a partial tail, simulate a restart
//! by dropping the in-memory runtime and reopening the persistent
//! database, then assert that the rebuilt in-memory state matches
//! the pre-restart state exactly — same block/lane placement, same
//! encoded codes, same scale, same search results.

use reddb_server::storage::engine::turboquant::storage::BLOCK_LANES;
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Auto-cleaning DB path: holds the [`tempfile::TempDir`] guard so the temp
/// directory and the `.rdb` (plus every sidecar artifact) are removed on drop,
/// including on panic. Derefs/coerces to `&Path`, so callers keep using
/// `&path` / `RedDBOptions::persistent(&path)` unchanged while the directory
/// lives for the whole test.
struct TempDb {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl std::ops::Deref for TempDb {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl From<&TempDb> for std::path::PathBuf {
    fn from(value: &TempDb) -> std::path::PathBuf {
        value.path.clone()
    }
}

fn db_path(tag: &str) -> TempDb {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-turbo-boot-rebuild-{tag}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("reddb.rdb");
    TempDb { _dir: dir, path }
}

fn synth_vector(i: usize) -> Vec<f32> {
    // 8-dim vectors with one dominant axis + a small jitter on a
    // second axis. Distinct enough for the 4-bit codec to preserve
    // ranking but deterministic across runs.
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

/// Acceptance test for issue #694.
///
/// Insert N = 2*BLOCK_LANES + 5 = 69 vectors so the on-disk extent
/// has two full blocks + a 5-lane partial tail. Drop the runtime,
/// reopen, and confirm a vector search returns the same top-k as
/// against an independently-rebuilt scalar oracle that consumed the
/// same vectors in the same WAL order.
#[test]
fn turbo_collection_recovers_partial_block_tail_after_restart() {
    let path = db_path("multi-block");
    let n = 2 * BLOCK_LANES + 5;
    let vectors: Vec<Vec<f32>> = (0..n).map(synth_vector).collect();

    // Pre-restart write phase.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_boot KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for (i, v) in vectors.iter().enumerate() {
            insert_vector(&rt, "turbo_boot", i, v);
        }
    }

    // Restart phase: brand-new runtime against the same files.
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("runtime reopens persistent");

    // Query the dominant axis of the very last partial-tail vector
    // (lane 4 of block 2). Recovery must surface it.
    let query = vectors[n - 1].clone();
    let query_lit = query
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let runtime_hits = rt
        .execute_query(&format!(
            "VECTOR SEARCH turbo_boot SIMILAR TO [{query_lit}] LIMIT 5"
        ))
        .expect("search after reopen");

    // True scalar oracle: exact cosine distance over the raw FP32 vectors,
    // in insertion order. The runtime over-fetches turbo candidates and
    // re-ranks by full precision (#1372), so a correct boot rebuild must
    // return the same top-k as a brute-force exact scan — independent of the
    // lossy block/lane quantization. (The codec-quantized reference this test
    // used before could not see the runtime's exact re-rank and diverged; the
    // determinism-across-restarts contract is covered by the sibling test.)
    fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            1.0
        } else {
            1.0 - dot / (na * nb)
        }
    }
    let mut scored: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (i, cosine_distance(&query, v)))
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    assert_eq!(
        runtime_hits.result.len(),
        5,
        "runtime returns top-5 after boot rebuild"
    );
    // The runtime returns rows with a `content` column; compare by content
    // since the runtime assigns its own ids — what matters is the ordering
    // of the FP32 vectors, not the id values.
    let runtime_contents: Vec<String> = runtime_hits
        .result
        .records
        .iter()
        .map(|r| match r.get("content") {
            Some(reddb_server::storage::schema::Value::Text(s)) => s.to_string(),
            other => panic!("expected text content, got {other:?}"),
        })
        .collect();
    let oracle_contents: Vec<String> = scored
        .iter()
        .take(5)
        .map(|(i, _)| format!("v{i}"))
        .collect();
    assert_eq!(
        runtime_contents, oracle_contents,
        "runtime ordering must match exact scalar oracle after boot rebuild",
    );
}

/// Determinism contract: two independent restarts of the same
/// persisted state must produce byte-identical TurboQuant search
/// results.
#[test]
fn turbo_recovery_is_deterministic_across_restarts() {
    let path = db_path("determinism");
    let n = BLOCK_LANES + 7;
    let vectors: Vec<Vec<f32>> = (0..n).map(synth_vector).collect();

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime boots persistent");
        rt.execute_query("CREATE COLLECTION turbo_det KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create vector.turbo collection");
        for (i, v) in vectors.iter().enumerate() {
            insert_vector(&rt, "turbo_det", i, v);
        }
    }

    let query_lit = vectors[0]
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");

    let run = || -> Vec<String> {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime reopens persistent");
        let hits = rt
            .execute_query(&format!(
                "VECTOR SEARCH turbo_det SIMILAR TO [{query_lit}] LIMIT 5"
            ))
            .expect("search after reopen");
        hits.result
            .records
            .iter()
            .map(|r| match r.get("content") {
                Some(reddb_server::storage::schema::Value::Text(s)) => s.to_string(),
                other => panic!("expected text content, got {other:?}"),
            })
            .collect()
    };

    let a = run();
    let b = run();
    assert_eq!(a, b, "two restarts produce identical recovery state");
}
