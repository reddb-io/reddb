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
//! Two outcomes are deliberately preserved while the owner model is absent:
//!
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
/// * Any `--no-auth` / `--dev` boot returns [`BootstrapDisposition::SkipDevBypass`]:
///   the caller skips all auth/bootstrap state. For cluster shapes this is
///   the explicit development carveout from ADR 0058.
/// * A non-cluster boot returns [`BootstrapDisposition::ProceedLocal`]: the
///   local node is the only authority for its own auth state.
/// * A cluster-shaped, non-`--no-auth` boot is rejected. There is no
///   reserved global system range owner model yet, so no member can prove
///   it is the single writer of global auth/vault/config/policy state.
pub fn authorize(
    deploy_profile: DeployProfile,
    no_auth: bool,
    input: AuthBootstrapInput,
) -> Result<BootstrapDisposition, String> {
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
                authorize(profile, false, AuthBootstrapInput::Env).unwrap(),
                BootstrapDisposition::ProceedLocal,
                "{profile:?} should proceed with local bootstrap"
            );
        }
    }

    #[test]
    fn cluster_no_auth_is_the_dev_bypass_carveout() {
        let disposition =
            authorize(DeployProfile::Cluster, true, AuthBootstrapInput::None).unwrap();
        assert_eq!(disposition, BootstrapDisposition::SkipDevBypass);
    }

    #[test]
    fn non_cluster_no_auth_also_skips() {
        let disposition =
            authorize(DeployProfile::Embedded, true, AuthBootstrapInput::Env).unwrap();
        assert_eq!(disposition, BootstrapDisposition::SkipDevBypass);
    }

    #[test]
    fn cluster_env_bootstrap_fails_closed() {
        let err = authorize(DeployProfile::Cluster, false, AuthBootstrapInput::Env).unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
        assert!(err.contains("env/preset"), "got: {err}");
    }

    #[test]
    fn cluster_manifest_bootstrap_fails_closed() {
        let err =
            authorize(DeployProfile::Cluster, false, AuthBootstrapInput::Manifest).unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
        assert!(err.contains("manifest"), "got: {err}");
    }

    #[test]
    fn cluster_without_explicit_input_still_fails_closed() {
        // A `simple`-preset cluster boot writes only a per-node
        // bootstrap-complete marker, which ADR 0058 forbids without a
        // proven owner. Fail closed so no divergent marker is written.
        let err = authorize(DeployProfile::Cluster, false, AuthBootstrapInput::None).unwrap_err();
        assert!(err.contains("no concrete authority owner"), "got: {err}");
    }
}
