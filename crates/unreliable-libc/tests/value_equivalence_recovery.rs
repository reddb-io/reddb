//! Write → persist → recover value-level equivalence suite (DST #1356, pillar B).
//!
//! Each seed drives the typed-value workload, which persists values spanning
//! every supported [`reddb_types::Value`] variant. The WAL is then crashed two
//! ways — an in-process truncation (the deterministic, exact-equivalence path)
//! and, on Linux, a real power-cut under the `unreliable-libc` `LD_PRELOAD` shim
//! — and the recovered committed values are asserted equal to the pre-crash
//! committed state. This layers value equivalence on top of the structural
//! recovery invariants from #1351 ([`recover_and_check`]). A failing seed is
//! printed and reproducible via `SEED=<n>`.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use unreliable_libc::{
    recover_and_check, recover_committed_values, run_typed_workload, TypedModel,
};

/// How many seeds to enumerate when `SEED` is not pinned.
const SEED_COUNT: u64 = 24;

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

/// Recover a fresh directory holding only `wal_bytes`, returning the committed
/// values. A bare WAL (no superblock) keeps the structural oracle from flagging
/// an unrealistically-ahead superblock when we truncate the WAL by hand.
fn recover_truncated(wal_bytes: &[u8]) -> Vec<unreliable_libc::RecoveredTx> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("wal.log"), wal_bytes).unwrap();
    // The structural oracle must also hold on the truncated prefix.
    recover_and_check(dir.path()).unwrap();
    recover_committed_values(dir.path()).unwrap()
}

#[test]
fn fault_free_run_recovers_every_committed_value() {
    for seed in seeds() {
        let dir = tempfile::tempdir().unwrap();
        let model = run_typed_workload(dir.path(), seed).unwrap();
        recover_and_check(dir.path())
            .unwrap_or_else(|err| panic!("structural oracle failed for SEED={seed}: {err}"));
        let recovered = recover_committed_values(dir.path()).unwrap();
        assert_eq!(
            recovered,
            model.all_committed(),
            "value equivalence failed for SEED={seed}; reproduce with SEED={seed}"
        );
    }
}

#[test]
fn truncated_crash_recovers_exactly_the_committed_prefix() {
    for seed in seeds() {
        let dir = tempfile::tempdir().unwrap();
        let model = run_typed_workload(dir.path(), seed).unwrap();
        let wal_bytes = std::fs::read(dir.path().join("wal.log")).unwrap();

        // Crash at every transaction's commit boundary, one byte before it (mid
        // commit record), and a few seed-derived interior offsets.
        for cut in crash_offsets(&model, &wal_bytes, seed) {
            let recovered = recover_truncated(&wal_bytes[..cut]);
            assert_eq!(
                recovered,
                model.committed_through(cut as u64),
                "value equivalence broke at SEED={seed} cut={cut}; reproduce with SEED={seed}"
            );
        }
    }
}

/// A spread of crash offsets to exercise: every commit boundary, the byte just
/// before each commit (a torn commit record), and seed-chosen interior bytes.
fn crash_offsets(model: &TypedModel, wal_bytes: &[u8], seed: u64) -> Vec<usize> {
    let mut offsets = Vec::new();
    for tx in &model.txs {
        let end = usize::try_from(tx.commit_end_offset).unwrap();
        offsets.push(end);
        if end > 0 {
            offsets.push(end - 1);
        }
    }
    // A handful of deterministic interior cuts derived from the seed.
    let len = wal_bytes.len().max(1) as u64;
    for k in 0..4u64 {
        let mixed = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(k.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        offsets.push(usize::try_from(mixed % len).unwrap_or(0));
    }
    offsets.push(wal_bytes.len());
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

// ----- Real power-cut under the LD_PRELOAD shim (Linux/glibc only) -----------

#[cfg(target_os = "linux")]
mod powercut {
    use super::*;
    use std::process::{Command, Stdio};

    /// Absolute path to the shim `.so`, baked in by `build.rs`.
    const SHIM_SO: &str = env!("UNRELIABLE_LIBC_SO");
    /// The typed workload binary, provided by cargo.
    const WORKLOAD_BIN: &str = env!("CARGO_BIN_EXE_typed_workload");

    fn spawn_powercut(seed: u64, dir: &Path) -> std::process::ExitStatus {
        Command::new(WORKLOAD_BIN)
            .arg(seed.to_string())
            .env("LD_PRELOAD", SHIM_SO)
            .env("UNRELIABLE_SEED", seed.to_string())
            .env("UNRELIABLE_DIR", dir)
            .env("UNRELIABLE_POWERCUT", "1")
            .env("UNRELIABLE_SHORT_PPM", "200000") // 20% short writes alongside the kill
            .env("UNRELIABLE_MAX_SYSCALLS", "40")
            // Keep stdout/stderr off regular files so the shim never faults them.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("failed to spawn typed_workload for SEED={seed}: {e}"))
    }

    #[test]
    fn power_cut_recovers_a_committed_value_prefix() {
        for seed in seeds() {
            // The deterministic full model for this seed: whatever the crashed
            // run persisted must be a value-for-value prefix of it.
            let model_dir = tempfile::tempdir().unwrap();
            let model = run_typed_workload(model_dir.path(), seed).unwrap();
            let full = model.all_committed();

            let crashed = tempfile::tempdir().unwrap();
            let _status = spawn_powercut(seed, crashed.path());

            // Structural invariants must hold after the real crash.
            recover_and_check(crashed.path())
                .unwrap_or_else(|err| panic!("structural oracle failed for SEED={seed}: {err}"));

            let recovered = recover_committed_values(crashed.path()).unwrap();
            assert!(
                recovered.len() <= full.len(),
                "recovered more committed txs than the model for SEED={seed}"
            );
            assert_eq!(
                recovered.as_slice(),
                &full[..recovered.len()],
                "recovered committed values are not a prefix of the model for SEED={seed}; \
                 reproduce with SEED={seed}"
            );
        }
    }
}
