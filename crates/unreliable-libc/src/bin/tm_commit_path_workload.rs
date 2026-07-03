//! Seeded TM commit-path workload binary, driven under `unreliable-libc`.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::ExitCode;
use unreliable_libc::{run_tm_commit_path_workload, TmCommitPathScenario};

fn main() -> ExitCode {
    let seed = resolve_seed();
    let scenario = match std::env::args().nth(2).as_deref() {
        Some("fcw_before_wal_append") => TmCommitPathScenario::FcwBeforeWalAppend,
        Some("wal_append_before_finalize") => TmCommitPathScenario::WalAppendBeforeFinalize,
        Some("savepoint_release_rollback") => TmCommitPathScenario::SavepointReleaseRollback,
        Some("concurrent_writers") => TmCommitPathScenario::ConcurrentWriters,
        Some(other) => {
            eprintln!("unknown TM commit-path scenario: {other}");
            return ExitCode::from(2);
        }
        None => TmCommitPathScenario::WalAppendBeforeFinalize,
    };

    println!("SEED={seed} scenario={}", scenario.name());

    let dir = match std::env::var_os("UNRELIABLE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("UNRELIABLE_DIR is required");
            return ExitCode::from(2);
        }
    };

    match run_tm_commit_path_workload(&dir, seed, scenario) {
        Ok(model) => {
            println!(
                "OK scenario={} committed={} wal_len={}",
                scenario.name(),
                model.txs.len(),
                model.wal_len
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("workload stopped on durability error: {err}");
            ExitCode::from(1)
        }
    }
}

fn resolve_seed() -> u64 {
    if let Some(arg) = std::env::args().nth(1) {
        if let Ok(seed) = arg.parse::<u64>() {
            return seed;
        }
    }
    for key in ["SEED", "UNRELIABLE_SEED"] {
        if let Ok(raw) = std::env::var(key) {
            if let Ok(seed) = raw.parse::<u64>() {
                return seed;
            }
        }
    }
    0
}
