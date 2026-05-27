//! Policy enforcement mode (#712).
//!
//! Controls what `AuthStore::check_policy_authz_with_role` does when the
//! policy evaluator returns [`DefaultDeny`][crate::auth::policies::Decision::DefaultDeny]
//! — i.e. no statement matched, neither allow nor deny.
//!
//! * [`PolicyEnforcementMode::LegacyRbac`] — fall back to the legacy
//!   role-based decision computed by [`legacy_rbac_decision`]. This is
//!   the default for existing installs so upgrading does not silently
//!   tighten access for principals that have not yet been migrated to
//!   IAM policies.
//! * [`PolicyEnforcementMode::PolicyOnly`] — surface the `DefaultDeny`
//!   as a deny. This is the default for fresh bootstraps and the
//!   long-term posture; the upcoming `MIGRATE POLICY MODE TO
//!   'policy_only'` SQL (next slice, S5B) flips an existing install
//!   over after the operator has audited their attached policies.
//!
//! The mode is plumbed through [`super::store::AuthStore`] and read on
//! every policy decision. It is configured by the `red.config.policy.
//! enforcement_mode` config key — see `runtime::impl_config` for the
//! write-path validation and the boot-time loader.

use super::action_catalog::{lookup, ActionCategory};
use super::Role;

/// Config key that selects the enforcement mode.
pub const ENFORCEMENT_MODE_CONFIG_KEY: &str = "red.config.policy.enforcement_mode";

/// Version at which `policy_only` becomes the only accepted mode and the
/// `legacy_rbac` fallback is removed. Reported by `SHOW POLICIES` so
/// operators know how long they have to migrate.
pub const POLICY_ONLY_HARD_VERSION: &str = "1.0.0";

/// Selects the behaviour of the policy evaluator when no statement
/// matches the requested `(action, resource)` pair. See the module
/// docs for the semantics of each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyEnforcementMode {
    /// Fall back to the role-based decision when no policy matches.
    LegacyRbac,
    /// Treat "no matching policy" as `DefaultDeny`.
    PolicyOnly,
}

impl PolicyEnforcementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyRbac => "legacy_rbac",
            Self::PolicyOnly => "policy_only",
        }
    }

    /// Parse a configuration value. Returns `None` for any string that
    /// is not exactly one of the two accepted modes — callers turn that
    /// `None` into the "invalid value" rejection at config-write time.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy_rbac" => Some(Self::LegacyRbac),
            "policy_only" => Some(Self::PolicyOnly),
            _ => None,
        }
    }

    /// Default for a fresh bootstrap (no prior config, no prior users).
    /// Fresh installs start in the strict posture so they never carry
    /// the legacy RBAC fallback as accumulated technical debt.
    pub const fn default_fresh_bootstrap() -> Self {
        Self::PolicyOnly
    }

    /// Default for an existing install that has no
    /// `enforcement_mode` key set. Preserves pre-#712 behaviour so the
    /// upgrade is non-disruptive; operators move to `policy_only`
    /// explicitly via the migration command (S5B).
    pub const fn default_existing_install() -> Self {
        Self::LegacyRbac
    }
}

impl std::fmt::Display for PolicyEnforcementMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Computes the legacy role-based decision for a `(role, action)` pair.
///
/// This is the function `LegacyRbac` mode falls back to when the
/// evaluator returns `DefaultDeny`. The mapping is action-category
/// driven (via [`super::action_catalog`]) so that adding a new action
/// to the catalog inherits a sensible role floor without touching this
/// function.
///
/// Category → required role floor:
///
/// * `Dml` reads (`select`) → `Read`.
/// * `Dml` writes / `Schema` → `Write`.
/// * `Ddl`, `Function`, `Mgmt`, `Policy`, `Admin`, `Config`, `Vault`,
///   `Wildcard`, `Other` → `Admin`.
/// * `Ai` → `Read` (analytics-facing surface; reserved category today).
///
/// Unknown actions (not in the catalog) require `Admin`, matching the
/// conservative pre-#712 default for verbs the kernel does not
/// recognise.
pub fn legacy_rbac_decision(role: Role, action: &str) -> bool {
    let required = required_role_for_action(action);
    role >= required
}

