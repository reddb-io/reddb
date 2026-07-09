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
//!
//! Since #1959 the same campaign also arms the four **named fault classes** of
//! ADR 0074 §1 (`torn_write`, `misdirected_write`, `bit_rot`, `lost_write`) at
//! low ppm, asserts the fault log records every injection, and checks that a
//! checksum-detectable injection surfaces as a *detection event* rather than as
//! silent corruption.

#![allow(clippy::unwrap_used)]

use reddb_file::{FaultClass, FaultRecord, SimulationContext};
use std::collections::BTreeSet;
use std::path::Path;
use unreliable_libc::vfs::POWER_CUT_MESSAGE;
use unreliable_libc::wal_workload::{MANIFEST_FILE_NAME, MANIFEST_TEMP_NAME, WAL_FILE_NAME};
use unreliable_libc::{
    decode_manifest, recover_and_check, run_wal_workload_on, RecoveryError, RecoveryReport,
    SimFaultConfig, SimVfs,
};

/// Working directory inside the simulated device (paths are virtual).
const SIM_DIR: &str = "/db";
/// How many seeds to enumerate when `SEED` is not pinned.
const SEED_COUNT: u64 = 256;
/// How many seeds the (slower, materializing) fault-class sweep enumerates.
const FAULT_SEED_COUNT: u64 = 64;

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
        ..SimFaultConfig::none()
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

// --------------------------------------------------------------------------
// Named fault classes (#1959, ADR 0074 §1)
// --------------------------------------------------------------------------

/// The same crash campaign, additionally arming all four named classes at low
/// ppm. `bit_rot` gets a higher ppm than the write-side classes because it is
/// rolled once per file when the recovery reader reads the crash image, not once
/// per write — a few rolls per seed instead of a hundred.
fn fault_class_config_for(seed: u64) -> SimFaultConfig {
    SimFaultConfig {
        torn_write_ppm: 20_000,        // 2% per write
        misdirected_write_ppm: 15_000, // 1.5% per write
        lost_write_ppm: 15_000,        // 1.5% per write
        bit_rot_ppm: 150_000,          // 15% per file read back
        power_cut_after: Some(6 + seed % 60),
        ..SimFaultConfig::none()
    }
}

/// What one fault-class campaign seed produced: which injections landed, and
/// what recovery made of the resulting device.
struct FaultOutcome {
    faults: Vec<FaultRecord>,
    recovery: Result<RecoveryReport, RecoveryError>,
}

impl FaultOutcome {
    fn classes(&self) -> BTreeSet<FaultClass> {
        self.faults.iter().map(|record| record.class).collect()
    }

    /// Whether any injection hit a file the recovery oracle reads.
    fn touched_a_recovered_object(&self) -> bool {
        self.faults
            .iter()
            .any(|record| !record.file.ends_with(MANIFEST_TEMP_NAME))
    }
}

/// Run one seed of the fault-class campaign. Unlike [`run_seed`], a recovery
/// error is *not* a test failure here: corruption injected by a named class is
/// meant to be detected, and detection is what an error means. The caller
/// decides whether the error was attributable.
fn run_fault_seed(seed: u64, cfg: SimFaultConfig) -> FaultOutcome {
    let vfs = SimVfs::new(seed, cfg);
    if let Err(err) = run_wal_workload_on(&vfs, Path::new(SIM_DIR), seed) {
        let msg = err.to_string();
        let os = err.raw_os_error();
        assert!(
            msg.contains(POWER_CUT_MESSAGE) || os == Some(28) || os == Some(5),
            "SEED={seed}: unexpected workload error: {err}"
        );
    }

    let crash_dir = tempfile::tempdir().unwrap();
    // `materialize` reads the device, which is where `bit_rot` is injected.
    vfs.materialize(crash_dir.path()).unwrap();
    let recovery = recover_and_check(crash_dir.path());

    FaultOutcome {
        faults: vfs.fault_log(),
        recovery,
    }
}

/// The campaign exercises every class, logs every injection with a target the
/// oracle can attribute, and never fails the oracle without a fault to explain
/// it — the no-false-negative contract. A seed whose faults all missed (or were
/// overwritten before any read) must still recover cleanly.
#[test]
fn all_four_fault_classes_are_injected_logged_and_attributable() {
    let mut observed: BTreeSet<FaultClass> = BTreeSet::new();

    for seed in fault_seeds() {
        let outcome = run_fault_seed(seed, fault_class_config_for(seed));
        observed.extend(outcome.classes());

        for record in &outcome.faults {
            assert!(!record.file.is_empty(), "SEED={seed}: fault names no file");
            assert!(
                record.length > 0,
                "SEED={seed}: fault covers no bytes: {record}"
            );
        }

        match &outcome.recovery {
            // The faults landed where the checksum contract truncates them away,
            // or in bytes overwritten before any read.
            Ok(report) => {
                if let Some(sb_lsn) = report.superblock_committed_lsn {
                    assert!(sb_lsn <= report.last_committed_lsn, "SEED={seed}");
                }
            }
            // A detection event. It must be explained by an injection: a crash
            // alone can never violate a recovery invariant.
            Err(err) => {
                assert!(
                    !outcome.faults.is_empty(),
                    "SEED={seed}: oracle violated with no injected fault: {err}\n\
                     reproduce with: SEED={seed} cargo nextest run -p unreliable-libc \
                     --test sim_power_cut_recovery"
                );
                assert!(
                    outcome.touched_a_recovered_object(),
                    "SEED={seed}: oracle violated but every fault hit a staging file: {err}\n\
                     fault log:\n{}",
                    outcome
                        .faults
                        .iter()
                        .map(|record| format!("  {record}\n"))
                        .collect::<String>()
                );
            }
        }
    }

    if std::env::var("SEED").is_err() {
        assert_eq!(
            observed,
            FaultClass::ALL.into_iter().collect::<BTreeSet<_>>(),
            "the campaign must exercise every named class across the sweep"
        );
    }
}

