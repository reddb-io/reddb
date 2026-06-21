//! Cluster bootstrap authority fail-closed seam (issue #1229, ADR 0058).
//!
//! These tests exercise the public seam through the two ways a boot becomes
//! cluster-shaped — the cluster *topology* (which defaults storage to the
//! cluster profile) and an explicit cluster *storage preset* — and prove
//! that:
//!
//!   * a credentialled cluster boot fails closed (no concrete authority
//!     owner can be proven), and
//!   * the explicit `--no-auth` / `--dev` development carveout stays
//!     allowed and resolves to the skip-all disposition.

use reddb::cluster::{authorize_cluster_bootstrap, AuthBootstrapInput, BootstrapDisposition};
use reddb::operational_bootstrap::{resolve_operational_bootstrap, OperationalBootstrapInput};
use reddb::storage::{DeployProfile, StorageDeployPreset};

/// Resolve the storage deploy profile a `REDDB_TOPOLOGY=cluster` boot lands
/// on, so the seam test mirrors the real boot path rather than hard-coding
/// the enum.
fn cluster_topology_profile() -> DeployProfile {
    let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
        topology: Some("cluster".to_string()),
        ..Default::default()
    })
    .expect("cluster topology resolves");
    plan.storage_profile.deploy_profile
}

#[test]
fn cluster_topology_path_is_cluster_shaped() {
    assert_eq!(cluster_topology_profile(), DeployProfile::Cluster);
}

#[test]
fn cluster_storage_preset_path_is_cluster_shaped() {
    assert_eq!(
        StorageDeployPreset::Cluster.selection().deploy_profile,
        DeployProfile::Cluster
    );
}

#[test]
fn cluster_topology_env_bootstrap_fails_closed() {
    let err = authorize_cluster_bootstrap(
        cluster_topology_profile(),
        false,
        AuthBootstrapInput::Env,
        false,
    )
    .expect_err("cluster topology must fail closed for env auth bootstrap");
    assert!(err.contains("no concrete authority owner"), "got: {err}");
}

#[test]
fn cluster_topology_manifest_bootstrap_fails_closed() {
    let err = authorize_cluster_bootstrap(
        cluster_topology_profile(),
        false,
        AuthBootstrapInput::Manifest,
        false,
    )
    .expect_err("cluster topology must fail closed for manifest auth bootstrap");
    assert!(err.contains("no concrete authority owner"), "got: {err}");
}

#[test]
fn cluster_storage_preset_env_bootstrap_fails_closed() {
    let profile = StorageDeployPreset::Cluster.selection().deploy_profile;
    let err = authorize_cluster_bootstrap(profile, false, AuthBootstrapInput::Env, false)
        .expect_err("cluster storage preset must fail closed for env auth bootstrap");
    assert!(err.contains("no concrete authority owner"), "got: {err}");
}

#[test]
fn cluster_storage_preset_manifest_bootstrap_fails_closed() {
    let profile = StorageDeployPreset::Cluster.selection().deploy_profile;
    let err = authorize_cluster_bootstrap(profile, false, AuthBootstrapInput::Manifest, false)
        .expect_err("cluster storage preset must fail closed for manifest auth bootstrap");
    assert!(err.contains("no concrete authority owner"), "got: {err}");
}

#[test]
fn cluster_no_auth_dev_bypass_is_allowed_and_skips_bootstrap() {
    // Both cluster-shaped paths keep the `--no-auth` / `--dev` carveout.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        let disposition =
            authorize_cluster_bootstrap(profile, true, AuthBootstrapInput::None, false).unwrap();
        assert_eq!(
            disposition,
            BootstrapDisposition::SkipDevBypass,
            "cluster --no-auth must skip every auth/bootstrap path"
        );
    }
}

#[test]
fn non_cluster_topology_still_bootstraps_locally() {
    let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
        topology: Some("standalone".to_string()),
        ..Default::default()
    })
    .expect("standalone topology resolves");
    let disposition = authorize_cluster_bootstrap(
        plan.storage_profile.deploy_profile,
        false,
        AuthBootstrapInput::Env,
        false,
    )
    .unwrap();
    assert_eq!(disposition, BootstrapDisposition::ProceedLocal);
}

#[test]
fn cluster_topology_reports_completion_instead_of_failing_closed() {
    // Issue #1230 — once a durable completion marker is visible through the
    // authority path, a cluster restart observes it (idempotent) rather than
    // failing closed. This is the only way a credentialled cluster boot
    // resolves to a non-error disposition today.
    let disposition = authorize_cluster_bootstrap(
        cluster_topology_profile(),
        false,
        AuthBootstrapInput::Manifest,
        true,
    )
    .expect("a completed cluster restart must not fail closed");
    assert_eq!(disposition, BootstrapDisposition::AlreadyComplete);
}

#[test]
fn duplicate_bootstrap_after_completion_is_idempotent_across_shapes() {
    // Issue #1230 — a duplicate bootstrap attempt after completion reports
    // the existing completed state for every cluster-shaped path and every
    // auth bootstrap input the operator re-supplies.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        for input in [
            AuthBootstrapInput::None,
            AuthBootstrapInput::Env,
            AuthBootstrapInput::Manifest,
        ] {
            let disposition =
                authorize_cluster_bootstrap(profile, false, input, true).unwrap();
            assert_eq!(
                disposition,
                BootstrapDisposition::AlreadyComplete,
                "duplicate bootstrap after completion must be idempotent"
            );
        }
    }
}

#[test]
fn non_cluster_restart_after_completion_is_idempotent() {
    // Issue #1230 — a standalone restart after a successful bootstrap also
    // observes the completion marker and recreates no global auth state.
    let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
        topology: Some("standalone".to_string()),
        ..Default::default()
    })
    .expect("standalone topology resolves");
    let disposition = authorize_cluster_bootstrap(
        plan.storage_profile.deploy_profile,
        false,
        AuthBootstrapInput::Env,
        true,
    )
    .unwrap();
    assert_eq!(disposition, BootstrapDisposition::AlreadyComplete);
}
