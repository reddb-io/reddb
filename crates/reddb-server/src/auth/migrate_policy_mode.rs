//! Pre-flight delta simulator for `MIGRATE POLICY MODE` (#714).
//!
//! Computes, for every non-admin principal in the install, the set of
//! `(action, resource)` pairs that the principal can access **today**
//! under [`PolicyEnforcementMode::LegacyRbac`] but would lose access to
//! under [`PolicyEnforcementMode::PolicyOnly`].
//!
//! The simulator is pure with respect to the [`AuthStore`]: it does not
//! mutate any state. Callers (the SQL executor for `MIGRATE POLICY MODE
//! TO 'policy_only' [DRY RUN]` and the matching HTTP endpoint) use the
//! returned delta to either refuse the migration (non-empty delta, no
//! DRY RUN) or return it as a result set (DRY RUN).
//!
//! The "admin holds bootstrap allow-all" carve-out from the spec is
//! enforced naturally: a principal with a wildcard `Allow` on `*`/`*`
//! receives [`iam_policies::Decision::Allow`] under either mode, so
//! they never appear in the delta. We don't filter them out by name.

use super::action_catalog::{LifecycleState, ACTIONS};
use super::enforcement_mode::legacy_rbac_decision;
use super::policies::{self as iam_policies, EvalContext, ResourceRef};
use super::store::AuthStore;
use super::{Role, UserId};

/// One row in the migration delta — a principal that would lose access
/// to `(action, resource)` if the install switched to `policy_only`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratePolicyDelta {
    pub principal: UserId,
    pub role: Role,
    pub action: String,
    pub resource_kind: String,
    pub resource_name: String,
}

/// Run the pre-flight simulator.
///
/// `resources` is the list of `(kind, name)` resource references to
/// probe. The caller decides what populates this — the SQL executor
/// passes every existing collection ("table:<name>"); tests can pass a
/// minimal set.
///
/// Returns deltas in stable order: by principal (UserId sort), then by
/// action (catalog order), then by resource (input order).
pub fn simulate_migration_delta(
    store: &AuthStore,
    resources: &[ResourceRef],
    now_ms: u128,
) -> Vec<MigratePolicyDelta> {
    let mut deltas: Vec<MigratePolicyDelta> = Vec::new();

    // Sort users by (tenant, username) so the output order is stable.
    let mut users = store.list_users();
    users.sort_by(|a, b| {
        a.tenant_id
            .cmp(&b.tenant_id)
            .then_with(|| a.username.cmp(&b.username))
    });

    for user in users {
        let uid = UserId::from_parts(user.tenant_id.as_deref(), &user.username);
        let role = user.role;
        let principal_is_admin_role = role == Role::Admin;
        let principal_is_platform_scoped = uid.tenant.is_none();
        let ctx = EvalContext {
            principal_tenant: uid.tenant.clone(),
            current_tenant: uid.tenant.clone(),
            peer_ip: None,
            mfa_present: false,
            now_ms,
            principal_is_admin_role,
            principal_is_platform_scoped,
        };
        let pols = store.effective_policies(&uid);
        let refs: Vec<&iam_policies::Policy> = pols.iter().map(|p| p.as_ref()).collect();

        for entry in ACTIONS.iter() {
            if matches!(entry.lifecycle_state, LifecycleState::Removed) {
                continue;
            }
            let action = entry.name;
            // Per-action shortcut: if the legacy RBAC posture would
            // already deny this action for the principal's role, there
            // is no access to lose. Skip the whole resource sweep.
            if !legacy_rbac_decision(role, action) {
                continue;
            }
            for resource in resources {
                let decision = iam_policies::evaluate(&refs, action, resource, &ctx);
                let lost = matches!(decision, iam_policies::Decision::DefaultDeny);
                if lost {
                    deltas.push(MigratePolicyDelta {
                        principal: uid.clone(),
                        role,
                        action: action.to_string(),
                        resource_kind: resource.kind.to_string(),
                        resource_name: resource.name.to_string(),
                    });
                }
            }
        }
    }

    deltas
}