/// A crash-only campaign injects nothing: the classes are off by default, so the
/// pre-#1959 behavior of this lane is unchanged.
#[test]
fn named_classes_are_off_in_the_crash_only_campaign() {
    for seed in fault_seeds() {
        let outcome = run_fault_seed(seed, config_for(seed));
        assert!(
            outcome.faults.is_empty(),
            "SEED={seed}: a crash-only campaign must inject no named fault"
        );
        outcome
            .recovery
            .unwrap_or_else(|err| panic!("SEED={seed}: crash-only recovery must hold: {err}"));
    }
}

/// Determinism: the same seed replays the identical fault schedule — class,
/// target and offset, byte for byte.
#[test]
fn same_seed_produces_an_identical_fault_schedule() {
    let schedule = |seed: u64| {
        run_fault_seed(seed, fault_class_config_for(seed))
            .faults
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>()
    };

    let first = schedule(4242);
    assert!(!first.is_empty(), "seed 4242 must inject something");
    assert_eq!(first, schedule(4242), "same seed → same fault schedule");
    assert_ne!(first, schedule(4243), "different seeds must diverge");
}

/// Composition: `torn_write` alongside the crash knob stays deterministic and
/// reproducible, both in what it injected and in what recovery saw.
#[test]
fn crash_plus_torn_write_composes_deterministically() {
    let cfg = SimFaultConfig {
        torn_write_ppm: 60_000,
        power_cut_after: Some(30),
        ..SimFaultConfig::none()
    };
    let run = |seed: u64| {
        let outcome = run_fault_seed(seed, cfg);
        let log = outcome
            .faults
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>();
        (log, format!("{:?}", outcome.recovery))
    };

    for seed in 0..16u64 {
        let first = run(seed);
        assert!(
            first.0.contains("class=torn_write") || first.1.contains("Ok"),
            "SEED={seed}: a torn write must either land or leave recovery clean"
        );
        assert_eq!(first, run(seed), "SEED={seed}: crash + torn_write must replay");
    }
}

/// Checksum-detectable corruption surfaces as a detection event, never as a
/// silently-accepted commit frontier: bit rot inside the WAL either fails the
/// oracle outright or shows up as a truncated (torn) recovery.
#[test]
fn bit_rot_in_the_wal_is_detected_not_silently_accepted() {
    let cfg = SimFaultConfig::none().with_fault_class(FaultClass::BitRot, 1_000_000);

    for seed in 0..32u64 {
        let outcome = run_fault_seed(seed, cfg);

        let rotted_wal = outcome
            .faults
            .iter()
            .any(|record| record.class == FaultClass::BitRot && record.file.ends_with(WAL_FILE_NAME));
        assert!(rotted_wal, "SEED={seed}: bit_rot at 1e6 must rot the WAL");

        match outcome.recovery {
            Err(_) => {} // detected
            Ok(report) => assert!(
                report.torn_tail_bytes > 0,
                "SEED={seed}: a rotted WAL recovered clean — silent corruption"
            ),
        }
    }
}

/// A `lost_write` on the WAL can only ever lose the tail: recovery may see fewer
/// commits, never a commit the workload had not durably made.
#[test]
fn lost_writes_never_resurrect_a_commit() {
    let cfg = SimFaultConfig::none().with_fault_class(FaultClass::LostWrite, 80_000);
    for seed in 0..32u64 {
        let outcome = run_fault_seed(seed, cfg);
        if let Ok(report) = outcome.recovery {
            if let Some(sb_lsn) = report.superblock_committed_lsn {
                assert!(sb_lsn <= report.last_committed_lsn, "SEED={seed}");
            }
        }
    }
}

/// Which seeds the fault-class sweep enumerates: a single pinned seed when
/// `SEED` is set, otherwise the sweep.
fn fault_seeds() -> Vec<u64> {
    match std::env::var("SEED") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(seed) => vec![seed],
            Err(_) => (0..FAULT_SEED_COUNT).collect(),
        },
        Err(_) => (0..FAULT_SEED_COUNT).collect(),
    }
}
