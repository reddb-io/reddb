//! Transaction-manager commit-path fault-injection campaign (#1651).

#![allow(clippy::unwrap_used)]

use std::path::Path;
use unreliable_libc::vfs::POWER_CUT_MESSAGE;
use unreliable_libc::{
    assert_tm_recovery_matches, recover_and_check, recover_tm_commit_path,
    run_tm_commit_path_workload, run_tm_commit_path_workload_on, SimFaultConfig, SimVfs,
    TmCommitPathScenario,
};

/// How many seeds to enumerate when `SEED` is not pinned.
const SEED_COUNT: u64 = 16;
const SIM_DIR: &str = "/db";

#[test]
fn tm_commit_path_campaign_enumerates_required_scenario_families() {
    let scenarios = unreliable_libc::tm_commit_path_scenarios();
    assert_eq!(
        scenarios,
        [
            "fcw_before_wal_append",
            "wal_append_before_finalize",
            "savepoint_release_rollback",
            "concurrent_writers",
        ]
    );
}

#[test]
fn deterministic_crash_offsets_preserve_tm_commit_atomicity() {
    for seed in seeds() {
        for scenario in TmCommitPathScenario::all() {
            let dir = tempfile::tempdir().unwrap();
            let model = run_tm_commit_path_workload(dir.path(), seed, scenario).unwrap();
            let wal = std::fs::read(dir.path().join("wal.log")).unwrap();

            for cut in crash_offsets(&model, wal.len()) {
                let crash_dir = tempfile::tempdir().unwrap();
                std::fs::write(crash_dir.path().join("wal.log"), &wal[..cut]).unwrap();
                recover_and_check(crash_dir.path()).unwrap_or_else(|err| {
                    panic!(
                        "structural oracle failed for SEED={seed} scenario={} cut={cut}: {err}",
                        scenario.name()
                    )
                });
                let recovered = recover_tm_commit_path(crash_dir.path()).unwrap();
                assert_tm_recovery_matches(seed, &model, &recovered);
            }
        }
    }
}

#[test]
fn simvfs_power_cuts_preserve_tm_commit_atomicity() {
    for seed in seeds() {
        for scenario in TmCommitPathScenario::all() {
            let model_dir = tempfile::tempdir().unwrap();
            let model = run_tm_commit_path_workload(model_dir.path(), seed, scenario).unwrap();

            let cfg = SimFaultConfig {
                power_cut_after: Some(power_cut_budget(scenario, seed)),
                ..SimFaultConfig::none()
            };
            let vfs = SimVfs::new(seed, cfg);
            match run_tm_commit_path_workload_on(&vfs, Path::new(SIM_DIR), seed, scenario) {
                Ok(_) => {}
                Err(err) => assert!(
                    err.to_string().contains(POWER_CUT_MESSAGE),
                    "SEED={seed} scenario={} unexpected workload error: {err}",
                    scenario.name()
                ),
            }

            let crash_dir = tempfile::tempdir().unwrap();
            vfs.materialize(crash_dir.path()).unwrap();
            recover_and_check(crash_dir.path()).unwrap_or_else(|err| {
                panic!(
                    "structural oracle failed for SEED={seed} scenario={}: {err}",
                    scenario.name()
                )
            });
            let recovered = recover_tm_commit_path(crash_dir.path()).unwrap();
            assert_tm_recovery_matches(seed, &model, &recovered);
        }
    }
}

#[test]
fn fault_free_run_recovers_the_full_tm_model() {
    for seed in seeds() {
        for scenario in TmCommitPathScenario::all() {
            let dir = tempfile::tempdir().unwrap();
            let model = run_tm_commit_path_workload(dir.path(), seed, scenario).unwrap();
            recover_and_check(dir.path()).unwrap();
            let recovered = recover_tm_commit_path(dir.path()).unwrap();
            assert_eq!(
                recovered,
                model.all_committed(),
                "SEED={seed} scenario={} did not recover the full model",
                scenario.name()
            );
        }
    }
}

fn crash_offsets(model: &unreliable_libc::TmCommitPathModel, wal_len: usize) -> Vec<usize> {
    let mut offsets = vec![0, reddb_file::wal_header::WAL_FILE_HEADER_BYTES, wal_len];
    for tx in &model.txs {
        let end = usize::try_from(tx.commit_end_offset).unwrap();
        offsets.push(end);
        if end > 0 {
            offsets.push(end - 1);
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

fn power_cut_budget(scenario: TmCommitPathScenario, seed: u64) -> u64 {
    match scenario {
        TmCommitPathScenario::FcwBeforeWalAppend => 2,
        TmCommitPathScenario::WalAppendBeforeFinalize => 5 + seed % 4,
        TmCommitPathScenario::SavepointReleaseRollback => 6 + seed % 6,
        TmCommitPathScenario::ConcurrentWriters => 5 + seed % 7,
    }
}

/// Seeds to enumerate: a single pinned seed when `SEED` is set (reproduction),
/// otherwise the documented sweep.
fn seeds() -> Vec<u64> {
    match std::env::var("SEED") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(seed) => vec![seed],
            Err(_) => documented_seeds(),
        },
        Err(_) => documented_seeds(),
    }
}

fn documented_seeds() -> Vec<u64> {
    (0..SEED_COUNT).collect()
}

// ----- Real power-cut under the LD_PRELOAD shim (Linux/glibc only) ---------

#[cfg(target_os = "linux")]
mod powercut {
    use super::*;
    use std::process::{Command, Stdio};

    /// Absolute path to the shim `.so`, baked in by `build.rs`.
    const SHIM_SO: &str = env!("UNRELIABLE_LIBC_SO");
    /// The TM commit-path workload binary, provided by cargo.
    const WORKLOAD_BIN: &str = env!("CARGO_BIN_EXE_tm_commit_path_workload");

    #[test]
    fn ld_preload_power_cut_preserves_tm_commit_atomicity() {
        for seed in seeds() {
            for scenario in TmCommitPathScenario::all() {
                let model_dir = tempfile::tempdir().unwrap();
                let model = run_tm_commit_path_workload(model_dir.path(), seed, scenario).unwrap();

                let crashed = tempfile::tempdir().unwrap();
                let _status = spawn_powercut(seed, scenario, crashed.path());
                recover_and_check(crashed.path()).unwrap_or_else(|err| {
                    panic!(
                        "structural oracle failed for SEED={seed} scenario={}: {err}",
                        scenario.name()
                    )
                });
                let recovered = recover_tm_commit_path(crashed.path()).unwrap();
                assert_tm_recovery_matches(seed, &model, &recovered);
            }
        }
    }

    fn spawn_powercut(
        seed: u64,
        scenario: TmCommitPathScenario,
        dir: &Path,
    ) -> std::process::ExitStatus {
        Command::new(WORKLOAD_BIN)
            .arg(seed.to_string())
            .arg(scenario.name())
            .env("LD_PRELOAD", SHIM_SO)
            .env("UNRELIABLE_SEED", seed.to_string())
            .env("UNRELIABLE_DIR", dir)
            .env("UNRELIABLE_POWERCUT", "1")
            .env("UNRELIABLE_SHORT_PPM", "200000")
            .env("UNRELIABLE_MAX_SYSCALLS", "48")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to spawn tm_commit_path_workload for SEED={seed} scenario={}: {e}",
                    scenario.name()
                )
            })
    }
}