/// Format a [`UserId`] as `tenant.username` (or just `username` for the
/// platform scope). Used by both the SQL result set and the HTTP body
/// so the two surfaces produce identical principal labels.
pub fn principal_label(uid: &UserId) -> String {
    match &uid.tenant {
        Some(t) => format!("{t}.{}", uid.username),
        None => uid.username.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::policies::Policy;
    use crate::auth::store::PrincipalRef;
    use crate::auth::AuthConfig;

    fn store_with_user(role: Role) -> (AuthStore, UserId) {
        let store = AuthStore::new(AuthConfig::default());
        store.create_user("alice", "p", role).unwrap();
        (store, UserId::platform("alice"))
    }

    fn resources() -> Vec<ResourceRef> {
        vec![ResourceRef::new("table", "orders")]
    }

    #[test]
    fn write_role_without_policy_loses_writes() {
        let (store, uid) = store_with_user(Role::Write);
        let deltas = simulate_migration_delta(&store, &resources(), 0);
        // Write role can do read+write actions under legacy_rbac. Each
        // such action paired with the single probed resource should be
        // a loss because the principal has no policies attached.
        assert!(!deltas.is_empty(), "expected losses, got none");
        assert!(
            deltas.iter().all(|d| d.principal == uid),
            "only alice should appear: {deltas:#?}"
        );
        assert!(
            deltas.iter().any(|d| d.action == "select"),
            "select should be among losses: {deltas:#?}"
        );
    }

    #[test]
    fn admin_role_with_bootstrap_allow_all_has_empty_delta() {
        let (store, uid) = store_with_user(Role::Admin);
        let policy = Policy::from_json_str(
            r#"{"id":"p-bootstrap","version":1,
                "statements":[{"effect":"allow","actions":["*"],"resources":["*"]}]}"#,
        )
        .unwrap();
        store.put_policy(policy).unwrap();
        store
            .attach_policy(PrincipalRef::User(uid.clone()), "p-bootstrap")
            .unwrap();
        let deltas = simulate_migration_delta(&store, &resources(), 0);
        assert!(
            deltas.is_empty(),
            "admin with allow-all should have empty delta, got {deltas:#?}"
        );
    }

    #[test]
    fn admin_role_without_any_policy_shows_up() {
        // An Admin role principal without the bootstrap allow-all
        // policy can do anything under legacy_rbac (via the role
        // fallback) but nothing under policy_only — every probed
        // (action, resource) is a delta row.
        let (store, _uid) = store_with_user(Role::Admin);
        let deltas = simulate_migration_delta(&store, &resources(), 0);
        assert!(!deltas.is_empty());
        assert!(deltas.iter().all(|d| d.role == Role::Admin));
    }

    #[test]
    fn read_role_with_select_allow_loses_nothing_on_select() {
        let (store, uid) = store_with_user(Role::Read);
        let policy = Policy::from_json_str(
            r#"{"id":"p-select-orders","version":1,
                "statements":[{"effect":"allow","actions":["select"],
                               "resources":["table:orders"]}]}"#,
        )
        .unwrap();
        store.put_policy(policy).unwrap();
        store
            .attach_policy(PrincipalRef::User(uid.clone()), "p-select-orders")
            .unwrap();
        let deltas = simulate_migration_delta(&store, &resources(), 0);
        assert!(
            deltas.iter().all(|d| d.action != "select"),
            "select on table:orders is covered: {deltas:#?}"
        );
    }

    #[test]
    fn read_role_actions_outside_role_floor_never_appear() {
        // A Read principal cannot perform write/admin actions under
        // legacy_rbac either — so even though policy_only would also
        // deny them, no access is "lost" by the migration. Delta rows
        // for a Read principal must only carry actions whose
        // legacy_rbac floor is `Read` (e.g. `select`, `ai:*` reads);
        // write- or admin-floored actions must be absent.
        let (store, _uid) = store_with_user(Role::Read);
        let deltas = simulate_migration_delta(&store, &resources(), 0);
        for d in &deltas {
            assert!(
                legacy_rbac_decision(Role::Read, &d.action),
                "Read role would never have access to `{}` under legacy_rbac, \
                 so it should not appear in the delta: {d:?}",
                d.action
            );
            assert!(
                !["insert", "update", "delete", "write", "truncate"].contains(&d.action.as_str()),
                "write-tier action {} leaked into Read principal delta",
                d.action
            );
        }
    }
}
