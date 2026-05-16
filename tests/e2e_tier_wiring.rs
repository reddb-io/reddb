//! Issue "Tier-wiring meta-slice": `RedDBOptions::with_layout(...)` must
//! flip the six tier-flag global toggles at open time, materialise the
//! support directories the layout demands, and expose the resolved
//! audit / slow log destinations via `tier_wiring::current_log_destinations`.
//!
//! Verifies the *defaults per tier* table from `apply_tier_defaults`'s
//! doc comment:
//!
//! | toggle                     | minimal | standard | performance | max  |
//! |----------------------------|:-------:|:--------:|:-----------:|:----:|
//! | `.meta.json` sidecar       |   off   |    off   |     off     |  on  |
//! | seq-N catalog journal      |   off   |    off   |     off     |  on  |
//! | `-shm` provisioning        |   off   | **on**   |   **on**    | on   |
//! | `fold_pager_meta`          |   off   |    off   |     off     |  on  |
//! | `fold_dwb_into_wal`        |   off   |    off   |     off     |  on  |
//! | audit/slow log destination | stderr  |  stderr  |    file     | file |

use reddb::{
    fold_dwb_into_wal_enabled, fold_pager_meta_enabled, meta_json_sidecar_enabled,
    seqn_journal_enabled, seqn_journal_retention, shm_provisioning_enabled,
    tier_wiring, LayoutOverrides, LogDestination, RedDBOptions, RedDBRuntime,
    StorageLayout, DEFAULT_METADATA_JOURNAL_RETENTION, OPT_IN_METADATA_JOURNAL_RETENTION,
};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// All tests in this binary mutate process-global tier toggles. Serialise
/// them so a parallel runner doesn't observe a half-applied tier state.
static TIER_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_tier_{prefix}_{unique}.rdb"))
}

fn reset_env() {
    std::env::remove_var("REDDB_META_JSON_SIDECAR");
    std::env::remove_var("REDDB_SEQN_JOURNAL");
    std::env::remove_var("REDDB_SEQN_JOURNAL_RETENTION");
    std::env::remove_var("REDDB_SHM_PROVISION");
    std::env::remove_var("REDDB_FOLD_PAGER_META");
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");
}

fn open_at_layout(prefix: &str, layout: StorageLayout) -> (RedDBRuntime, PathBuf) {
    let path = persistent_path(prefix);
    let options = RedDBOptions::persistent(&path).with_layout(layout);
    let rt = RedDBRuntime::with_options(options).expect("runtime opens");
    (rt, path)
}

#[test]
fn minimal_tier_defaults_all_toggles_off() {
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_env();
    let (_rt, _path) = open_at_layout("minimal", StorageLayout::Minimal);

    assert!(!meta_json_sidecar_enabled(), "meta.json off for Minimal");
    assert!(!seqn_journal_enabled(), "seq-N journal off for Minimal");
    assert!(!shm_provisioning_enabled(), "-shm off for Minimal");
    assert!(!fold_pager_meta_enabled(), "fold_pager_meta off for Minimal");
    assert!(!fold_dwb_into_wal_enabled(), "fold_dwb off for Minimal");

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert_eq!(audit, LogDestination::Stderr, "minimal audit -> stderr");
    assert_eq!(slow, LogDestination::Stderr, "minimal slow -> stderr");
}

#[test]
fn standard_tier_provisions_shm_only() {
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_env();
    let (_rt, _path) = open_at_layout("standard", StorageLayout::Standard);

    assert!(!meta_json_sidecar_enabled(), "meta.json off for Standard");
    assert!(!seqn_journal_enabled(), "seq-N journal off for Standard");
    assert!(shm_provisioning_enabled(), "-shm ON for Standard");
    assert!(!fold_pager_meta_enabled(), "fold_pager_meta off for Standard");
    assert!(!fold_dwb_into_wal_enabled(), "fold_dwb off for Standard");
    assert_eq!(seqn_journal_retention(), OPT_IN_METADATA_JOURNAL_RETENTION);

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert_eq!(audit, LogDestination::Stderr);
    assert_eq!(slow, LogDestination::Stderr);
}

#[test]
fn performance_tier_routes_logs_to_file() {
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_env();
    let (_rt, _path) = open_at_layout("performance", StorageLayout::Performance);

    assert!(shm_provisioning_enabled(), "-shm ON for Performance");
    assert!(!meta_json_sidecar_enabled());
    assert!(!seqn_journal_enabled());

    let (audit, slow) = tier_wiring::current_log_destinations();
    let audit_path = audit.file_path().expect("performance audit -> file");
    let slow_path = slow.file_path().expect("performance slow -> file");
    assert!(audit_path.ends_with("logs/audit.log"));
    assert!(slow_path.ends_with("logs/slow.log"));

    // ensure_dirs was called — the parent of audit_path must exist.
    let logs_dir = audit_path.parent().unwrap();
    assert!(logs_dir.exists(), "logs dir {} created", logs_dir.display());
}

#[test]
fn max_tier_enables_all_toggles() {
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_env();
    let (_rt, _path) = open_at_layout("max", StorageLayout::Max);

    assert!(meta_json_sidecar_enabled(), "meta.json ON for Max");
    assert!(seqn_journal_enabled(), "seq-N journal ON for Max");
    assert!(shm_provisioning_enabled(), "-shm ON for Max");
    assert!(fold_pager_meta_enabled(), "fold_pager_meta ON for Max");
    assert!(fold_dwb_into_wal_enabled(), "fold_dwb ON for Max");
    assert_eq!(
        seqn_journal_retention(),
        DEFAULT_METADATA_JOURNAL_RETENTION,
        "Max retention = 32"
    );

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert!(matches!(audit, LogDestination::File(_)));
    assert!(matches!(slow, LogDestination::File(_)));
}

#[test]
fn layout_overrides_win_over_tier_default() {
    use reddb::LogRoutingOverrides;
    let _g = TIER_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_env();
    let path = persistent_path("overrides");
    let override_dest = LogDestination::Syslog;
    let overrides = LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(override_dest.clone()),
            slow_log: Some(override_dest.clone()),
        },
        ..LayoutOverrides::default()
    };
    let options = RedDBOptions::persistent(&path)
        .with_layout(StorageLayout::Performance)
        .with_layout_overrides(overrides);
    let _rt = RedDBRuntime::with_options(options).expect("runtime opens");

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert_eq!(audit, LogDestination::Syslog, "override beats tier default");
    assert_eq!(slow, LogDestination::Syslog);
}
