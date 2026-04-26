//! PLAN.md B1 — cold-start P95 measurement.
//!
//! Drives one cold-start scenario at a time. Three scenarios match
//! PLAN.md B1 targets:
//!
//!   - `warm`         — data dir present, fresh process. Target P95 < 2s.
//!   - `volume_only`  — alias of `warm`; identical under a fresh-process
//!                      bench harness. Kept for symmetry with the PLAN.
//!   - `cold_remote`  — data dir wiped before every iteration, remote
//!                      backend (`LocalBackend` rooted at a tmp dir)
//!                      seeded with one snapshot. Auto-restore pulls
//!                      from the backend on open. Target P95 < 10s for 1 GB.
//!
//! Per iteration:
//!   1. (warm/volume_only) Open the existing data dir.
//!   2. (cold_remote) Wipe the data dir, then open — auto-restore fires.
//!   3. Measure open-time + per-phase markers, drop runtime, repeat.
//!
//! Emits a single JSON record on stdout for the shell driver
//! (`scripts/cold-start-bench.sh`) to aggregate into
//! `bench/cold-start-baseline.md`.
//!
//! Env (all optional):
//!   COLD_START_SCENARIO   — `warm` | `volume_only` | `cold_remote`; default `warm`
//!   COLD_START_SIZE_MB    — target DB size; default 100
//!   COLD_START_ITERS      — measured iterations; default 20
//!   COLD_START_WARMUP     — discarded warmup iterations; default 2
//!   COLD_START_DATA_DIR   — pre-populated data dir; if unset, a fresh
//!                           dir is built and torn down per run
//!   COLD_START_REMOTE_DIR — pre-seeded remote dir for `cold_remote`;
//!                           if unset, a fresh remote is built
//!   COLD_START_KEEP_DIR   — when set, leaves data + remote dirs intact
//!                           (used by the shell driver to reuse a large
//!                           DB across scenarios)

use reddb::api::RedDBOptions;
use reddb::application::{CreateRowInput, CreateRowsBatchInput, EntityUseCases};
use reddb::storage::backend::LocalBackend;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const TARGET_BYTES_PER_ROW: u64 = 256;
const ROWS_PER_BATCH: usize = 1_000;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn dir_size_bytes(path: &PathBuf) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    total += md.len();
                } else if md.is_dir() {
                    total += dir_size_bytes(&e.path());
                }
            }
        }
    }
    total
}

fn populate(rt: &RedDBRuntime, target_bytes: u64) {
    let uc = EntityUseCases::new(rt);
    let mut written = 0u64;
    let mut id: u64 = 0;
    while written < target_bytes {
        let mut rows = Vec::with_capacity(ROWS_PER_BATCH);
        for _ in 0..ROWS_PER_BATCH {
            id += 1;
            rows.push(CreateRowInput {
                collection: "bench_rows".into(),
                fields: vec![
                    ("id".into(), Value::Integer(id as i64)),
                    ("name".into(), Value::text(format!("user-{id:08}"))),
                    (
                        "payload".into(),
                        Value::text("x".repeat(TARGET_BYTES_PER_ROW as usize / 2)),
                    ),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            });
        }
        uc.create_rows_batch(CreateRowsBatchInput {
            collection: "bench_rows".into(),
            rows,
        })
        .expect("populate batch");
        written += TARGET_BYTES_PER_ROW * ROWS_PER_BATCH as u64;
    }
}

#[derive(Default)]
struct Sample {
    open_ms: u64,
    restore_ms: u64,
    wal_replay_ms: u64,
    index_warmup_ms: u64,
    total_ms: u64,
}

fn options_with_optional_remote(data_dir: &PathBuf, remote_dir: Option<&PathBuf>) -> RedDBOptions {
    // `RedDBOptions::persistent` expects a file path (the `data.rdb`
    // blob). Anchoring it inside `data_dir` keeps every artifact the
    // engine writes (`data.rdb`, lease, manifest, WAL segments) under
    // the same directory the bench can wipe between iterations.
    let opts = RedDBOptions::persistent(data_dir.join("data.rdb"));
    match remote_dir {
        Some(rd) => {
            // LocalBackend treats the `remote_key` as a filesystem path and
            // derives snapshot/wal prefixes from `remote_namespace_prefix()`
            // (parent directory of `remote_key`). Anchoring the key under
            // `<remote_dir>/data.rdb` keeps every prefix below `remote_dir`.
            let key = rd.join("data.rdb").to_string_lossy().to_string();
            opts.with_remote_backend(Arc::new(LocalBackend), key)
        }
        None => opts,
    }
}

fn open_and_measure(data_dir: &PathBuf, remote_dir: Option<&PathBuf>) -> Sample {
    let opts = options_with_optional_remote(data_dir, remote_dir);
    let t0 = Instant::now();
    let rt = RedDBRuntime::with_options(opts).expect("reopen");
    let open_ms = t0.elapsed().as_millis() as u64;

    let phases = rt.lifecycle().cold_start_phases();
    let mut s = Sample {
        open_ms,
        ..Default::default()
    };
    for (name, dur) in phases.durations_ms() {
        match name {
            "restore" => s.restore_ms = dur,
            "wal_replay" => s.wal_replay_ms = dur,
            "index_warmup" => s.index_warmup_ms = dur,
            "total" => s.total_ms = dur,
            _ => {}
        }
    }
    drop(rt);
    s
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Warm,
    VolumeOnly,
    ColdRemote,
}

impl Scenario {
    fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "warm" => Scenario::Warm,
            "volume_only" | "volume-only" => Scenario::VolumeOnly,
            "cold_remote" | "cold-remote" => Scenario::ColdRemote,
            other => panic!(
                "[cold-start-bench] unknown COLD_START_SCENARIO={other}; expected warm | volume_only | cold_remote"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Scenario::Warm => "warm",
            Scenario::VolumeOnly => "volume_only",
            Scenario::ColdRemote => "cold_remote",
        }
    }
}

