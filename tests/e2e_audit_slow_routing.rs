//! gh-471 iter 2: opening a runtime at `Performance` tier must wire
//! the `AuditLogger` and `SlowQueryLogger` to the `LogDestination::File(...)`
//! paths the layout resolves. Files land under `<dbname>.rdb.red/logs/`.
//!
//! Iter 1 only proved that `tier_wiring::current_log_destinations()`
//! reported a `File(...)` destination for Performance / Max. This test
//! closes the loop: the runtime *consumes* that destination, so the
//! actual log files exist on disk after `RedDBRuntime::with_options`.

use std::sync::Mutex;
use std::time::Duration;

use reddb::{
    tier_wiring, LayoutOverrides, LogDestination, LogRoutingOverrides, RedDBOptions, RedDBRuntime,
    StorageLayout,
};

#[allow(dead_code)]
mod support;

// Tier toggles + global audit sink are process-globals — serialise the
// two routing tests so they don't observe each other's state.
static TIER_GUARD: Mutex<()> = Mutex::new(());

#[test]
fn performance_tier_creates_slow_log_file_under_support_dir() {
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("audit-slow-perf");
    let options = RedDBOptions::persistent(data.path()).with_layout(StorageLayout::Performance);
    let _rt = RedDBRuntime::with_options(options).expect("runtime opens");

    let (audit_dest, slow_dest) = tier_wiring::current_log_destinations();
    let audit_path = audit_dest
        .file_path()
        .expect("Performance audit -> File(_)")
        .to_path_buf();
    let slow_path = slow_dest
        .file_path()
        .expect("Performance slow -> File(_)")
        .to_path_buf();

    // SlowQueryLogger opens the file synchronously on construction —
    // the file must exist the moment the runtime returns.
    assert!(
        slow_path.exists(),
        "slow log file missing at {}",
        slow_path.display()
    );
    assert!(
        slow_path.ends_with("logs/slow.log"),
        "slow log not under logs/: {}",
        slow_path.display()
    );

    // AuditLogger opens its file inside a writer thread; give it a
    // moment to spawn + create the file before asserting.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !audit_path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        audit_path.exists(),
        "audit log file missing at {}",
        audit_path.display()
    );
    assert!(
        audit_path.ends_with("logs/audit.log"),
        "audit log not under logs/: {}",
        audit_path.display()
    );

    // Both files live in the same support tree (<dbname>.rdb.red/logs/).
    assert_eq!(audit_path.parent(), slow_path.parent());
}

#[test]
fn syslog_override_falls_back_without_panicking() {
    // Syslog is a documented stub (ADR 0018): runtime must open without
    // panicking even when the resolved destination is Syslog. The audit
    // / slow sinks fall back to a file-on-disk so events still survive.
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("audit-slow-syslog");
    let overrides = LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(LogDestination::Syslog),
            slow_log: Some(LogDestination::Syslog),
        },
        ..LayoutOverrides::default()
    };
    let options = RedDBOptions::persistent(data.path())
        .with_layout(StorageLayout::Performance)
        .with_layout_overrides(overrides);
    let _rt = RedDBRuntime::with_options(options).expect("runtime opens with syslog override");
}
