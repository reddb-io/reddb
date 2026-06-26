//! In-process, OS-portable power-cut + fault-injection recovery suite
//! (DST Fatia, #1355).
//!
//! This is the fast, portable counterpart to the `LD_PRELOAD` shim suite
//! (`power_cut_recovery.rs`): instead of spawning a process under a real libc
//! shim, it routes the representative WAL workload through the in-memory,
//! seed-driven [`SimVfs`], injects torn writes / dropped & reordered `fsync` /
//! `ENOSPC` / partial rename, materializes the post-crash device into a temp
//! dir, and asserts the **shared recovery-invariant oracle**
//! [`recover_and_check`]. Because everything is in-process and in-memory, this
//! lane runs on every OS and enumerates many more seeds per second than the
//! shim — a failing seed prints and reproduces exactly via `SEED=<n>`.

#![allow(clippy::unwrap_used)]

use reddb_file::SimulationContext;
use std::path::Path;
use unreliable_libc::vfs::POWER_CUT_MESSAGE;
use unreliable_libc::wal_workload::{MANIFEST_FILE_NAME, MANIFEST_TEMP_NAME};
use unreliable_libc::{
    decode_manifest, recover_and_check, run_wal_workload_on, RecoveryReport, SimFaultConfig, SimVfs,
};

/// Working directory inside the simulated device (paths are virtual).
const SIM_DIR: &str = "/db";
/// How many seeds to enumerate when `SEED` is not pinned.
const SEED_COUNT: u64 = 256;

/// Derive a fault config from the seed so each seed exercises a different
/// interleaving of every fault family, including a power-cut budget.
fn config_for(seed: u64) -> SimFaultConfig {
    SimFaultConfig {
        enospc_ppm: 30_000 + (seed % 7) * 10_000,         // 3%..9%
        drop_fsync_ppm: 40_000 + (seed % 5) * 12_000,     // 4%..~9%
        reorder_fsync_ppm: 50_000 + (seed % 3) * 20_000,  // 5%..9%
        revert_rename_ppm: 100_000 + (seed % 4) * 50_000, // 10%..25%
        torn_rename_ppm: 80_000 + (seed % 6) * 20_000,    // 8%..18%
        // Cut power somewhere inside the workload's durability syscalls.
        power_cut_after: Some(4 + seed % 60),
    }
}

/// Run one seed end to end: drive the workload on a fault-injecting `SimVfs`,
/// materialize the crash image, and assert the oracle. Panics with the seed
/// embedded so any failure reproduces via `SEED=<n>`.
fn run_seed(seed: u64, cfg: SimFaultConfig) -> RecoveryReport {
    let vfs = SimVfs::new(seed, cfg);
    // The workload returns Err on ENOSPC or the power-cut; both are expected
    // outcomes under fault injection — the oracle decides what landed on disk.
    match run_wal_workload_on(&vfs, Path::new(SIM_DIR), seed) {
        Ok(_) => {}
        Err(err) => {
            // Expected fault outcomes: a power-cut, an ENOSPC write failure (28),
            // or a dropped/reordered-fsync failure (EIO, 5). Anything else is a
            // real bug in the workload or the backend.
            let msg = err.to_string();
            let os = err.raw_os_error();
            assert!(
                msg.contains(POWER_CUT_MESSAGE) || os == Some(28) || os == Some(5),
                "SEED={seed}: unexpected workload error: {err}"
            );
        }
    }

    let crash_dir = tempfile::tempdir().unwrap();
    vfs.materialize(crash_dir.path()).unwrap();

    let report = match recover_and_check(crash_dir.path()) {
        Ok(report) => report,
        Err(err) => panic!(
            "recovery invariant violated for SEED={seed}: {err}\n\
             reproduce with: SEED={seed} cargo nextest run -p unreliable-libc --test sim_power_cut_recovery"
        ),
    };

    assert_manifest_consistent(seed, crash_dir.path(), &report);
    report
}

