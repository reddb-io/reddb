//! Cluster bootstrap authority — fail-closed seam (ADR 0058).
//!
//! Cluster first boot needs a single authority for global auth, vault,
//! config, and policy. ADR 0058 makes that authority the reserved global
//! system range owner, fenced by lease/term and ownership epoch. No
//! concrete owner model is implemented yet, so this module is only the
//! runtime *seam*: it decides whether a cluster-shaped boot is allowed to
//! perform auth bootstrap, and fails closed whenever no concrete owner can
//! be proven.
//!
//! Three outcomes are deliberately preserved while the owner model is absent:
//!
//! * A boot that observes a durable, already-published bootstrap completion
//!   marker through the authority path returns
//!   [`BootstrapDisposition::AlreadyComplete`] *before* any other check.
//!   First boot is over: restarts and duplicate attempts are idempotent and
//!   must not recreate admins, reissue the vault, or reapply mutable config
//!   over operator changes (issue #1230). This holds for every shape,
//!   including a cluster, so a once-bootstrapped cluster observes completion
//!   instead of failing closed.
//! * Explicit `--no-auth` / `--dev` cluster-shaped boots remain allowed as
//!   a development carveout and skip every auth/bootstrap path. They must
//!   create no admin, vault, or bootstrap-complete state.
//! * Every other cluster-shaped boot that would create auth/bootstrap state
//!   (preset env, credentials, or a manifest) is rejected, because a
//!   symmetric member cannot prove that it — and not a peer — is the single
//!   writer of global auth state.
//!
//! Both the cluster *topology* default and an explicit cluster *storage
//! preset* resolve to [`DeployProfile::Cluster`], so the deploy profile is
//! the single signal this seam reads for "cluster-shaped".

use crate::storage::DeployProfile;

/// The kind of auth bootstrap a boot is requesting, as classified from the
/// CLI/env contract. Used only to render a precise denial message; the
/// fail-closed decision does not depend on which variant is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthBootstrapInput {
    /// No auth bootstrap requested (e.g. the `simple` preset with no
    /// credentials and no manifest). A non-owner still must not write a
    /// per-node bootstrap-complete marker, so this is fail-closed too on a
    /// cluster-shaped boot.
    None,
    /// Auth bootstrap requested through the environment/preset surface:
    /// the `production`/`cloud`/`regulated` presets, or
    /// `REDDB_USERNAME` + `REDDB_PASSWORD`.
    Env,
    /// Auth bootstrap requested through `REDDB_BOOTSTRAP_MANIFEST`.
    Manifest,
}

impl AuthBootstrapInput {
    const fn describe(self) -> &'static str {
        match self {
            Self::None => "no explicit auth bootstrap input",
            Self::Env => "auth bootstrap env/preset input",
            Self::Manifest => "auth bootstrap manifest input",
        }
    }
}

/// What the boot path should do with auth bootstrap, once the authority
/// seam has authorized it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapDisposition {
    /// Proceed with ordinary local single-owner bootstrap. The local node
    /// is the sole authority for its own auth state (standalone,
    /// serverless, or primary-replica).
    ProceedLocal,
    /// Skip every auth/bootstrap path. Reached only by an explicit
    /// `--no-auth` / `--dev` boot; for a cluster shape this is the
    /// documented development carveout.
    SkipDevBypass,
    /// First boot already completed: a durable bootstrap completion marker
    /// is visible through the authority path. The caller must treat this as
    /// idempotent — rehydrate read-only state, but recreate no users,
    /// reissue no vault certificate, and reapply no mutable config over
    /// operator changes (issue #1230). Restarts and duplicate bootstrap
    /// attempts after completion land here, including on a cluster shape.
    AlreadyComplete,
}

/// `true` when this boot is cluster-shaped for the purpose of auth
/// bootstrap authority. Both the cluster topology default and an explicit
/// cluster storage preset land on [`DeployProfile::Cluster`].
pub const fn is_cluster_shaped(deploy_profile: DeployProfile) -> bool {
    matches!(deploy_profile, DeployProfile::Cluster)
}

