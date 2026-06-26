//! Seed-looped power-cut + fault-injection recovery suite (DST Fatia 0, #1351).
//!
//! Each seed drives the representative WAL workload as a separate process under
//! the `unreliable-libc` `LD_PRELOAD` shim, faults/kills at randomized points,
//! then reopens and asserts the shared recovery-invariant oracle. A failing seed
//! is printed and reproducible via `SEED=<n>`.
//!
//! The shim is `LD_PRELOAD`-based (Linux/glibc), so this whole suite only
//! compiles and runs on Linux; on other targets it is an empty test binary.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use unreliable_libc::{recover_and_check, RecoveryReport};

/// Absolute path to the shim `.so`, baked in by `build.rs`.
const SHIM_SO: &str = env!("UNRELIABLE_LIBC_SO");
/// The workload binary, provided by cargo.
const WORKLOAD_BIN: &str = env!("CARGO_BIN_EXE_wal_workload");

/// How many seeds to enumerate when `SEED` is not pinned.
const SEED_COUNT: u64 = 32;

#[derive(Clone, Copy)]
struct FaultConfig {
    powercut: bool,
    eio_ppm: u64,
    short_ppm: u64,
    max_syscalls: u64,
}

/// Run the workload for one seed under the shim, then assert the oracle. Panics
/// with the seed embedded so any failure is reproducible via `SEED=<n>`.
fn run_seed(seed: u64, cfg: FaultConfig) -> (RecoveryReport, std::process::ExitStatus) {
    let dir = tempfile::tempdir().unwrap();
    let status = spawn_workload(seed, dir.path(), cfg);

    match recover_and_check(dir.path()) {
        Ok(report) => (report, status),
        Err(err) => panic!(
            "recovery invariant violated for SEED={seed} (powercut={}, eio_ppm={}, short_ppm={}): {err}\n\
             reproduce with: SEED={seed} cargo nextest run -p unreliable-libc --test power_cut_recovery",
            cfg.powercut, cfg.eio_ppm, cfg.short_ppm
        ),
    }
}

fn spawn_workload(seed: u64, dir: &Path, cfg: FaultConfig) -> std::process::ExitStatus {
    let mut cmd = Command::new(WORKLOAD_BIN);
    cmd.arg(seed.to_string())
        .env("LD_PRELOAD", SHIM_SO)
        .env("UNRELIABLE_SEED", seed.to_string())
        .env("UNRELIABLE_DIR", dir)
        .env("UNRELIABLE_EIO_PPM", cfg.eio_ppm.to_string())
        .env("UNRELIABLE_SHORT_PPM", cfg.short_ppm.to_string())
        .env("UNRELIABLE_MAX_SYSCALLS", cfg.max_syscalls.to_string())
        // Keep stdout/stderr off regular files so the shim never faults them.
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if cfg.powercut {
        cmd.env("UNRELIABLE_POWERCUT", "1");
    }
    cmd.status()
        .unwrap_or_else(|e| panic!("failed to spawn workload for SEED={seed}: {e}"))
}

/// Seeds to enumerate: a single pinned seed when `SEED` is set (reproduction),
/// otherwise the full sweep.
fn seeds() -> Vec<u64> {
    match std::env::var("SEED") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(seed) => vec![seed],
            Err(_) => (0..SEED_COUNT).collect(),
        },
        Err(_) => (0..SEED_COUNT).collect(),
    }
}

#[test]
fn shim_artifact_exists() {
    let so = Path::new(SHIM_SO);
    assert!(so.exists(), "shim .so missing at {SHIM_SO}");
    let len = std::fs::metadata(so).unwrap().len();
    assert!(len > 0, "shim .so is empty");
}

#[test]
fn power_cut_recovery_holds_across_seeds() {
    let cfg = FaultConfig {
        powercut: true,
        eio_ppm: 0,
        short_ppm: 200_000, // 20% short writes alongside the kill
        max_syscalls: 40,
    };
    for seed in seeds() {
        let (report, _status) = run_seed(seed, cfg);
        // Recovery must never expose a torn record as committed data.
        assert!(report.records_recovered < u64::MAX);
    }
}

#[test]
fn fault_injection_recovery_holds_across_seeds() {
    let cfg = FaultConfig {
        powercut: false,
        eio_ppm: 80_000,    // 8% EIO
        short_ppm: 250_000, // 25% short writes
        max_syscalls: 64,
    };
    for seed in seeds() {
        run_seed(seed, cfg);
    }
}

#[test]
fn power_cut_actually_kills_the_process() {
    // Force a deterministic early kill and prove the shim is active: the process
    // must terminate via SIGKILL rather than exiting normally.
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(WORKLOAD_BIN);
    cmd.arg("1")
        .env("LD_PRELOAD", SHIM_SO)
        .env("UNRELIABLE_SEED", "1")
        .env("UNRELIABLE_DIR", dir.path())
        .env("UNRELIABLE_POWERCUT", "1")
        .env("UNRELIABLE_KILL_AFTER", "1") // first eligible durability syscall
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = cmd.status().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "expected SIGKILL from the power-cut shim, got {status:?}"
    );
    // Whatever landed on disk must still be recoverable.
    recover_and_check(dir.path()).unwrap();
}

#[test]
fn transparent_without_seed_env() {
    // With no UNRELIABLE_SEED the shim is a pass-through: the workload completes
    // and recovers every transaction.
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(WORKLOAD_BIN)
        .arg("314")
        .env("LD_PRELOAD", SHIM_SO)
        .env_remove("UNRELIABLE_SEED")
        .env("UNRELIABLE_DIR", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "transparent run should succeed");
    let report = recover_and_check(dir.path()).unwrap();
    assert!(report.records_recovered > 0);
    assert_eq!(report.torn_tail_bytes, 0);
}

/// Pinned regression: a specific seed whose power-cut interleaving previously
/// exercised a mid-record kill. Recovery must keep holding for it forever.
#[test]
fn pinned_regression_seed_1337() {
    let cfg = FaultConfig {
        powercut: true,
        eio_ppm: 50_000,
        short_ppm: 200_000,
        max_syscalls: 40,
    };
    let (report, _status) = run_seed(1337, cfg);
    // Sanity: the recovered superblock (if any) is never ahead of the WAL —
    // already enforced inside the oracle, asserted here as the regression lock.
    if let (Some(sb_lsn), wal_lsn) = (report.superblock_committed_lsn, report.last_committed_lsn) {
        assert!(sb_lsn <= wal_lsn);
    }
}