/// Internal: minimum role required to satisfy `action` under the legacy
/// RBAC posture. Exposed via [`legacy_rbac_decision`].
fn required_role_for_action(action: &str) -> Role {
    // The two distinct reads in the DML category. Everything else in
    // the DML category mutates data and demands Write.
    if action == "select" {
        return Role::Read;
    }
    match lookup(action) {
        Some(entry) => match entry.category {
            ActionCategory::Dml => Role::Write,
            ActionCategory::Schema => Role::Write,
            ActionCategory::Ai => Role::Read,
            ActionCategory::Notification => Role::Write,
            ActionCategory::Stream => Role::Write,
            ActionCategory::Ddl
            | ActionCategory::Function
            | ActionCategory::Mgmt
            | ActionCategory::Policy
            | ActionCategory::Admin
            | ActionCategory::Config
            | ActionCategory::Vault
            | ActionCategory::Wildcard
            | ActionCategory::Other => Role::Admin,
        },
        None => Role::Admin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_both_modes() {
        assert_eq!(
            PolicyEnforcementMode::parse("legacy_rbac"),
            Some(PolicyEnforcementMode::LegacyRbac)
        );
        assert_eq!(
            PolicyEnforcementMode::parse("policy_only"),
            Some(PolicyEnforcementMode::PolicyOnly)
        );
    }

    #[test]
    fn parse_rejects_invalid_values() {
        for bad in &[
            "",
            "rbac",
            "LEGACY_RBAC",
            "policy-only",
            "off",
            " policy_only",
        ] {
            assert!(
                PolicyEnforcementMode::parse(bad).is_none(),
                "parse should reject {bad:?}"
            );
        }
    }

    #[test]
    fn defaults_documented_for_fresh_vs_existing() {
        // Fresh bootstraps land in the strict posture; existing
        // installs land in the lenient one so an upgrade does not
        // accidentally lock anyone out.
        assert_eq!(
            PolicyEnforcementMode::default_fresh_bootstrap(),
            PolicyEnforcementMode::PolicyOnly
        );
        assert_eq!(
            PolicyEnforcementMode::default_existing_install(),
            PolicyEnforcementMode::LegacyRbac
        );
    }

    #[test]
    fn display_round_trip() {
        for m in &[
            PolicyEnforcementMode::LegacyRbac,
            PolicyEnforcementMode::PolicyOnly,
        ] {
            let s = m.to_string();
            assert_eq!(PolicyEnforcementMode::parse(&s), Some(*m));
        }
    }

    #[test]
    fn legacy_rbac_select_requires_only_read() {
        assert!(legacy_rbac_decision(Role::Read, "select"));
        assert!(legacy_rbac_decision(Role::Write, "select"));
        assert!(legacy_rbac_decision(Role::Admin, "select"));
    }

    #[test]
    fn legacy_rbac_dml_write_requires_write() {
        for action in &["insert", "update", "delete", "truncate", "write"] {
            assert!(
                !legacy_rbac_decision(Role::Read, action),
                "Read must not satisfy {action}",
            );
            assert!(
                legacy_rbac_decision(Role::Write, action),
                "Write must satisfy {action}",
            );
            assert!(
                legacy_rbac_decision(Role::Admin, action),
                "Admin must satisfy {action}",
            );
        }
    }

    #[test]
    fn legacy_rbac_admin_categories_require_admin() {
        for action in &[
            "create",
            "drop",
            "alter",
            "grant",
            "revoke",
            "policy:put",
            "admin:bootstrap",
            "config:write",
            "vault:read",
            "*",
        ] {
            assert!(
                !legacy_rbac_decision(Role::Read, action),
                "Read must not satisfy {action}",
            );
            assert!(
                !legacy_rbac_decision(Role::Write, action),
                "Write must not satisfy {action}",
            );
            assert!(
                legacy_rbac_decision(Role::Admin, action),
                "Admin must satisfy {action}",
            );
        }
    }

    #[test]
    fn legacy_rbac_unknown_action_requires_admin() {
        // Conservative default: an action verb the catalog does not
        // know about cannot be granted to non-admins under legacy
        // RBAC fallback. Operators must add the verb to the catalog
        // (and ideally to a policy) before non-admin principals can
        // use it.
        assert!(!legacy_rbac_decision(Role::Read, "made-up:verb"));
        assert!(!legacy_rbac_decision(Role::Write, "made-up:verb"));
        assert!(legacy_rbac_decision(Role::Admin, "made-up:verb"));
    }

    #[test]
    fn hard_version_constant_is_well_formed() {
        // We expose this string in SHOW POLICIES. It must parse as a
        // dotted semver-style identifier (digits and dots only), with
        // at least one dot, so client tooling can compare versions.
        let v = POLICY_ONLY_HARD_VERSION;
        assert!(v.contains('.'), "hard version must look like x.y[.z]");
        for ch in v.chars() {
            assert!(
                ch.is_ascii_digit() || ch == '.',
                "hard version must contain only digits and dots, got {ch:?}"
            );
        }
    }
}