/// The atomically-renamed manifest must always be crash-consistent: it is the
/// old value, a new value no further ahead than the WAL's recovered frontier,
/// or absent/torn — never a fabricated commit ahead of what the WAL durably
/// holds. The staging temp must also never be mistaken for the live manifest.
fn assert_manifest_consistent(seed: u64, dir: &Path, report: &RecoveryReport) {
    let manifest = std::fs::read(dir.join(MANIFEST_FILE_NAME)).unwrap_or_default();
    if let Some((_generation, committed_lsn)) = decode_manifest(&manifest) {
        assert!(
            committed_lsn <= report.last_committed_lsn,
            "SEED={seed}: manifest committed_lsn {committed_lsn} ahead of WAL {}",
            report.last_committed_lsn
        );
    }
    // A leftover temp file is fine, but it is never the live manifest name.
    assert_ne!(MANIFEST_FILE_NAME, MANIFEST_TEMP_NAME);
}

/// Which seeds to enumerate: a single pinned seed when `SEED` is set
/// (reproduction), otherwise the full sweep.
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
fn power_cut_recovery_holds_across_seeds() {
    for seed in seeds() {
        let report = run_seed(seed, config_for(seed));
        // Sanity: the recovered superblock is never ahead of the WAL (already
        // enforced inside the oracle; asserted here as a guard).
        if let Some(sb_lsn) = report.superblock_committed_lsn {
            assert!(sb_lsn <= report.last_committed_lsn, "SEED={seed}");
        }
    }
}

#[test]
fn fault_free_sim_run_recovers_everything() {
    let vfs = SimVfs::new(123, SimFaultConfig::none());
    let outcome = run_wal_workload_on(&vfs, Path::new(SIM_DIR), 123).unwrap();
    let crash_dir = tempfile::tempdir().unwrap();
    vfs.materialize(crash_dir.path()).unwrap();
    let report = recover_and_check(crash_dir.path()).unwrap();
    assert_eq!(report.last_committed_lsn, outcome.last_committed_lsn);
    assert_eq!(report.torn_tail_bytes, 0);
    assert!(report.records_recovered > 0);
    assert_eq!(
        report.superblock_committed_lsn,
        Some(outcome.last_committed_lsn)
    );
    // A fault-free run leaves the manifest agreeing with the WAL frontier.
    let manifest = std::fs::read(crash_dir.path().join(MANIFEST_FILE_NAME)).unwrap();
    assert_eq!(
        decode_manifest(&manifest).map(|(_, lsn)| lsn),
        Some(outcome.last_committed_lsn)
    );
}

#[test]
fn same_seed_recovers_identically() {
    // The whole pipeline (faults + crash image + recovery) is a pure function
    // of the seed, so two runs produce the same recovered report.
    let a = run_seed(4242, config_for(4242));
    let b = run_seed(4242, config_for(4242));
    assert_eq!(a, b, "recovery must be reproducible by seed");
}

#[test]
fn enospc_alone_never_violates_invariants() {
    // ENOSPC stops the writer mid-transaction without a power-cut; recovery must
    // still hold (no torn record exposed as committed).
    let cfg = SimFaultConfig {
        enospc_ppm: 150_000,
        ..SimFaultConfig::none()
    };
    for seed in 0..64u64 {
        run_seed(seed, cfg);
    }
}

#[test]
fn dropped_and_reordered_fsync_never_resurrect_data() {
    let cfg = SimFaultConfig {
        drop_fsync_ppm: 200_000,
        reorder_fsync_ppm: 200_000,
        power_cut_after: Some(20),
        ..SimFaultConfig::none()
    };
    for seed in 0..64u64 {
        run_seed(seed, cfg);
    }
}

#[test]
fn same_seed_produces_byte_identical_buggify_trace_log() {
    fn trace_for(seed: u64) -> Vec<u8> {
        let context = SimulationContext::new(seed);
        let guard = context.install();
        let _ = run_seed(seed, config_for(seed));
        guard.trace()
    }

    let first = trace_for(4242);
    let second = trace_for(4242);
    assert_eq!(
        first, second,
        "same seed must produce byte-identical trace logs"
    );

    let trace = String::from_utf8(first).unwrap();
    assert!(trace.contains("env=REDDB_TURBO_CRASH_AT"));
    assert!(trace.contains("point=simvfs_"));
}

/// Pinned regression: a specific seed whose interleaving previously exercised a
/// mid-record power-cut alongside a reverted rename. Recovery must keep holding.
#[test]
fn pinned_regression_seed_1337() {
    let report = run_seed(1337, config_for(1337));
    if let Some(sb_lsn) = report.superblock_committed_lsn {
        assert!(sb_lsn <= report.last_committed_lsn);
    }
}
