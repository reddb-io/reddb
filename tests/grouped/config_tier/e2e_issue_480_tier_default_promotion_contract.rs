//! Issue #480 — Promote feature flags to tier defaults (per-phase go/no-go).
//! PRD #467.
//!
//! This is a meta-issue tracking phased promotion of mature feature flags
//! from "opt-in via env hatch / `LayoutOverrides`" to ON-by-default on a
//! given tier. The promotion criteria (4 objective gates) and phase
//! grouping (A cosmetic, B pager substrate, C catalog placement) are
//! documented in ADR-0018 under the `gh-480` section.
//!
//! This test file anchors three contracts under issue #480:
//!
//!   1. The ADR-0018 promotion-criteria section is present and names the
//!      four gates (release count, perf benchmark, no urgent/incident,
//!      ADR addendum). A future edit that silently drops a gate would
//!      fail this test at the right boundary.
//!   2. The currently-promoted state matches the `apply_tier_defaults`
//!      truth table — Phase A live for performance/max, `-shm` on
//!      standard, `fold_pager_meta` / `fold_dwb_into_wal` / meta.json /
//!      seq-N journal still max-only pending their gates.
//!   3. The override-stability commitment ("override surface for a
//!      promoted flag remains available for at least 2 further releases")
//!      is encoded by the continued presence of the env hatch +
//!      `LayoutOverrides` entry for every promoted flag in the codebase.
//!      A drop without a deprecation slice would surface here.
//!
//! Each test maps to one acceptance bullet on the issue.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::{
    fold_dwb_into_wal_enabled, fold_pager_meta_enabled, meta_json_sidecar_enabled,
    seqn_journal_enabled, shm_provisioning_enabled, tier_wiring, LogDestination, RedDBOptions,
    RedDBRuntime, StorageLayout,
};

fn reset_env() {
    std::env::remove_var("REDDB_META_JSON_SIDECAR");
    std::env::remove_var("REDDB_SEQN_JOURNAL");
    std::env::remove_var("REDDB_SEQN_JOURNAL_RETENTION");
    std::env::remove_var("REDDB_SHM_PROVISION");
    std::env::remove_var("REDDB_FOLD_PAGER_META");
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");
}

fn open_at_layout(prefix: &str, layout: StorageLayout) -> (support::TempDbFile, RedDBRuntime) {
    let db = support::temp_db_file(prefix);
    let options = RedDBOptions::persistent(db.path()).with_layout(layout);
    let rt = RedDBRuntime::with_options(options).expect("runtime opens");
    (db, rt)
}

fn read_adr_0018() -> String {
    // Tests run from the workspace root.
    let path = std::path::Path::new(".red/adr/0018-tiered-storage-layout.md");
    std::fs::read_to_string(path).expect("ADR-0018 readable")
}

// ---------------------------------------------------------------------------
// Acceptance bullet 1: promotion criteria documented in ADR-0018.
// ---------------------------------------------------------------------------

#[test]
fn adr_0018_documents_four_objective_promotion_gates() {
    let adr = read_adr_0018();
    let section_anchor = "Promotion criteria for tier-default feature flags (gh-480)";
    assert!(
        adr.contains(section_anchor),
        "ADR-0018 must carry the gh-480 promotion criteria section"
    );

    // Gate 1: one full release with the flag as voluntary opt-in.
    assert!(
        adr.contains("One full release"),
        "gate 1 (release-count) must be documented"
    );
    // Gate 2: no perf regression on the standard benchmark set.
    assert!(
        adr.contains("No perf regression on the standard benchmark set"),
        "gate 2 (perf benchmark) must be documented"
    );
    // Gate 3: no open urgent/incident issue against the flag's surface.
    assert!(
        adr.contains("priority:urgent") && adr.contains("type:incident"),
        "gate 3 (no urgent/incident) must be documented"
    );
    // Gate 4: ADR addendum entry for the promotion decision.
    assert!(
        adr.contains("An ADR addendum"),
        "gate 4 (ADR addendum) must be documented"
    );
}

