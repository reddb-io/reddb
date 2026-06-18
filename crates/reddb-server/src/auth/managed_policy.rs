//! Managed policy guardrail (#646).
//!
//! Companion to [`super::managed_config::ManagedConfigGate`]. Guards
//! mutations of IAM policy documents that the operator has marked
//! `managed=true` in [`super::registry::ConfigRegistry`] under a
//! `resource_type` of [`RESOURCE_TYPE_POLICY`].
//!
//! The gate sits in front of the four ordinary policy-mutation paths
//! (`put_policy`, `delete_policy`, `attach_policy`, `detach_policy`).
//! Given the policy id the caller is about to mutate, plus the
//! operation kind, it looks the id up in the registry and:
//!
//! * If no entry matches, or the matched entry is **not** managed,
//!   returns [`ManagedPolicyDecision::PassThrough`] — ordinary IAM
//!   rules govern the mutation (criterion #1 baseline: ordinary
//!   policies remain mutable per ordinary policy).
//!
//! * If the matched entry is managed, caller must satisfy
//!   [`AuthStore::check_policy_authz`] against the operation's action
//!   and the entry's `required_resource`. Otherwise
//!   [`ManagedPolicyDecision::Deny`] with [`DenyReason::PolicyDenied`].
//!
//! Because the `managed=true` bit lives in the registry — not in the
//! submitted Policy document — re-submitting a Policy JSON that omits
//! any "managed" hint cannot flip the entry off; the gate consults the
//! registry per call (criterion #3).
//!
//! Every Deny carries the matched entry id, version, op-derived action,
//! and required resource so the Control Event Ledger / audit hook can
//! persist the evidence (criterion #5). The matched action/resource
//! string mirrors what `check_policy_authz` would have evaluated, so an
//! investigator can replay the decision.

use super::policies::{EvalContext, ResourceRef};
use super::registry::{ConfigRegistry, ConfigRegistryEntry, EvidenceRequirement};
use super::store::AuthStore;
use super::UserId;

/// Resource-type tag a registry entry must carry to govern a policy id.
/// Entries of any other `resource_type` are ignored by this gate even if
/// their id collides with a policy id — those entries describe other
/// governance surfaces (config keys, vault paths, audit) and must not
/// silently gate policy mutations.
pub const RESOURCE_TYPE_POLICY: &str = "policy";

/// Which mutation the caller is attempting on a (possibly-managed)
/// policy id. The gate uses this to derive the policy action it asks
/// the IAM evaluator about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOp {
    /// `put_policy` — install or replace the document.
    Put,
    /// `delete_policy` — remove the document and its attachments.
    Drop,
    /// `attach_policy` — bind the policy to a user or group.
    Attach,
    /// `detach_policy` — unbind the policy from a user or group.
    Detach,
}

impl PolicyOp {
    /// IAM action verb the gate evaluates for this op against the
    /// managed policy's `required_resource`. Matches the verbs already
    /// in `ACTION_ALLOWLIST` (see `policies.rs`).
    pub fn action(self) -> &'static str {
        match self {
            Self::Put => "policy:put",
            Self::Drop => "policy:drop",
            Self::Attach => "policy:attach",
            Self::Detach => "policy:detach",
        }
    }
}

/// Outcome of a managed-policy mutation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedPolicyDecision {
    /// Policy id is not governed by a managed registry entry. Caller
    /// should proceed with ordinary IAM checks.
    PassThrough { policy_id: String, op: PolicyOp },
    /// Policy is managed and the caller satisfied the policy gate.
    /// Caller may proceed; the returned evidence requirement tells the
    /// Control Event Ledger how much detail to persist.
    Allow {
        entry_id: String,
        entry_version: u64,
        op: PolicyOp,
        matched_action: String,
        matched_resource: String,
        evidence: EvidenceRequirement,
    },
    /// Policy is managed and the caller failed one of the gates.
    /// Carries enough metadata for Control Event / audit emission.
    Deny {
        entry_id: String,
        entry_version: u64,
        op: PolicyOp,
        matched_action: String,
        matched_resource: String,
        reason: DenyReason,
    },
}

