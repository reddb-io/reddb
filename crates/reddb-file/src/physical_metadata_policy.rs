//! Process-wide policy knobs for physical metadata sidecars.
//!
//! Runtime crates decide the active storage tier; this module owns how that
//! tier maps to file-artifact emission policy.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// Retention applied when the seq-N catalog journal is enabled at the `Max`
/// tier. See [`seqn_journal_retention`].
pub const DEFAULT_METADATA_JOURNAL_RETENTION: usize = 32;
/// Retention applied when the seq-N catalog journal is opt-in enabled outside
/// of the `Max` tier, keeping forensics surface minimal on lower tiers.
pub const OPT_IN_METADATA_JOURNAL_RETENTION: usize = 4;

// JSON sidecar policy. 0 = unset (consult env, default off), 1 = enabled,
// 2 = disabled. Threaded as a process-global because metadata saves are reached
// from many call sites that do not currently carry a layout handle.
static META_JSON_SIDECAR_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for the legacy `<data>.meta.json` sidecar.
pub fn set_meta_json_sidecar_enabled(enabled: bool) {
    META_JSON_SIDECAR_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether new metadata writes should additionally emit the JSON sidecar.
/// Defaults to `false`; opt-in via [`set_meta_json_sidecar_enabled`] or the
/// `REDDB_META_JSON_SIDECAR=1` env var. Reads always tolerate either JSON or
/// binary.
pub fn meta_json_sidecar_enabled() -> bool {
    match META_JSON_SIDECAR_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => env_flag("REDDB_META_JSON_SIDECAR"),
    }
}

// Seq-N catalog journal policy. 0 = unset (consult env, default off), 1 =
// enabled, 2 = disabled. Mirrors the meta-json sidecar toggle but governs the
// `<data>.meta.rdbx.seq-{N}` forensic trail emitted on every metadata save.
static SEQN_JOURNAL_POLICY: AtomicU8 = AtomicU8::new(0);
// Retention override. 0 = unset (consult env, default off-tier retention).
static SEQN_JOURNAL_RETENTION: AtomicUsize = AtomicUsize::new(0);

/// Process-wide opt-in for the seq-N catalog journal.
pub fn set_seqn_journal_enabled(enabled: bool) {
    SEQN_JOURNAL_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether new metadata saves should also emit a seq-N journal entry.
pub fn seqn_journal_enabled() -> bool {
    match SEQN_JOURNAL_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => env_flag("REDDB_SEQN_JOURNAL"),
    }
}

// Pager-meta sidecar policy (#477). 0 = unset (consult env, default off: keep
// `<data>-meta` shadow), 1 = enabled (fold meta into page 1 + overflow chain;
// no `-meta` sidecar), 2 = disabled (current behavior).
static FOLD_PAGER_META_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for folding pager metadata (page 1) into the datafile
/// without an adjacent `<data>-meta` shadow.
pub fn set_fold_pager_meta_enabled(enabled: bool) {
    FOLD_PAGER_META_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether the pager should fold metadata into page 1 only and skip the
/// `<data>-meta` sidecar shadow. Reads still tolerate the sidecar so existing
/// databases keep working through the flag flip.
pub fn fold_pager_meta_enabled() -> bool {
    match FOLD_PAGER_META_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => env_flag("REDDB_FOLD_PAGER_META"),
    }
}

// Fold-DWB-into-WAL policy (#478). 0 = unset (consult env, default off: keep
// `-dwb` sidecar), 1 = enabled (emit FullPageImage WAL records before first
// page modification per checkpoint cycle; no `-dwb` sidecar), 2 = disabled.
static FOLD_DWB_INTO_WAL_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for folding the double-write buffer into the WAL via
/// full-page-image records.
pub fn set_fold_dwb_into_wal_enabled(enabled: bool) {
    FOLD_DWB_INTO_WAL_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether the pager should fold DWB into WAL (no `<data>-dwb` sidecar).
/// Reads still tolerate the legacy sidecar so existing databases keep working
/// through the flag flip.
pub fn fold_dwb_into_wal_enabled() -> bool {
    match FOLD_DWB_INTO_WAL_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => env_flag("REDDB_FOLD_DWB_INTO_WAL"),
    }
}

/// Process-wide retention for the seq-N journal. `0` resets to defaults (env
/// or off-tier baseline).
pub fn set_seqn_journal_retention(retention: usize) {
    SEQN_JOURNAL_RETENTION.store(retention, Ordering::Relaxed);
}

