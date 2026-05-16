//! Issue gh-478 iter 2: OLTP throughput benchmark comparing
//!  - DWB OFF (legacy `-dwb` sidecar fsync-on-every-write)
//!  - DWB ON  (no sidecar; WAL-only durability path)
//!
//! Acceptance target: < 20% regression on a small-transaction workload.
//! The benchmark deliberately uses the simplest measuring harness
//! (Instant + ratio) rather than criterion because the umbrella test
//! crate already pulls integration deps and we don't want a new
//! `[[bench]]` entry on the hot test path.
//!
//! Run:
//!   CARGO_TARGET_DIR=.target-gh478-iter2 \
//!     cargo test --release --test fold_dwb_into_wal_bench \
//!       -- --ignored --nocapture
//!
//! The test is `#[ignore]` because perf measurement does not belong
//! in the default test cadence (noisy, slow, host-dependent). The
//! relative ratio is still printed deterministically for the iter 2
//! acceptance note.

use reddb::{set_fold_dwb_into_wal_enabled, RedDBOptions, RedDBRuntime};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static POLICY_GUARD: Mutex<()> = Mutex::new(());

const TX_COUNT: usize = 200;

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-dwb", "-hdr", "-meta"] {
        let mut p = path.to_path_buf().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
    let mut wal = path.to_path_buf();
    wal.set_extension("wal");
    let _ = std::fs::remove_file(&wal);
}

fn run_workload(path: &Path) -> std::time::Duration {
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .expect("persistent runtime opens");
    rt.execute_query("CREATE TABLE oltp (n INTEGER, label TEXT)")
        .expect("ddl");
    // Warm-up: 5 inserts + checkpoint so the first txn cost (file
    // initialisation, header writes) is not part of the measured window.
    for i in 0..5 {
        rt.execute_query(&format!(
            "INSERT INTO oltp (n, label) VALUES ({i}, 'warm-{i}')"
        ))
        .expect("warm");
    }
    rt.checkpoint().expect("warm flush");

    let start = Instant::now();
    for i in 0..TX_COUNT {
        rt.execute_query(&format!(
            "INSERT INTO oltp (n, label) VALUES ({i}, 'r-{i}')"
        ))
        .expect("insert");
        // Every 10th insert triggers a checkpoint — keeps the page
        // write path active, which is where the DWB sidecar (OFF) or
        // WAL-only path (ON) actually differ.
        if i % 10 == 9 {
            rt.checkpoint().expect("flush");
        }
    }
    rt.checkpoint().expect("final flush");
    start.elapsed()
}

#[test]
#[ignore = "perf bench — run explicitly with --ignored --nocapture"]
fn fold_dwb_into_wal_oltp_overhead_under_20pct() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());

    // Baseline: DWB OFF (legacy sidecar fsync path).
    set_fold_dwb_into_wal_enabled(false);
    let off_path = persistent_path("bench_off");
    cleanup(&off_path);
    let off = run_workload(&off_path);
    cleanup(&off_path);

    // Treatment: DWB folded into WAL.
    set_fold_dwb_into_wal_enabled(true);
    let on_path = persistent_path("bench_on");
    cleanup(&on_path);
    let on = run_workload(&on_path);
    cleanup(&on_path);

    set_fold_dwb_into_wal_enabled(false);

    let ratio = on.as_secs_f64() / off.as_secs_f64();
    eprintln!(
        "[gh-478 bench] tx_count={TX_COUNT} off={off:?} on={on:?} ratio(on/off)={ratio:.3}"
    );

    // Acceptance: ON must not be >20% slower than OFF. ON faster is
    // fine — that's a win, not a regression. We assert the upper
    // bound only.
    assert!(
        ratio <= 1.20,
        "fold_dwb_into_wal=ON regression {ratio:.3}x exceeds 20% gate",
    );
}