fn fresh_tmp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-cold-start-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

fn main() {
    let scenario =
        Scenario::parse(&std::env::var("COLD_START_SCENARIO").unwrap_or_else(|_| "warm".into()));
    let size_mb = env_u64("COLD_START_SIZE_MB", 100);
    let iters = env_u64("COLD_START_ITERS", 20) as usize;
    let warmup = env_u64("COLD_START_WARMUP", 2) as usize;
    let keep_dir = std::env::var("COLD_START_KEEP_DIR").is_ok();

    let provided_dir = env_path("COLD_START_DATA_DIR");
    let provided_remote = env_path("COLD_START_REMOTE_DIR");
    let data_dir = provided_dir
        .clone()
        .unwrap_or_else(|| fresh_tmp_dir("data"));
    let remote_dir = provided_remote
        .clone()
        .unwrap_or_else(|| fresh_tmp_dir("remote"));

    eprintln!(
        "[cold-start-bench] scenario={} size_mb={} data_dir={} remote_dir={}",
        scenario.name(),
        size_mb,
        data_dir.display(),
        remote_dir.display(),
    );

    // Populate phase. For `warm` / `volume_only` we just fill the data
    // dir to the target size. For `cold_remote` we additionally seed
    // the remote with one snapshot via `trigger_backup`, then leave
    // the remote untouched while the iteration loop wipes the data
    // dir per cycle so auto-restore fires from the remote each open.
    let needs_populate = match scenario {
        Scenario::Warm | Scenario::VolumeOnly => provided_dir.is_none(),
        Scenario::ColdRemote => provided_remote.is_none(),
    };
    if needs_populate {
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir).expect("mkdir data dir");
        if scenario == Scenario::ColdRemote {
            let _ = std::fs::remove_dir_all(&remote_dir);
            std::fs::create_dir_all(&remote_dir).expect("mkdir remote dir");
        }
        eprintln!(
            "[cold-start-bench] populating {} to ~{} MB",
            data_dir.display(),
            size_mb
        );
        let backend_for_seed = if scenario == Scenario::ColdRemote {
            Some(&remote_dir)
        } else {
            None
        };
        let opts = options_with_optional_remote(&data_dir, backend_for_seed);
        let rt = RedDBRuntime::with_options(opts).expect("populate open");
        populate(&rt, size_mb * 1_048_576);
        if scenario == Scenario::ColdRemote {
            eprintln!("[cold-start-bench] triggering backup to remote");
            rt.trigger_backup().expect("seed backup");
        }
        drop(rt);
    }

    let data_bytes = dir_size_bytes(&data_dir);
    let remote_bytes = if scenario == Scenario::ColdRemote {
        dir_size_bytes(&remote_dir)
    } else {
        0
    };
    eprintln!(
        "[cold-start-bench] data_dir = {} MB; remote_dir = {} MB",
        data_bytes / 1_048_576,
        remote_bytes / 1_048_576
    );

    let total_iters = warmup + iters;
    let mut samples: Vec<Sample> = Vec::with_capacity(iters);
    for i in 0..total_iters {
        if scenario == Scenario::ColdRemote {
            // Wipe local data dir so the open path takes the
            // auto-restore branch in `RedDB::open_with_options`.
            let _ = std::fs::remove_dir_all(&data_dir);
            std::fs::create_dir_all(&data_dir).expect("mkdir data dir before iter");
        }
        let backend_for_iter = if scenario == Scenario::ColdRemote {
            Some(&remote_dir)
        } else {
            None
        };
        let s = open_and_measure(&data_dir, backend_for_iter);
        if i < warmup {
            eprintln!(
                "[cold-start-bench] warmup {} open={}ms total={}ms",
                i, s.open_ms, s.total_ms
            );
            continue;
        }
        eprintln!(
            "[cold-start-bench] iter {} open={}ms restore={}ms wal_replay={}ms index_warmup={}ms total={}ms",
            i - warmup,
            s.open_ms,
            s.restore_ms,
            s.wal_replay_ms,
            s.index_warmup_ms,
            s.total_ms
        );
        samples.push(s);
    }

    if !keep_dir {
        if provided_dir.is_none() {
            let _ = std::fs::remove_dir_all(&data_dir);
        }
        if provided_remote.is_none() {
            let _ = std::fs::remove_dir_all(&remote_dir);
        }
    }

    let mut opens: Vec<u64> = samples.iter().map(|s| s.open_ms).collect();
    let mut totals: Vec<u64> = samples.iter().map(|s| s.total_ms).collect();
    let mut restores: Vec<u64> = samples.iter().map(|s| s.restore_ms).collect();
    opens.sort_unstable();
    totals.sort_unstable();
    restores.sort_unstable();

    println!(
        "{{\"scenario\":\"{}\",\"size_mb\":{},\"iters\":{},\"data_bytes\":{},\"remote_bytes\":{},\"open_ms_p50\":{},\"open_ms_p95\":{},\"open_ms_p99\":{},\"total_ms_p50\":{},\"total_ms_p95\":{},\"total_ms_p99\":{},\"restore_ms_p50\":{},\"restore_ms_p95\":{}}}",
        scenario.name(),
        size_mb,
        iters,
        data_bytes,
        remote_bytes,
        percentile(&opens, 0.50),
        percentile(&opens, 0.95),
        percentile(&opens, 0.99),
        percentile(&totals, 0.50),
        percentile(&totals, 0.95),
        percentile(&totals, 0.99),
        percentile(&restores, 0.50),
        percentile(&restores, 0.95),
    );
}