/// Decide whether a boot may perform auth bootstrap.
///
/// Returns [`BootstrapDisposition`] when the boot is allowed to continue,
/// or an operator-facing error string when the cluster bootstrap authority
/// fails closed.
///
/// * When `already_completed` is `true` a durable bootstrap completion
///   marker is visible through the authority path, so this returns
///   [`BootstrapDisposition::AlreadyComplete`] before any other check. First
///   boot is over: the caller must be idempotent and recreate no global auth
///   state (issue #1230). This wins even for a cluster shape, so a restart of
///   a once-bootstrapped cluster observes completion instead of failing
///   closed.
/// * Any `--no-auth` / `--dev` boot returns [`BootstrapDisposition::SkipDevBypass`]:
///   the caller skips all auth/bootstrap state. For cluster shapes this is
///   the explicit development carveout from ADR 0058.
/// * A non-cluster boot returns [`BootstrapDisposition::ProceedLocal`]: the
///   local node is the only authority for its own auth state.
/// * A cluster-shaped, non-`--no-auth` boot with no completion marker is
///   rejected. There is no reserved global system range owner model yet, so
///   no member can prove it is the single writer of global
///   auth/vault/config/policy state.
pub fn authorize(
    deploy_profile: DeployProfile,
    no_auth: bool,
    input: AuthBootstrapInput,
    already_completed: bool,
) -> Result<BootstrapDisposition, String> {
    if already_completed {
        // The durable completion marker is the authority path's record that
        // first boot already produced global auth state. Observing it must
        // never recreate users, reissue the vault, or reapply mutable config
        // over operator changes — and it must short-circuit the fail-closed
        // gate, which only guards the *first* write of that state. Duplicate
        // bootstrap attempts after completion therefore report the existing
        // completed state idempotently (issue #1230).
        return Ok(BootstrapDisposition::AlreadyComplete);
    }

    if no_auth {
        // `--no-auth` / `--dev` is the last word on auth for this boot
        // (issue #663). The caller skips every preset/credential path, so
        // no admin, vault, or bootstrap marker is created — exactly the
        // cluster development carveout ADR 0058 keeps open.
        return Ok(BootstrapDisposition::SkipDevBypass);
    }

    if !is_cluster_shaped(deploy_profile) {
        return Ok(BootstrapDisposition::ProceedLocal);
    }

    // Cluster-shaped, credentialled boot. ADR 0058 requires the reserved
    // global system range owner — fenced by lease/term + ownership epoch —
    // before any member may create admins, initialize vault material,
    // install policy, apply a manifest, or publish the bootstrap-complete
    // marker. No owner model is implemented, so no member can prove
    // ownership: fail closed instead of letting a symmetric member create
    // divergent global auth state.
    Err(format!(
        "cluster bootstrap authority: refusing to run auth bootstrap on a \
         cluster-shaped boot ({}) — no concrete authority owner is available. \
         The reserved global system range owner (ADR 0058) is not yet \
         implemented, so no member can prove it is the single writer of \
         global auth/vault/config/policy state. Use --no-auth / --dev for a \
         development cluster, or run auth bootstrap on a non-cluster topology.",
        input.describe(),
    ))
}

/// Where cluster vault first boot must create or open its vault, and whether
/// the owner path may consume env/`_FILE` secret inputs.
///
/// Issue #1231 wires vault first boot through the bootstrap authority so the
/// vault, key material, and emitted certificate belong to the *real*
/// cluster-global auth store the authority model selected — never a scratch
/// or per-member-only database, which PRD #1227 explicitly forbids ("do not
/// mint a certificate from an emptyDir/scratch database and apply it to a
/// different real store").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultBootstrapPlan {
    /// Create or open the vault against the cluster-global auth store. The
    /// boot is the proven authority owner (or the only local authority), so
    /// the vault pages, key material, and certificate live in the real store
    /// and the certificate unseals that same store on restart.
    ///
    /// `consume_secret_inputs` is `true` only on the first write
    /// ([`BootstrapDisposition::ProceedLocal`]): the owner path reads the
    /// env/`_FILE` secret inputs, mints the certificate, and seals the real
    /// store. A restart that observes the durable completion marker
    /// ([`BootstrapDisposition::AlreadyComplete`]) sets it to `false`: the
    /// existing vault is opened and unsealed, but no secret input is consumed,
    /// so first boot is never re-run and the vault is never rotated. Because
    /// a non-owner cluster boot fails closed in [`authorize`] *before* any
    /// plan is produced, secret inputs are never consumed by a non-owner.
    OpenClusterGlobalStore { consume_secret_inputs: bool },
    /// Skip every vault/auth path — the explicit `--no-auth` / `--dev`
    /// development carveout. No vault is created or opened and no certificate
    /// is minted.
    SkipNoVault,
}

