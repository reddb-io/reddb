//! Manual, process-isolated vector cold-start benchmark.
//!
//! Run with `cargo test -p reddb-io-server --test vector_cold_start_bench -- --ignored --nocapture`.
//! Set REDDB_BENCH_VECTORS and REDDB_BENCH_DIM to vary the fixture.
//! Process restarts are real; the OS page cache is not flushed.

use std::path::Path;
use std::time::Instant;

use reddb_server::api::DurabilityMode;
use reddb_server::storage::engine::turboquant::index::TurboQuantIndex;
use reddb_server::storage::layout::{LayoutOverrides, StorageLayout, TieredLayoutPaths};
use reddb_server::storage::EntityId;
use reddb_server::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
use reddb_server::{RedDBOptions, RedDBRuntime};

fn vector(i: usize, dim: usize) -> Vec<f32> {
    let mut vector = vec![0.01; dim];
    vector[i % dim] = 1.0 + (i % 100) as f32 / 100.0;
    vector
}

fn options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_layout(StorageLayout::Standard)
        .with_durability_mode(DurabilityMode::Async)
        .with_storage_profile(StorageProfileSelection {
            deploy_profile: DeployProfile::Embedded,
            packaging: StoragePackaging::OperationalDirectory,
            replica_count: 0,
            managed_backup: false,
            wal_retention: false,
        })
        .unwrap()
}

#[test]
#[ignore = "manual benchmark"]
fn vector_cold_start_child() {
    let Some(path) = std::env::var_os("REDDB_BENCH_CHILD_DB") else {
        return;
    };
    let dim: usize = std::env::var("REDDB_BENCH_DIM").unwrap().parse().unwrap();
    let start = Instant::now();
    let runtime = RedDBRuntime::with_options(options(Path::new(&path))).unwrap();
    let open = start.elapsed();
    assert!(runtime.db().is_paged());
    let literal = vector(0, dim)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let start = Instant::now();
    let result = runtime
        .execute_query(&format!(
            "VECTOR SEARCH bench_vectors SIMILAR TO [{literal}] MODE APPROXIMATE LIMIT 1"
        ))
        .unwrap();
    let search = start.elapsed();
    assert_eq!(result.result.records.len(), 1);
    println!(
        "BENCH open_ms={:.3} first_search_ms={:.3}",
        open.as_secs_f64() * 1000.0,
        search.as_secs_f64() * 1000.0
    );
}

#[test]
#[ignore = "manual benchmark"]
fn vector_cold_start_benchmark() {
    let count: usize = std::env::var("REDDB_BENCH_VECTORS")
        .unwrap_or_else(|_| "2000".into())
        .parse()
        .unwrap();
    let dim: usize = std::env::var("REDDB_BENCH_DIM")
        .unwrap_or_else(|_| "8".into())
        .parse()
        .unwrap();
    assert!(count > 0 && dim > 0);
    let mut index = TurboQuantIndex::new(dim, 0x7155_8807_FED4_2913);
    let start = Instant::now();
    for i in 0..count {
        index.insert(EntityId::new(i as u64 + 1), vector(i, dim));
    }
    let initial = start.elapsed();
    let start = Instant::now();
    for i in 0..count {
        index.insert(EntityId::new(i as u64 + 1), vector(i, dim));
    }
    let duplicate = start.elapsed();
    assert_eq!(index.len(), count);
    println!(
        "BENCH count={count} dim={dim} index_build_ms={:.3} duplicate_pass_ms={:.3}",
        initial.as_secs_f64() * 1000.0,
        duplicate.as_secs_f64() * 1000.0
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.rdb");
    let snapshot =
        TieredLayoutPaths::new(&path, StorageLayout::Standard, LayoutOverrides::default())
            .turbo_snapshot_path("bench_vectors")
            .unwrap();
    {
        let runtime = RedDBRuntime::with_options(options(&path)).unwrap();
        assert!(runtime.db().is_paged());
        runtime
            .execute_query(&format!(
                "CREATE COLLECTION bench_vectors KIND vector.turbo DIM {dim} METRIC cosine"
            ))
            .unwrap();
        for i in 0..count {
            runtime
                .db()
                .vector("bench_vectors")
                .dense(vector(i, dim))
                .save()
                .unwrap();
        }
        runtime.checkpoint().unwrap();
    }
    // A reopened runtime builds its index from the persisted entities, then
    // publishes a populated snapshot. No setup time is included in timings.
    if snapshot.exists() {
        std::fs::remove_file(&snapshot).unwrap();
    }
    {
        let runtime = RedDBRuntime::with_options(options(&path)).unwrap();
        let literal = vector(0, dim)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "VECTOR SEARCH bench_vectors SIMILAR TO [{literal}] MODE APPROXIMATE LIMIT 1"
            ))
            .unwrap();
        runtime.checkpoint().unwrap();
    }
    assert!(snapshot.exists());
    let stored =
        reddb_file::read_turboquant_snapshot(&snapshot, dim as u32, 0x7155_8807_FED4_2913).unwrap();
    assert_eq!(
        stored.vectors.len(),
        count,
        "snapshot must cover the fixture"
    );
    println!(
        "BENCH database_bytes={} snapshot_bytes={}",
        std::fs::metadata(&path).unwrap().len(),
        std::fs::metadata(&snapshot).unwrap().len()
    );
    for mode in ["snapshot", "no_snapshot"] {
        for iteration in 0..3 {
            if mode == "no_snapshot" && snapshot.exists() {
                std::fs::remove_file(&snapshot).unwrap();
            } else if mode == "snapshot" {
                assert!(
                    snapshot.exists(),
                    "snapshot must exist for each snapshot run"
                );
            }
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "vector_cold_start_child",
                    "--nocapture",
                ])
                .env("REDDB_BENCH_CHILD_DB", &path)
                .env("REDDB_BENCH_DIM", dim.to_string())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "child failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output = String::from_utf8(result.stdout).unwrap();
            let measurement = output
                .lines()
                .find_map(|line| line.split_once("BENCH ").map(|(_, value)| value))
                .expect("child measurement");
            println!("BENCH mode={mode} iteration={iteration} {measurement}");
        }
    }
}
