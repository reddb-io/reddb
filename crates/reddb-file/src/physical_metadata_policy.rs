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
