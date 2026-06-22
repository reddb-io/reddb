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

use reddb::auth::vault::{KeyPair, Vault, VaultState};
use reddb::auth::{Role, User};
use reddb::cluster::{
    authorize_cluster_bootstrap, authorize_vault_bootstrap, AuthBootstrapInput,
    BootstrapDisposition, VaultBootstrapPlan,
};
use reddb::operational_bootstrap::{resolve_operational_bootstrap, OperationalBootstrapInput};
use reddb::storage::engine::pager::{Pager, PagerConfig};
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
            let disposition = authorize_cluster_bootstrap(profile, false, input, true).unwrap();
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

// ----- Issue #1231: cluster vault first boot wired to the real auth store ---

#[test]
fn cluster_first_boot_vault_plan_fails_closed_for_every_input() {
    // Acceptance: a cluster-shaped first boot with no proven owner is rejected
    // before any vault plan is produced, so neither a scratch nor a
    // per-member-only vault is minted and no secret input is consumed by a
    // non-owner. Holds for both cluster-shaped paths and every input.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        for input in [
            AuthBootstrapInput::None,
            AuthBootstrapInput::Env,
            AuthBootstrapInput::Manifest,
        ] {
            let err = authorize_vault_bootstrap(profile, false, input, false).unwrap_err();
            assert!(
                err.contains("no concrete authority owner"),
                "cluster first-boot vault must fail closed, got: {err}"
            );
        }
    }
}

#[test]
fn completed_cluster_restart_unseals_real_store_without_secrets() {
    // Acceptance: once first boot has completed, a cluster restart is
    // authorized through the completion marker and lands on the unseal-only
    // plan against the real cluster-global auth store — no secret input is
    // consumed, so the vault is never re-minted or rotated.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        let plan =
            authorize_vault_bootstrap(profile, false, AuthBootstrapInput::Manifest, true).unwrap();
        assert_eq!(
            plan,
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: false,
            },
            "completed cluster restart must unseal the real store without secrets"
        );
    }
}

#[test]
fn non_cluster_owner_first_boot_opens_real_store_and_consumes_secrets() {
    // Acceptance: the owner path (here a non-cluster single authority, the
    // only shape that can prove ownership today) opens the vault against the
    // real store and consumes the env/`_FILE` secret inputs to mint the
    // certificate.
    let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
        topology: Some("standalone".to_string()),
        ..Default::default()
    })
    .expect("standalone topology resolves");
    let vault_plan = authorize_vault_bootstrap(
        plan.storage_profile.deploy_profile,
        false,
        AuthBootstrapInput::Env,
        false,
    )
    .unwrap();
    assert_eq!(
        vault_plan,
        VaultBootstrapPlan::OpenClusterGlobalStore {
            consume_secret_inputs: true,
        }
    );
}

#[test]
fn dev_bypass_cluster_boot_skips_the_vault() {
    // Acceptance: `--no-auth` / `--dev` cluster-shaped boot skips the vault
    // entirely — no certificate is minted and no store is opened.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        let plan =
            authorize_vault_bootstrap(profile, true, AuthBootstrapInput::None, false).unwrap();
        assert_eq!(plan, VaultBootstrapPlan::SkipNoVault);
    }
}

// ----- Issue #1235: duplicate and concurrent cluster bootstrap drills -------
//
// These drills exercise the supported cluster delivery shape under the two
// adversarial first-boot conditions PRD #1227 calls out — many symmetric
// members racing first boot, and a member re-attempting bootstrap after
// completion — and prove the four safety properties end to end:
//
//   1. Concurrent first-boot attempts produce exactly one owner path and no
//      divergent local auth/vault state on any other member.
//   2. A restart after a completed cluster bootstrap preserves the same
//      vault/certificate relationship, admin users, default policies, and
//      completion marker.
//   3. A duplicate bootstrap after completion is idempotent: it rotates no
//      credential and overwrites no config.
//   4. A cluster-shaped `--no-auth` boot stays anonymous: it creates no
//      admin, vault, or policy bootstrap state.

