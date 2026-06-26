//! The seeded WAL workload binary, driven under the `unreliable-libc` shim.
//!
//! Usage (the integration test wires these up):
//! ```text
//! LD_PRELOAD=libunreliable_libc.so \
//! UNRELIABLE_SEED=<n> UNRELIABLE_POWERCUT=1 UNRELIABLE_DIR=<dir> \
//!   wal_workload <seed>
//! ```
//!
//! The working directory comes from `UNRELIABLE_DIR`; the seed comes from the
//! first CLI argument (falling back to `SEED`, then `UNRELIABLE_SEED`). The seed
//! is printed to stdout first so a failing run is reproducible via `SEED=<n>`.
//! Any durability error surfaced by the shim (`EIO`) exits non-zero; a power-cut
//! (`SIGKILL`) terminates the process outright.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::ExitCode;
use unreliable_libc::run_wal_workload;

fn main() -> ExitCode {
    let seed = resolve_seed();
    // Printed before any durable work so the harness can always read the seed.
    println!("SEED={seed}");

    let dir = match std::env::var_os("UNRELIABLE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("UNRELIABLE_DIR is required");
            return ExitCode::from(2);
        }
    };

    match run_wal_workload(&dir, seed) {
        Ok(outcome) => {
            println!(
                "OK committed={} lsn={} checkpoints={}",
                outcome.transactions_committed, outcome.last_committed_lsn, outcome.checkpoints
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            // An injected EIO/short-write made a durable call fail. This is an
            // expected outcome under fault injection; the oracle decides
            // whether what landed on disk is recoverable.
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