/// Map an authorized [`BootstrapDisposition`] to its cluster vault first-boot
/// plan. This is the single place that decides the vault target store and
/// whether secret inputs feed the owner path; callers must not re-derive it.
pub const fn plan_vault_bootstrap(disposition: BootstrapDisposition) -> VaultBootstrapPlan {
    match disposition {
        // First write of global auth state: the owner consumes secret inputs
        // and seals the real cluster-global store.
        BootstrapDisposition::ProceedLocal => VaultBootstrapPlan::OpenClusterGlobalStore {
            consume_secret_inputs: true,
        },
        // Restart after completion: open and unseal the existing real store,
        // but consume no secret input — never re-mint or rotate the vault.
        BootstrapDisposition::AlreadyComplete => VaultBootstrapPlan::OpenClusterGlobalStore {
            consume_secret_inputs: false,
        },
        // `--no-auth` / `--dev`: skip the vault entirely.
        BootstrapDisposition::SkipDevBypass => VaultBootstrapPlan::SkipNoVault,
    }
}

/// Authorize cluster vault first boot end to end: run the bootstrap authority
/// gate, then map the authorized disposition to its [`VaultBootstrapPlan`].
///
/// A cluster-shaped first boot with no proven owner fails closed in
/// [`authorize`] *before* any plan is produced, so no scratch or per-member
/// vault is ever minted and no secret input is consumed by a non-owner
/// (issue #1231). The owner path ([`VaultBootstrapPlan::OpenClusterGlobalStore`])
/// is the only outcome that creates or opens vault material, and it always
/// targets the real cluster-global auth store.
pub fn authorize_vault_bootstrap(
    deploy_profile: DeployProfile,
    no_auth: bool,
    input: AuthBootstrapInput,
    already_completed: bool,
) -> Result<VaultBootstrapPlan, String> {
    authorize(deploy_profile, no_auth, input, already_completed).map(plan_vault_bootstrap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_cluster_profiles_proceed_locally() {
        for profile in [
            DeployProfile::Embedded,
            DeployProfile::Serverless,
            DeployProfile::PrimaryReplica,
        ] {
            assert!(!is_cluster_shaped(profile), "{profile:?} is not cluster");
            assert_eq!(
                authorize(profile, false, AuthBootstrapInput::Env, false).unwrap(),
                BootstrapDisposition::ProceedLocal,
                "{profile:?} should proceed with local bootstrap"
            );
        }
    }

    #[test]
    fn cluster_no_auth_is_the_dev_bypass_carveout() {
        let disposition = authorize(
            DeployProfile::Cluster,
            true,
            AuthBootstrapInput::None,
            false,
        )
        .unwrap();
        assert_eq!(disposition, BootstrapDisposition::SkipDevBypass);
    }

    #[test]
    fn non_cluster_no_auth_also_skips() {
        let disposition = authorize(
            DeployProfile::Embedded,
            true,
            AuthBootstrapInput::Env,
            false,
        )
        .unwrap();
        assert_eq!(disposition, BootstrapDisposition::SkipDevBypass);
    }

    #[test]
    fn cluster_env_bootstrap_fails_closed() {
        let err = authorize(
            DeployProfile::Cluster,
            false,
            AuthBootstrapInput::Env,
            false,
        )
        .unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
        assert!(err.contains("env/preset"), "got: {err}");
    }

    #[test]
    fn cluster_manifest_bootstrap_fails_closed() {
        let err = authorize(
            DeployProfile::Cluster,
            false,
            AuthBootstrapInput::Manifest,
            false,
        )
        .unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
        assert!(err.contains("manifest"), "got: {err}");
    }

    #[test]
    fn cluster_without_explicit_input_still_fails_closed() {
        // A `simple`-preset cluster boot writes only a per-node
        // bootstrap-complete marker, which ADR 0058 forbids without a
        // proven owner. Fail closed so no divergent marker is written.
        let err = authorize(
            DeployProfile::Cluster,
            false,
            AuthBootstrapInput::None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
    }

    #[test]
    fn completion_marker_makes_local_restart_idempotent() {
        // Acceptance #2: restart after a successful non-cluster bootstrap
        // observes the durable completion marker and must not recreate
        // global auth state.
        for profile in [
            DeployProfile::Embedded,
            DeployProfile::Serverless,
            DeployProfile::PrimaryReplica,
        ] {
            assert_eq!(
                authorize(profile, false, AuthBootstrapInput::Env, true).unwrap(),
                BootstrapDisposition::AlreadyComplete,
                "{profile:?} restart should be idempotent once completed"
            );
        }
    }

    #[test]
    fn completion_marker_short_circuits_cluster_fail_closed() {
        // Acceptance #1/#3: once first boot has completed, a cluster restart
        // observes completion through the authority path instead of failing
        // closed, even though no concrete owner model exists yet. This is
        // the only path that lets a credentialled cluster boot succeed.
        let disposition = authorize(
            DeployProfile::Cluster,
            false,
            AuthBootstrapInput::Manifest,
            true,
        )
        .unwrap();
        assert_eq!(disposition, BootstrapDisposition::AlreadyComplete);
    }

    #[test]
    fn duplicate_bootstrap_after_completion_is_idempotent_for_every_input() {
        // Acceptance #3: a duplicate bootstrap attempt after completion
        // reports the existing completed state regardless of which auth
        // bootstrap input the operator re-supplies.
        for input in [
            AuthBootstrapInput::None,
            AuthBootstrapInput::Env,
            AuthBootstrapInput::Manifest,
        ] {
            assert_eq!(
                authorize(DeployProfile::Cluster, false, input, true).unwrap(),
                BootstrapDisposition::AlreadyComplete,
                "{input:?} duplicate after completion should be idempotent"
            );
        }
    }

    #[test]
    fn completion_marker_wins_over_dev_bypass() {
        // The durable completion marker is checked before the `--no-auth`
        // carveout, so a once-bootstrapped node never silently downgrades
        // into the anonymous dev path on restart.
        let disposition =
            authorize(DeployProfile::Cluster, true, AuthBootstrapInput::None, true).unwrap();
        assert_eq!(disposition, BootstrapDisposition::AlreadyComplete);
    }

    // ----- Issue #1231: cluster vault first boot wired to the real store ----

    #[test]
    fn owner_first_boot_plan_opens_real_store_and_consumes_secrets() {
        // ProceedLocal is the proven-owner first write: the vault is opened
        // against the real cluster-global store and the env/`_FILE` secret
        // inputs feed the certificate-minting owner path.
        assert_eq!(
            plan_vault_bootstrap(BootstrapDisposition::ProceedLocal),
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: true,
            }
        );
    }

    #[test]
    fn restart_after_completion_unseals_real_store_without_consuming_secrets() {
        // AlreadyComplete reopens the same real store and unseals it with the
        // existing certificate, but consumes no secret input, so the vault is
        // never re-minted or rotated on restart.
        assert_eq!(
            plan_vault_bootstrap(BootstrapDisposition::AlreadyComplete),
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: false,
            }
        );
    }

    #[test]
    fn dev_bypass_plan_skips_the_vault() {
        assert_eq!(
            plan_vault_bootstrap(BootstrapDisposition::SkipDevBypass),
            VaultBootstrapPlan::SkipNoVault
        );
    }

    #[test]
    fn cluster_first_boot_mints_no_vault_and_consumes_no_secret() {
        // Acceptance: a cluster-shaped first boot with no proven owner fails
        // closed before any plan is produced, so neither a scratch nor a
        // per-member vault is minted and no secret input is consumed by a
        // non-owner. This holds for every credentialled input.
        for input in [
            AuthBootstrapInput::None,
            AuthBootstrapInput::Env,
            AuthBootstrapInput::Manifest,
        ] {
            let err =
                authorize_vault_bootstrap(DeployProfile::Cluster, false, input, false).unwrap_err();
            assert!(err.contains("no concrete authority owner"), "got: {err}");
        }
    }

    #[test]
    fn non_cluster_owner_gets_real_store_vault_plan() {
        for profile in [
            DeployProfile::Embedded,
            DeployProfile::Serverless,
            DeployProfile::PrimaryReplica,
        ] {
            assert_eq!(
                authorize_vault_bootstrap(profile, false, AuthBootstrapInput::Env, false).unwrap(),
                VaultBootstrapPlan::OpenClusterGlobalStore {
                    consume_secret_inputs: true,
                },
                "{profile:?} owner should open the real store and consume secrets"
            );
        }
    }

    #[test]
    fn completed_cluster_restart_gets_unseal_only_plan() {
        // Once a cluster has bootstrapped, a restart is authorized through the
        // completion marker and lands on the unseal-only plan against the real
        // store — the only way a credentialled cluster boot opens a vault.
        assert_eq!(
            authorize_vault_bootstrap(
                DeployProfile::Cluster,
                false,
                AuthBootstrapInput::Manifest,
                true,
            )
            .unwrap(),
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: false,
            }
        );
    }

    // The next two tests prove the runtime consequence of the owner plan
    // against a real pager-backed store: a freshly minted certificate seals
    // the real store and unseals that same store on restart, while a
    // scratch/per-member certificate cannot unseal it. The vault here is the
    // real cluster-global auth store's pager, never an emptyDir scratch DB.

    fn vault_test_pager() -> (crate::storage::engine::pager::Pager, std::path::PathBuf) {
        use crate::storage::engine::pager::{Pager, PagerConfig};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_dir =
            std::env::temp_dir().join(format!("reddb_cluster_vault_{}_{}", std::process::id(), id));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let pager = Pager::open(&tmp_dir.join("cluster.rdb"), PagerConfig::default()).unwrap();
        (pager, tmp_dir)
    }

    #[test]
    fn owner_certificate_unseals_the_same_store_on_restart() {
        use crate::auth::vault::{KeyPair, Vault, VaultState};

        // Only run when the plan authorizes the owner path.
        let plan = authorize_vault_bootstrap(
            DeployProfile::PrimaryReplica,
            false,
            AuthBootstrapInput::Env,
            false,
        )
        .unwrap();
        assert!(matches!(
            plan,
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: true
            }
        ));

        let (pager, tmp_dir) = vault_test_pager();

        // First boot: mint the certificate and seal the real store.
        let kp = KeyPair::generate();
        let vault = Vault::with_certificate_bytes(&pager, &kp.certificate).unwrap();
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: Some(kp.master_secret.clone()),
            kv: std::collections::HashMap::new(),
        };
        vault.save(&pager, &state).unwrap();

        // Restart (unseal-only plan): the emitted certificate unseals the same
        // store without consuming any secret input.
        let restart_plan = plan_vault_bootstrap(BootstrapDisposition::AlreadyComplete);
        assert_eq!(
            restart_plan,
            VaultBootstrapPlan::OpenClusterGlobalStore {
                consume_secret_inputs: false
            }
        );
        let reopened = Vault::with_certificate(&pager, &kp.certificate_hex()).unwrap();
        let loaded = reopened.load(&pager).unwrap().unwrap();
        assert!(loaded.bootstrapped);
        assert_eq!(loaded.master_secret, Some(kp.master_secret));

        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn scratch_certificate_cannot_unseal_the_real_store() {
        use crate::auth::vault::{KeyPair, Vault, VaultState};

        let (pager, tmp_dir) = vault_test_pager();

        // Real cluster-global store sealed by the owner's certificate.
        let owner = KeyPair::generate();
        let vault = Vault::with_certificate_bytes(&pager, &owner.certificate).unwrap();
        vault
            .save(
                &pager,
                &VaultState {
                    users: vec![],
                    api_keys: vec![],
                    bootstrapped: true,
                    master_secret: Some(owner.master_secret.clone()),
                    kv: std::collections::HashMap::new(),
                },
            )
            .unwrap();

        // A per-member scratch certificate (minted against a different DB)
        // must not unseal the real store — the anti-goal PRD #1227 forbids.
        let scratch = KeyPair::generate();
        let scratch_vault = Vault::with_certificate_bytes(&pager, &scratch.certificate).unwrap();
        assert!(scratch_vault.load(&pager).is_err());

        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