#[test]
fn adr_0018_documents_phase_grouping_a_b_c() {
    let adr = read_adr_0018();
    assert!(
        adr.contains("Phase A — cosmetic"),
        "Phase A bucket must be named"
    );
    assert!(
        adr.contains("Phase B — pager substrate"),
        "Phase B bucket must be named"
    );
    assert!(
        adr.contains("Phase C — catalog placement"),
        "Phase C bucket must be named"
    );
}

// ---------------------------------------------------------------------------
// Acceptance bullet 2: Phase A live default for performance/max (logs
// + cache routed into `<dbname>.rdb.red/` subdirs). Was promoted under
// gh-471 — this is the worked example.
// ---------------------------------------------------------------------------

#[test]
fn phase_a_performance_routes_logs_into_red_subdir() {
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();
    let _rt = open_at_layout("phaseA_perf", StorageLayout::Performance);

    let (audit, slow) = tier_wiring::current_log_destinations();
    let audit_path = audit.file_path().expect("performance audit -> file");
    let slow_path = slow.file_path().expect("performance slow -> file");
    assert!(
        audit_path.ends_with("logs/audit.log"),
        "performance audit log routed into <db>.rdb.red/logs/ — got {}",
        audit_path.display()
    );
    assert!(
        slow_path.ends_with("logs/slow.log"),
        "performance slow log routed into <db>.rdb.red/logs/ — got {}",
        slow_path.display()
    );
}

#[test]
fn phase_a_max_routes_logs_into_red_subdir() {
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();
    let _rt = open_at_layout("phaseA_max", StorageLayout::Max);

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert!(
        matches!(audit, LogDestination::File(_)),
        "max audit -> file"
    );
    assert!(matches!(slow, LogDestination::File(_)), "max slow -> file");
}

#[test]
fn phase_a_minimal_and_standard_keep_stderr() {
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();
    let _rt = open_at_layout("phaseA_min", StorageLayout::Minimal);
    let (audit_min, slow_min) = tier_wiring::current_log_destinations();
    assert_eq!(audit_min, LogDestination::Stderr);
    assert_eq!(slow_min, LogDestination::Stderr);

    let _rt2 = open_at_layout("phaseA_std", StorageLayout::Standard);
    let (audit_std, slow_std) = tier_wiring::current_log_destinations();
    assert_eq!(
        audit_std,
        LogDestination::Stderr,
        "Phase A not yet promoted on Standard — stderr remains the default"
    );
    assert_eq!(slow_std, LogDestination::Stderr);
}

// ---------------------------------------------------------------------------
// Acceptance bullet 3: Phase B partial promotion.
//   - `-shm`: promoted to Standard (gh-475). Anchored under #480 here.
//   - `fold_pager_meta` / `fold_dwb_into_wal`: NOT yet promoted to
//     Standard. ADR explicitly notes they remain Max-only pending gates 1
//     and 2 (the `fold_pager_meta` benchmark is the still-open dependency).
//   - `.meta.json` sidecar / seq-N journal: also Max-only.
// ---------------------------------------------------------------------------

#[test]
fn phase_b_shm_promoted_to_standard() {
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();
    let _rt = open_at_layout("phaseB_shm_std", StorageLayout::Standard);
    assert!(
        shm_provisioning_enabled(),
        "Phase B partial promotion: -shm ON for Standard"
    );
}

#[test]
fn phase_b_fold_pager_meta_remains_max_only() {
    let _g = crate::config_tier_shared::tier_state_lock();

    reset_env();
    let _rt_std = open_at_layout("phaseB_fpm_std", StorageLayout::Standard);
    assert!(
        !fold_pager_meta_enabled(),
        "fold_pager_meta must not be promoted to Standard before its gates clear"
    );

    reset_env();
    let _rt_perf = open_at_layout("phaseB_fpm_perf", StorageLayout::Performance);
    assert!(
        !fold_pager_meta_enabled(),
        "fold_pager_meta must not be promoted to Performance before its gates clear"
    );

    reset_env();
    let _rt_max = open_at_layout("phaseB_fpm_max", StorageLayout::Max);
    assert!(
        fold_pager_meta_enabled(),
        "fold_pager_meta still ON for Max — Max is the opt-in surface today"
    );
}