/// Why a managed policy mutation was denied. Designed for Control Event
/// payloads: the variant tells operators what *kind* of guard tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// The policy evaluator rejected the op-derived action /
    /// required-resource pair (either explicit Deny or DefaultDeny).
    PolicyDenied,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyDenied => {
                write!(f, "managed policy required IAM permission was denied")
            }
        }
    }
}

impl ManagedPolicyDecision {
    /// Convenience: did this decision permit the mutation (PassThrough
    /// or Allow)?
    pub fn permitted(&self) -> bool {
        matches!(self, Self::PassThrough { .. } | Self::Allow { .. })
    }
}

/// Stateless guard wrapping a [`ConfigRegistry`] reference.
pub struct ManagedPolicyGate<'a> {
    registry: &'a ConfigRegistry,
}

impl<'a> ManagedPolicyGate<'a> {
    pub fn new(registry: &'a ConfigRegistry) -> Self {
        Self { registry }
    }

    /// Evaluate `op` on `policy_id` for `actor`. Returns one of the
    /// three [`ManagedPolicyDecision`] variants — see the module docs
    /// for the decision rules.
    pub fn check_mutation(
        &self,
        auth: &AuthStore,
        actor: &UserId,
        ctx: &EvalContext,
        policy_id: &str,
        op: PolicyOp,
    ) -> ManagedPolicyDecision {
        let Some(entry) = self.lookup_governing_entry(policy_id) else {
            return ManagedPolicyDecision::PassThrough {
                policy_id: policy_id.to_string(),
                op,
            };
        };
        if !entry.managed {
            return ManagedPolicyDecision::PassThrough {
                policy_id: policy_id.to_string(),
                op,
            };
        }

        let (kind, name) = split_required_resource(&entry.required_resource, policy_id);
        let matched_resource = format!("{kind}:{name}");
        let matched_action = op.action().to_string();

        let resource = ResourceRef::new(kind, name);
        if !auth.check_policy_authz(actor, op.action(), &resource, ctx) {
            return ManagedPolicyDecision::Deny {
                entry_id: entry.id.clone(),
                entry_version: entry.version,
                op,
                matched_action,
                matched_resource,
                reason: DenyReason::PolicyDenied,
            };
        }

        ManagedPolicyDecision::Allow {
            entry_id: entry.id,
            entry_version: entry.version,
            op,
            matched_action,
            matched_resource,
            evidence: entry.evidence_requirement,
        }
    }

    /// Registry entry governing `policy_id` — exact id match against an
    /// entry whose `resource_type` is [`RESOURCE_TYPE_POLICY`]. There is
    /// no namespace fallback for policies (unlike config keys): policy
    /// ids are flat, not dotted, so a "most-specific" walk has nothing
    /// to climb.
    fn lookup_governing_entry(&self, policy_id: &str) -> Option<ConfigRegistryEntry> {
        let e = self.registry.get_active(policy_id)?;
        if e.resource_type == RESOURCE_TYPE_POLICY {
            Some(e)
        } else {
            None
        }
    }
}