/// Resolved retention bound for the seq-N journal. Falls back to env
/// `REDDB_SEQN_JOURNAL_RETENTION`, then to
/// [`OPT_IN_METADATA_JOURNAL_RETENTION`].
pub fn seqn_journal_retention() -> usize {
    let stored = SEQN_JOURNAL_RETENTION.load(Ordering::Relaxed);
    if stored > 0 {
        return stored;
    }
    std::env::var("REDDB_SEQN_JOURNAL_RETENTION")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(OPT_IN_METADATA_JOURNAL_RETENTION)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
fn reset_physical_metadata_policy_for_test() {
    META_JSON_SIDECAR_POLICY.store(0, Ordering::Relaxed);
    SEQN_JOURNAL_POLICY.store(0, Ordering::Relaxed);
    FOLD_PAGER_META_POLICY.store(0, Ordering::Relaxed);
    FOLD_DWB_INTO_WAL_POLICY.store(0, Ordering::Relaxed);
    SEQN_JOURNAL_RETENTION.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static POLICY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(name: &str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }

    fn remove_env(name: &str) {
        unsafe {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn env_flags_and_explicit_overrides_drive_sidecar_policies() {
        let _guard = POLICY_TEST_LOCK.lock().unwrap();
        reset_physical_metadata_policy_for_test();

        for value in ["1", "true", "TRUE", "yes", "on"] {
            set_env("REDDB_META_JSON_SIDECAR", value);
            assert!(meta_json_sidecar_enabled(), "{value}");
        }
        set_env("REDDB_META_JSON_SIDECAR", "false");
        assert!(!meta_json_sidecar_enabled());

        set_meta_json_sidecar_enabled(true);
        set_env("REDDB_META_JSON_SIDECAR", "false");
        assert!(meta_json_sidecar_enabled());
        set_meta_json_sidecar_enabled(false);
        set_env("REDDB_META_JSON_SIDECAR", "1");
        assert!(!meta_json_sidecar_enabled());
        remove_env("REDDB_META_JSON_SIDECAR");
    }

    #[test]
    fn journal_and_fold_policy_overrides_are_independent() {
        let _guard = POLICY_TEST_LOCK.lock().unwrap();
        reset_physical_metadata_policy_for_test();

        set_env("REDDB_SEQN_JOURNAL", "1");
        assert!(seqn_journal_enabled());
        set_seqn_journal_enabled(false);
        assert!(!seqn_journal_enabled());
        set_seqn_journal_enabled(true);
        assert!(seqn_journal_enabled());
        remove_env("REDDB_SEQN_JOURNAL");

        set_env("REDDB_FOLD_PAGER_META", "yes");
        assert!(fold_pager_meta_enabled());
        set_fold_pager_meta_enabled(false);
        assert!(!fold_pager_meta_enabled());
        set_fold_pager_meta_enabled(true);
        assert!(fold_pager_meta_enabled());
        remove_env("REDDB_FOLD_PAGER_META");

        set_env("REDDB_FOLD_DWB_INTO_WAL", "on");
        assert!(fold_dwb_into_wal_enabled());
        set_fold_dwb_into_wal_enabled(false);
        assert!(!fold_dwb_into_wal_enabled());
        set_fold_dwb_into_wal_enabled(true);
        assert!(fold_dwb_into_wal_enabled());
        remove_env("REDDB_FOLD_DWB_INTO_WAL");
    }

    #[test]
    fn seqn_journal_retention_prefers_override_then_env_then_default() {
        let _guard = POLICY_TEST_LOCK.lock().unwrap();
        reset_physical_metadata_policy_for_test();
        remove_env("REDDB_SEQN_JOURNAL_RETENTION");

        assert_eq!(seqn_journal_retention(), OPT_IN_METADATA_JOURNAL_RETENTION);

        set_env("REDDB_SEQN_JOURNAL_RETENTION", "12");
        assert_eq!(seqn_journal_retention(), 12);
        set_env("REDDB_SEQN_JOURNAL_RETENTION", "0");
        assert_eq!(seqn_journal_retention(), OPT_IN_METADATA_JOURNAL_RETENTION);
        set_env("REDDB_SEQN_JOURNAL_RETENTION", "bad");
        assert_eq!(seqn_journal_retention(), OPT_IN_METADATA_JOURNAL_RETENTION);

        set_seqn_journal_retention(99);
        assert_eq!(seqn_journal_retention(), 99);
        set_seqn_journal_retention(0);
        assert_eq!(seqn_journal_retention(), OPT_IN_METADATA_JOURNAL_RETENTION);

        remove_env("REDDB_SEQN_JOURNAL_RETENTION");
    }
}