#[test]
fn phase_b_fold_dwb_into_wal_remains_max_only() {
    let _g = crate::config_tier_shared::tier_state_lock();

    reset_env();
    let _rt_std = open_at_layout("phaseB_fdw_std", StorageLayout::Standard);
    assert!(
        !fold_dwb_into_wal_enabled(),
        "fold_dwb_into_wal must not be promoted to Standard before its gates clear"
    );

    reset_env();
    let _rt_max = open_at_layout("phaseB_fdw_max", StorageLayout::Max);
    assert!(
        fold_dwb_into_wal_enabled(),
        "fold_dwb_into_wal still ON for Max"
    );
}

#[test]
fn meta_json_and_seqn_journal_remain_max_only() {
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();
    let _rt_std = open_at_layout("aux_std", StorageLayout::Standard);
    assert!(!meta_json_sidecar_enabled());
    assert!(!seqn_journal_enabled());

    reset_env();
    let _rt_max = open_at_layout("aux_max", StorageLayout::Max);
    assert!(meta_json_sidecar_enabled());
    assert!(seqn_journal_enabled());
}

// ---------------------------------------------------------------------------
// Acceptance bullet 4: Phase C — `embed_catalog_in_datafile`. The ADR
// explicitly states this flag has NOT been introduced yet ("named here
// as a forward placeholder ... cannot be promoted until it exists"). We
// pin that the ADR continues to flag this as pre-existence. The moment
// the flag lands, this test fails and the regression points whoever
// adds it back to the gates section.
// ---------------------------------------------------------------------------

#[test]
fn phase_c_embed_catalog_in_datafile_not_yet_introduced() {
    let adr = read_adr_0018();
    assert!(
        adr.contains("embed_catalog_in_datafile"),
        "Phase C placeholder must be named in ADR-0018"
    );
    assert!(
        adr.contains("has not been introduced yet") || adr.contains("forward placeholder"),
        "ADR must state that `embed_catalog_in_datafile` is a pre-existence \
         placeholder until the flag is actually introduced"
    );
}

// ---------------------------------------------------------------------------
// Acceptance bullet 5: override-stability commitment. Every promoted
// flag's `LayoutOverrides` surface must remain for at least two
// releases after promotion. We exercise it by routing Phase A logs to
// a non-default destination — the live promotion that ships an override
// surface today.
//
// Note: `-shm` does not expose a post-promotion opt-out via env hatch
// (the env var is read only as an opt-in when the policy hasn't been
// set explicitly, so `apply_tier_defaults(Standard)` consumes the
// switch). Today the rollback path for `-shm` is "select a lower
// tier". Whether a dedicated override field should land in
// `LayoutOverrides` is tracked under the next promotion-readiness
// review per ADR-0018; this test does not assert that surface.
// ---------------------------------------------------------------------------

#[test]
fn promoted_phase_a_log_routing_override_remains_available() {
    use reddb::{LayoutOverrides, LogRoutingOverrides};
    let _g = crate::config_tier_shared::tier_state_lock();
    reset_env();

    let override_dest = LogDestination::Syslog;
    let overrides = LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(override_dest.clone()),
            slow_log: Some(override_dest.clone()),
        },
        ..LayoutOverrides::default()
    };
    let db = support::temp_db_file("override_phaseA");
    let options = RedDBOptions::persistent(db.path())
        .with_layout(StorageLayout::Performance)
        .with_layout_overrides(overrides);
    let _rt = RedDBRuntime::with_options(options).expect("runtime opens");

    let (audit, slow) = tier_wiring::current_log_destinations();
    assert_eq!(
        audit,
        LogDestination::Syslog,
        "promoted Phase A routing must still accept LayoutOverrides"
    );
    assert_eq!(slow, LogDestination::Syslog);
}