/// Split `"kind:name"` from a registry entry's `required_resource`.
/// Falls back to `("policy", policy_id)` when the colon is absent so
/// older entries that just stored a bare policy id still produce a
/// well-formed [`ResourceRef`] aligned with the policy id under
/// evaluation.
fn split_required_resource<'a>(s: &'a str, policy_id: &'a str) -> (&'a str, &'a str) {
    match s.split_once(':') {
        Some((k, n)) if !k.is_empty() => (k, n),
        _ => ("policy", policy_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::policies::Policy;
    use crate::auth::registry::{ConfigRegistryDraft, Mutability, Sensitivity};
    use crate::auth::store::PrincipalRef;
    use crate::auth::{AuthConfig, Role};
    use std::sync::Arc;

    fn store() -> Arc<AuthStore> {
        Arc::new(AuthStore::new(AuthConfig::default()))
    }

    fn registry_admin_ctx() -> EvalContext {
        EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        }
    }

    fn allow_all_registry(id: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["red.registry:*"],
                    "resources": ["registry:*"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn allow_all_policies(id: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["policy:*"],
                    "resources": ["*"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn allow_policy_action(id: &str, action: &str, resource_glob: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["{action}"],
                    "resources": ["{resource_glob}"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn deny_policy_action(id: &str, action: &str, resource_glob: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "deny",
                    "actions": ["{action}"],
                    "resources": ["{resource_glob}"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn seed_registry_admin(store: &Arc<AuthStore>) -> UserId {
        store.create_user("seeder", "p", Role::Admin).unwrap();
        let uid = UserId::platform("seeder");
        store.put_policy(allow_all_registry("p-reg-allow")).unwrap();
        store
            .attach_policy(PrincipalRef::User(uid.clone()), "p-reg-allow")
            .unwrap();
        uid
    }

    fn managed_policy_draft(id: &str) -> ConfigRegistryDraft {
        ConfigRegistryDraft {
            id: id.to_string(),
            resource_type: RESOURCE_TYPE_POLICY.into(),
            schema: "iam-policy/v1".into(),
            mutability: Mutability::MutableViaGovernance,
            sensitivity: Sensitivity::Internal,
            managed: true,
            // For policy ops the gate derives the action from PolicyOp;
            // `required_action` is recorded for completeness but is not
            // what the evaluator is asked about.
            required_action: "policy:*".into(),
            required_resource: format!("policy:{id}"),
            evidence_requirement: EvidenceRequirement::Metadata,
        }
    }

    fn unmanaged_policy_draft(id: &str) -> ConfigRegistryDraft {
        let mut d = managed_policy_draft(id);
        d.managed = false;
        d
    }

    // ---- acceptance #1 ---------------------------------------------------

    #[test]
    fn policy_can_be_installed_as_managed_through_registry() {
        // Governance path is `ConfigRegistry::register` — gated by
        // `red.registry:register` on `registry:<id>`. The same surface
        // that pins managed configs also pins managed policies.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let entry = reg
            .register(
                &store,
                &seeder,
                &registry_admin_ctx(),
                managed_policy_draft("p-baseline-readonly"),
                1_000,
            )
            .expect("register");
        assert!(entry.managed);
        assert_eq!(entry.resource_type, RESOURCE_TYPE_POLICY);

        // Sanity: an unmanaged sibling registers with managed=false.
        let other = reg
            .register(
                &store,
                &seeder,
                &registry_admin_ctx(),
                unmanaged_policy_draft("p-tenant-custom"),
                1_000,
            )
            .unwrap();
        assert!(!other.managed);
    }

    // ---- acceptance #2 ---------------------------------------------------

    #[test]
    fn ordinary_allow_all_user_can_put_drop_attach_or_detach_managed_policy() {
        // Managed policy is policy-first: alice@platform has
        // `policy:*` on `*`, so the managed gate allows all four ops.
        // Protection comes from explicit Deny or missing permission,
        // not a structural flag.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store.create_user("alice", "p", Role::Admin).unwrap();
        let alice = UserId::platform("alice");
        store
            .put_policy(allow_all_policies("p-alice-allow"))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(alice.clone()), "p-alice-allow")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_001,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        for op in [
            PolicyOp::Put,
            PolicyOp::Drop,
            PolicyOp::Attach,
            PolicyOp::Detach,
        ] {
            let decision = gate.check_mutation(&store, &alice, &ctx, "p-baseline-readonly", op);
            assert!(
                matches!(decision, ManagedPolicyDecision::Allow { op: got_op, .. } if got_op == op),
                "op={op:?} got {decision:?}"
            );
        }
    }

    // ---- acceptance #3 ---------------------------------------------------

    #[test]
    fn re_putting_managed_policy_without_managed_metadata_still_uses_registry_gate() {
        // The "managed" bit lives in the registry, not in the policy
        // document. A caller who rewrites the policy JSON to omit any
        // managed hint still goes through the registry-backed gate. The
        // outcome is policy-derived, so alice's allow-all policy permits it.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store.create_user("alice", "p", Role::Admin).unwrap();
        let alice = UserId::platform("alice");
        store
            .put_policy(allow_all_policies("p-alice-allow"))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(alice.clone()), "p-alice-allow")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_002,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };

        // First attempt — straightforward put — allowed by policy.
        let d1 = gate.check_mutation(&store, &alice, &ctx, "p-baseline-readonly", PolicyOp::Put);
        assert!(matches!(d1, ManagedPolicyDecision::Allow { .. }), "{d1:?}");

        // Second attempt — caller "submits" a stripped policy doc; the
        // gate result is identical because it only consults the
        // registry. The registry entry stays managed at v1; no
        // supersede happened.
        let d2 = gate.check_mutation(&store, &alice, &ctx, "p-baseline-readonly", PolicyOp::Put);
        assert!(matches!(d2, ManagedPolicyDecision::Allow { .. }), "{d2:?}");

        assert_eq!(reg.get_active("p-baseline-readonly").unwrap().version, 1);
        assert!(reg.get_active("p-baseline-readonly").unwrap().managed);
    }

    // ---- acceptance #4 ---------------------------------------------------

    #[test]
    fn caller_with_matching_policy_is_allowed() {
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Admin, None)
            .expect("create user");
        let ops = UserId::platform("ops");
        // Grant ops `policy:*` on the managed policy resource.
        store
            .put_policy(allow_policy_action(
                "p-ops-policy",
                "policy:*",
                "policy:p-baseline-readonly",
            ))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops.clone()), "p-ops-policy")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_003,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        for op in [
            PolicyOp::Put,
            PolicyOp::Drop,
            PolicyOp::Attach,
            PolicyOp::Detach,
        ] {
            let decision = gate.check_mutation(&store, &ops, &ctx, "p-baseline-readonly", op);
            assert!(
                matches!(decision, ManagedPolicyDecision::Allow { .. }),
                "op={op:?} got {decision:?}"
            );
            assert!(decision.permitted());
        }
    }

    #[test]
    fn caller_without_matching_policy_is_policy_denied() {
        // No allow grants the per-op action on the managed policy —
        // DefaultDeny falls out of the evaluator.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Write, None)
            .unwrap();
        let ops = UserId::platform("ops");
        // Unrelated policy so the evaluator goes through IAM rather
        // than short-circuiting on "no policies".
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{"id":"p-unrelated","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.x"]}]}"#,
                )
                .unwrap(),
            )
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops.clone()), "p-unrelated")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_004,
            principal_is_admin_role: false,
            principal_is_platform_scoped: true,
        };
        let decision =
            gate.check_mutation(&store, &ops, &ctx, "p-baseline-readonly", PolicyOp::Put);
        match decision {
            ManagedPolicyDecision::Deny { reason, .. } => {
                assert!(matches!(reason, DenyReason::PolicyDenied), "got {reason:?}");
            }
            other => panic!("expected Deny(PolicyDenied), got {other:?}"),
        }
    }

    #[test]
    fn explicit_deny_overrides_allow() {
        // Principal with a broad policy-* allow *and* an explicit deny
        // on the managed policy resource — deny wins.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Admin, None)
            .unwrap();
        let ops = UserId::platform("ops");
        store.put_policy(allow_all_policies("p-allow")).unwrap();
        store
            .put_policy(deny_policy_action(
                "p-deny",
                "policy:put",
                "policy:p-baseline-readonly",
            ))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops.clone()), "p-allow")
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops.clone()), "p-deny")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_005,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        // The put op is explicitly denied → PolicyDenied.
        let decision =
            gate.check_mutation(&store, &ops, &ctx, "p-baseline-readonly", PolicyOp::Put);
        match decision {
            ManagedPolicyDecision::Deny { reason, .. } => {
                assert!(matches!(reason, DenyReason::PolicyDenied), "got {reason:?}");
            }
            other => panic!("expected Deny(PolicyDenied), got {other:?}"),
        }
        // A different op (drop) is not covered by the deny, so the
        // broad allow still wins.
        let drop_decision =
            gate.check_mutation(&store, &ops, &ctx, "p-baseline-readonly", PolicyOp::Drop);
        assert!(
            matches!(drop_decision, ManagedPolicyDecision::Allow { .. }),
            "got {drop_decision:?}"
        );
    }

    // ---- acceptance #5 ---------------------------------------------------

    #[test]
    fn deny_carries_resource_and_reason_for_audit_hook() {
        // The Deny payload must give the audit hook / Control Event
        // integration enough detail to reconstruct what was attempted
        // and why it failed.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store.create_user("alice", "p", Role::Admin).unwrap();
        let alice = UserId::platform("alice");

        let reg = ConfigRegistry::new();
        let mut draft = managed_policy_draft("p-tenant-isolation");
        draft.evidence_requirement = EvidenceRequirement::Full;
        reg.register(&store, &seeder, &registry_admin_ctx(), draft, 1_000)
            .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_006,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        let decision =
            gate.check_mutation(&store, &alice, &ctx, "p-tenant-isolation", PolicyOp::Attach);
        match decision {
            ManagedPolicyDecision::Deny {
                entry_id,
                entry_version,
                op,
                matched_action,
                matched_resource,
                reason,
            } => {
                assert_eq!(entry_id, "p-tenant-isolation");
                assert_eq!(entry_version, 1);
                assert_eq!(op, PolicyOp::Attach);
                assert_eq!(matched_action, "policy:attach");
                assert_eq!(matched_resource, "policy:p-tenant-isolation");
                let rendered = reason.to_string();
                assert!(
                    rendered.contains("IAM permission"),
                    "reason should be audit-renderable: {rendered}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    // ---- passthrough / resource-type discipline -------------------------

    #[test]
    fn unmanaged_policy_passes_through() {
        // Registry has an entry but managed=false — the gate must not
        // interfere with ordinary IAM rules.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            unmanaged_policy_draft("p-tenant-custom"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let alice = UserId::platform("alice");
        let d = gate.check_mutation(
            &store,
            &alice,
            &EvalContext::default(),
            "p-tenant-custom",
            PolicyOp::Put,
        );
        assert!(matches!(d, ManagedPolicyDecision::PassThrough { .. }));
        assert!(d.permitted());
    }

    #[test]
    fn unknown_policy_passes_through() {
        let store = store();
        let _ = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let gate = ManagedPolicyGate::new(&reg);
        let alice = UserId::platform("alice");
        let d = gate.check_mutation(
            &store,
            &alice,
            &EvalContext::default(),
            "p-anything",
            PolicyOp::Drop,
        );
        assert!(matches!(d, ManagedPolicyDecision::PassThrough { .. }));
    }

    #[test]
    fn entry_with_unrelated_resource_type_does_not_gate_policy_mutations() {
        // Pin the contract: a registry entry whose `resource_type` is
        // not `policy` (e.g. `config_key` reusing a name that collides
        // with a policy id) must not silently fire the policy gate.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let mut d = managed_policy_draft("p-baseline-readonly");
        d.resource_type = "config_key".into();
        reg.register(&store, &seeder, &registry_admin_ctx(), d, 1_000)
            .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let alice = UserId::platform("alice");
        let dec = gate.check_mutation(
            &store,
            &alice,
            &EvalContext::default(),
            "p-baseline-readonly",
            PolicyOp::Put,
        );
        assert!(
            matches!(dec, ManagedPolicyDecision::PassThrough { .. }),
            "got {dec:?}"
        );
    }

    #[test]
    fn tenant_scoped_user_with_matching_policy_can_mutate_managed_policy() {
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Admin, Some("acme"))
            .expect("create tenant-scoped user");
        let ops_acme = UserId::scoped("acme", "ops");
        store
            .put_policy(allow_all_policies("p-ops-acme-allow"))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops_acme.clone()), "p-ops-acme-allow")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_policy_draft("p-baseline-readonly"),
            1_000,
        )
        .unwrap();

        let gate = ManagedPolicyGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: Some("acme".into()),
            current_tenant: Some("acme".into()),
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_100,
            principal_is_admin_role: true,
            principal_is_platform_scoped: false,
        };

        for op in [
            PolicyOp::Put,
            PolicyOp::Drop,
            PolicyOp::Attach,
            PolicyOp::Detach,
        ] {
            let decision = gate.check_mutation(&store, &ops_acme, &ctx, "p-baseline-readonly", op);
            assert!(
                matches!(decision, ManagedPolicyDecision::Allow { op: got_op, .. } if got_op == op),
                "op={op:?} got {decision:?}"
            );
        }
    }

    #[test]
    fn split_required_resource_handles_bare_string() {
        assert_eq!(
            split_required_resource("policy:p-baseline", "p-baseline"),
            ("policy", "p-baseline")
        );
        // Bare string with no colon falls back to (policy, policy_id),
        // not the bare value, so the resource always aligns with the
        // policy id under evaluation.
        assert_eq!(
            split_required_resource("p-anything-else", "p-baseline"),
            ("policy", "p-baseline")
        );
    }
}