use std::sync::{Arc, Barrier};
use std::thread;

/// How many symmetric members race a single drill. Enough threads to make a
/// real owner-election race observable while staying cheap in CI.
const CLUSTER_MEMBERS: usize = 16;

/// Release every member thread at the same instant so the drill exercises a
/// genuine concurrent race rather than a staggered sequence.
fn race<T, F>(members: usize, body: F) -> Vec<T>
where
    T: Send + 'static,
    F: Fn(usize) -> T + Send + Sync + 'static,
{
    let barrier = Arc::new(Barrier::new(members));
    let body = Arc::new(body);
    let handles: Vec<_> = (0..members)
        .map(|member| {
            let barrier = Arc::clone(&barrier);
            let body = Arc::clone(&body);
            thread::spawn(move || {
                // All members line up, then boot together.
                barrier.wait();
                body(member)
            })
        })
        .collect();
    handles
        .into_iter()
        .map(|h| h.join().expect("member thread panicked"))
        .collect()
}

/// A throwaway real (pager-backed) cluster-global auth store, isolated per
/// drill so the certificate seal exercises the same store on restart — never
/// an emptyDir/scratch DB, the anti-goal PRD #1227 forbids.
fn cluster_vault_pager(label: &str) -> (Pager, std::path::PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_dir = std::env::temp_dir().join(format!(
        "reddb_cluster_drill_{label}_{}_{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let pager = Pager::open(&tmp_dir.join("cluster.rdb"), PagerConfig::default()).unwrap();
    (pager, tmp_dir)
}

/// The admin/policy state a completed cluster owner seals into the real store.
/// `kv` carries the default policy entries: the vault's encrypted key-value
/// store is the durable home for bootstrap-installed policy material.
fn completed_owner_state(certificate_master: &[u8]) -> VaultState {
    let admin = User {
        username: "admin".into(),
        tenant_id: None,
        password_hash: "argon2id$seal$admin".into(),
        scram_verifier: None,
        role: Role::Admin,
        api_keys: vec![],
        created_at: 1,
        updated_at: 1,
        enabled: true,
    };
    let operator = User {
        username: "operator".into(),
        tenant_id: None,
        password_hash: "argon2id$seal$operator".into(),
        scram_verifier: None,
        role: Role::Write,
        api_keys: vec![],
        created_at: 2,
        updated_at: 2,
        enabled: true,
    };
    let mut kv = std::collections::HashMap::new();
    kv.insert("red.secret.policy.default".into(), "allow-admin".into());
    kv.insert("red.secret.policy.readonly".into(), "deny-write".into());
    VaultState {
        users: vec![admin, operator],
        api_keys: vec![],
        bootstrapped: true,
        master_secret: Some(certificate_master.to_vec()),
        kv,
    }
}

#[test]
fn concurrent_cluster_first_boot_elects_no_owner_and_writes_no_divergent_state() {
    // Acceptance #1: many symmetric members race auth bootstrap on a
    // cluster-shaped first boot. No owner model exists, so no member can prove
    // it is the single writer — every concurrent attempt fails closed
    // identically. Because zero members reach a state-creating disposition, no
    // divergent local auth state can exist on any member.
    let profile = cluster_topology_profile();
    let outcomes = race(CLUSTER_MEMBERS, move |_member| {
        authorize_cluster_bootstrap(profile, false, AuthBootstrapInput::Env, false)
    });

    let owners = outcomes.iter().filter(|o| o.is_ok()).count();
    assert_eq!(
        owners, 0,
        "no symmetric member may self-elect as the bootstrap owner under a race"
    );
    for outcome in &outcomes {
        let err = outcome.as_ref().unwrap_err();
        assert!(
            err.contains("no concrete authority owner"),
            "every racing member must fail closed, got: {err}"
        );
    }
}

#[test]
fn concurrent_cluster_first_boot_vault_mints_no_scratch_and_consumes_no_secret() {
    // Acceptance #1 (vault): the same race against vault first boot. Each
    // member fails closed before any plan is produced, so no member mints a
    // scratch or per-member vault and no secret input is consumed by a
    // non-owner. Covers every auth bootstrap input a member might supply.
    let profile = cluster_topology_profile();
    for input in [
        AuthBootstrapInput::None,
        AuthBootstrapInput::Env,
        AuthBootstrapInput::Manifest,
    ] {
        let outcomes = race(CLUSTER_MEMBERS, move |_member| {
            authorize_vault_bootstrap(profile, false, input, false)
        });
        for outcome in &outcomes {
            let err = outcome.as_ref().unwrap_err();
            assert!(
                err.contains("no concrete authority owner"),
                "racing vault first boot must mint no scratch vault, got: {err}"
            );
        }
    }
}

#[test]
fn concurrent_attempts_after_completion_converge_idempotently() {
    // Acceptance #1 + #3: once the owner has published the durable completion
    // marker, a swarm of members (including the original owner's restart and
    // duplicate re-attempts) race the authority path. Every member observes
    // the one owner's completed state — `AlreadyComplete` — and the vault plan
    // is unseal-only, so no member rotates or re-mints the shared vault.
    let profile = cluster_topology_profile();
    let auth = race(CLUSTER_MEMBERS, move |_member| {
        authorize_cluster_bootstrap(profile, false, AuthBootstrapInput::Manifest, true)
    });
    for outcome in &auth {
        assert_eq!(
            outcome.as_ref().unwrap(),
            &BootstrapDisposition::AlreadyComplete,
            "every post-completion member must converge on the completed state"
        );
    }

    let vault = race(CLUSTER_MEMBERS, move |_member| {
        authorize_vault_bootstrap(profile, false, AuthBootstrapInput::Manifest, true)
    });
    for outcome in &vault {
        assert_eq!(
            outcome.as_ref().unwrap(),
            &VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: false,
            },
            "post-completion members unseal the shared store without rotating it"
        );
    }
}

#[test]
fn restart_after_cluster_bootstrap_preserves_vault_admins_policies_and_marker() {
    // Acceptance #2: a restart after a completed cluster bootstrap reopens the
    // same real cluster-global store with the emitted certificate and finds
    // the identical vault/certificate relationship, admin users, default
    // policies, master secret, and completion marker — nothing re-minted.
    let (pager, tmp_dir) = cluster_vault_pager("restart");

    // Owner first boot: mint the certificate and seal the real store.
    let kp = KeyPair::generate();
    let owner_state = completed_owner_state(&kp.master_secret);
    let vault = Vault::with_certificate_bytes(&pager, &kp.certificate).unwrap();
    vault.save(&pager, &owner_state).unwrap();

    // Restart lands on the unseal-only plan (no secret consumed → no rotation).
    let restart_plan = authorize_vault_bootstrap(
        cluster_topology_profile(),
        false,
        AuthBootstrapInput::Manifest,
        true,
    )
    .unwrap();
    assert_eq!(
        restart_plan,
        VaultBootstrapPlan::OpenClusterGlobalStore {
            consume_secret_inputs: false,
        }
    );

    // The same certificate unseals the same store; the sealed state is intact.
    let reopened = Vault::with_certificate(&pager, &kp.certificate_hex()).unwrap();
    let loaded = reopened.load(&pager).unwrap().unwrap();

    assert!(
        loaded.bootstrapped,
        "completion marker must survive restart"
    );
    assert_eq!(
        loaded.master_secret.as_deref(),
        Some(kp.master_secret.as_slice()),
        "the same certificate/master-secret relationship must survive restart"
    );

    let mut admins: Vec<_> = loaded
        .users
        .iter()
        .filter(|u| matches!(u.role, Role::Admin))
        .map(|u| u.username.clone())
        .collect();
    admins.sort();
    assert_eq!(admins, vec!["admin".to_string()], "admin user must persist");
    assert_eq!(loaded.users.len(), 2, "no bootstrap user may be dropped");

    assert_eq!(
        loaded
            .kv
            .get("red.secret.policy.default")
            .map(String::as_str),
        Some("allow-admin"),
        "default policy material must persist unchanged"
    );
    assert_eq!(
        loaded
            .kv
            .get("red.secret.policy.readonly")
            .map(String::as_str),
        Some("deny-write"),
        "default policy material must persist unchanged"
    );

    drop(pager);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn duplicate_bootstrap_after_completion_rotates_no_credential_and_overwrites_no_config() {
    // Acceptance #3: a duplicate bootstrap re-attempt after completion resolves
    // to the unseal-only plan and therefore consumes no secret input. The
    // certificate is not rotated and the sealed config (users + policies) is
    // not overwritten — the duplicate boot observes byte-identical state.
    let (pager, tmp_dir) = cluster_vault_pager("duplicate");

    let kp = KeyPair::generate();
    let original = completed_owner_state(&kp.master_secret);
    let vault = Vault::with_certificate_bytes(&pager, &kp.certificate).unwrap();
    vault.save(&pager, &original).unwrap();

    // Duplicate bootstrap attempt: operator re-supplies a manifest on a boot
    // that already completed. The plan must not consume the re-supplied secret.
    let dup_plan = authorize_vault_bootstrap(
        cluster_topology_profile(),
        false,
        AuthBootstrapInput::Manifest,
        true,
    )
    .unwrap();
    assert_eq!(
        dup_plan,
        VaultBootstrapPlan::OpenClusterGlobalStore {
            consume_secret_inputs: false,
        },
        "a duplicate boot must not consume secret inputs (no rotation)"
    );

    // Because no secret is consumed, the existing certificate still unseals the
    // store and the persisted state is unchanged from first boot.
    let reopened = Vault::with_certificate(&pager, &kp.certificate_hex()).unwrap();
    let after = reopened.load(&pager).unwrap().unwrap();
    assert_eq!(
        after.master_secret.as_deref(),
        Some(kp.master_secret.as_slice()),
        "duplicate boot must not rotate the certificate/master secret"
    );
    // Compare sealed config field-by-field (the serialized form orders the KV
    // map non-deterministically, so a byte compare would flake).
    assert!(after.bootstrapped, "completion marker must not be cleared");
    let user_ident = |s: &VaultState| {
        let mut idents: Vec<_> = s
            .users
            .iter()
            .map(|u| (u.username.clone(), u.role.as_str(), u.enabled))
            .collect();
        idents.sort();
        idents
    };
    assert_eq!(
        user_ident(&after),
        user_ident(&original),
        "duplicate boot must overwrite no admin/operator user"
    );
    assert_eq!(
        after.kv, original.kv,
        "duplicate boot must overwrite no default policy material"
    );

    drop(pager);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn cluster_no_auth_boot_creates_no_admin_vault_or_policy_state() {
    // Acceptance #4: a cluster-shaped `--no-auth` boot stays intentionally
    // anonymous across both cluster-shaped paths and under a concurrent race.
    // The auth disposition skips every bootstrap path and the vault plan opens
    // no store, so no admin, vault, or policy state is ever created.
    for profile in [
        cluster_topology_profile(),
        StorageDeployPreset::Cluster.selection().deploy_profile,
    ] {
        let auth = race(CLUSTER_MEMBERS, move |_member| {
            authorize_cluster_bootstrap(profile, true, AuthBootstrapInput::None, false)
        });
        for outcome in &auth {
            assert_eq!(
                outcome.as_ref().unwrap(),
                &BootstrapDisposition::SkipDevBypass,
                "cluster --no-auth must skip every auth/bootstrap path"
            );
        }

        let vault = race(CLUSTER_MEMBERS, move |_member| {
            authorize_vault_bootstrap(profile, true, AuthBootstrapInput::None, false)
        });
        for outcome in &vault {
            assert_eq!(
                outcome.as_ref().unwrap(),
                &VaultBootstrapPlan::SkipNoVault,
                "cluster --no-auth must open no vault and mint no certificate"
            );
        }
    }
}
