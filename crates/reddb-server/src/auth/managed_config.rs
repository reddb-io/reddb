//! Managed config namespace enforcement (#649).
//!
//! Guards mutations of config keys that the operator has marked
//! `managed=true` in [`super::registry::ConfigRegistry`]. The gate sits
//! in front of the ordinary config write path: given the config key the
//! caller is about to set, it looks the key up in the registry (by
//! exact id, then by parent-namespace fallback), and:
//!
//! * If no entry matches, or the matched entry is **not** managed,
//!   returns [`ManagedConfigDecision::PassThrough`] — ordinary policy
//!   rules govern the write (criterion #5: non-managed configs remain
//!   writable per ordinary policy).
//!
//! * If the matched entry is managed, caller must satisfy
//!   [`AuthStore::check_policy_authz`] against the entry's
//!   `required_action` / `required_resource`. Otherwise
//!   [`ManagedConfigDecision::Deny`] with [`DenyReason::PolicyDenied`].
//!
//! Every Deny carries the matched entry id, version, required action,
//! and required resource so the Control Event Ledger can persist the
//! evidence (criterion #4). The matched action/resource string mirrors
//! what `check_policy_authz` would have evaluated, so an investigator
//! can replay the decision.

use super::policies::{EvalContext, ResourceRef};
use super::registry::{ConfigRegistry, ConfigRegistryEntry, EvidenceRequirement, Mutability};
use super::store::AuthStore;
use super::UserId;

/// Outcome of a managed-config write check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedConfigDecision {
    /// Key is not governed by a managed registry entry. Caller should
    /// proceed with ordinary policy checks against `config:write` (etc.).
    PassThrough { key: String },
    /// Key is managed and the caller satisfied the policy gate. Caller
    /// may proceed; the returned evidence requirement tells the Control
    /// Event Ledger how much detail to persist.
    Allow {
        entry_id: String,
        entry_version: u64,
        resource_type: String,
        managed: bool,
        mutability: Mutability,
        matched_action: String,
        matched_resource: String,
        evidence: EvidenceRequirement,
    },
    /// Key is managed and the caller failed one of the gates. Carries
    /// enough metadata for Control Event emission.
    Deny {
        entry_id: String,
        entry_version: u64,
        resource_type: String,
        managed: bool,
        mutability: Mutability,
        matched_action: String,
        matched_resource: String,
        reason: DenyReason,
    },
}

/// Why a managed config write was denied. Designed for Control Event
/// payloads: the variant tells operators what *kind* of guard tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// The policy evaluator rejected the action/resource pair (either
    /// explicit Deny or DefaultDeny).
    PolicyDenied,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyDenied => write!(f, "managed config required policy permission was denied"),
        }
    }
}

impl ManagedConfigDecision {
    /// Convenience: did this decision permit the write (PassThrough or
    /// Allow)? Callers that just want a boolean gate use this; callers
    /// that need to emit Control Events match on the full variant.
    pub fn permitted(&self) -> bool {
        matches!(self, Self::PassThrough { .. } | Self::Allow { .. })
    }
}

/// Resource-type tag for entries that govern a single config key (e.g.
/// `red.config.audit.enabled`). The gate matches by exact id.
pub const RESOURCE_TYPE_CONFIG_KEY: &str = "config_key";
/// Resource-type tag for entries that govern an entire dotted namespace
/// (e.g. `red.config.audit`). The gate matches any descendant key under
/// the namespace.
pub const RESOURCE_TYPE_CONFIG_NAMESPACE: &str = "config_namespace";

/// Stateless guard wrapping a [`ConfigRegistry`] reference.
pub struct ManagedConfigGate<'a> {
    registry: &'a ConfigRegistry,
}

impl<'a> ManagedConfigGate<'a> {
    pub fn new(registry: &'a ConfigRegistry) -> Self {
        Self { registry }
    }

    /// Evaluate a write to `key` for `actor`. Returns one of the three
    /// [`ManagedConfigDecision`] variants — see the module docs for the
    /// decision rules.
    pub fn check_write(
        &self,
        auth: &AuthStore,
        actor: &UserId,
        ctx: &EvalContext,
        key: &str,
    ) -> ManagedConfigDecision {
        let Some(entry) = self.lookup_governing_entry(key) else {
            return ManagedConfigDecision::PassThrough {
                key: key.to_string(),
            };
        };
        if !entry.managed {
            // The registry knows about this key but the operator has not
            // marked it managed. Ordinary policy rules apply.
            return ManagedConfigDecision::PassThrough {
                key: key.to_string(),
            };
        }

        let (kind, name) = split_required_resource(&entry.required_resource);
        let matched_resource = format!("{kind}:{name}");

        let resource = ResourceRef::new(kind, name);
        if !auth.check_policy_authz(actor, &entry.required_action, &resource, ctx) {
            return ManagedConfigDecision::Deny {
                entry_id: entry.id.clone(),
                entry_version: entry.version,
                resource_type: entry.resource_type.clone(),
                managed: entry.managed,
                mutability: entry.mutability,
                matched_action: entry.required_action.clone(),
                matched_resource,
                reason: DenyReason::PolicyDenied,
            };
        }

        ManagedConfigDecision::Allow {
            entry_id: entry.id.clone(),
            entry_version: entry.version,
            resource_type: entry.resource_type.clone(),
            managed: entry.managed,
            mutability: entry.mutability,
            matched_action: entry.required_action.clone(),
            matched_resource,
            evidence: entry.evidence_requirement,
        }
    }

    /// Most-specific registry entry governing `key`: exact match first,
    /// then dotted-namespace ancestors (`a.b.c` → `a.b` → `a`). Only
    /// entries whose `resource_type` is `config_key` (for exact match)
    /// or `config_namespace` (for ancestor match) qualify; an entry of
    /// any other resource_type is ignored even if its id collides with
    /// the key — those entries describe other governance surfaces
    /// (vault paths, policies, audit) and must not silently gate config
    /// writes.
    fn lookup_governing_entry(&self, key: &str) -> Option<ConfigRegistryEntry> {
        if let Some(e) = self.registry.get_active(key) {
            if e.resource_type == RESOURCE_TYPE_CONFIG_KEY {
                return Some(e);
            }
        }
        let mut cursor = key;
        while let Some(idx) = cursor.rfind('.') {
            cursor = &cursor[..idx];
            if let Some(e) = self.registry.get_active(cursor) {
                if e.resource_type == RESOURCE_TYPE_CONFIG_NAMESPACE {
                    return Some(e);
                }
            }
        }
        None
    }
}

/// Split `"kind:name"` from a registry entry's `required_resource`.
/// Falls back to `("config", whole_string)` when the colon is absent so
/// older entries that just stored a bare config key still produce a
/// well-formed [`ResourceRef`].
fn split_required_resource(s: &str) -> (&str, &str) {
    match s.split_once(':') {
        Some((k, n)) if !k.is_empty() => (k, n),
        _ => ("config", s),
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
        // Admin shape used only for *seeding* the registry — managed
        // entries register via the governance path, which requires
        // `red.registry:*` allow. The fact that the seeder is admin
        // here is incidental.
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

    fn allow_config_write(id: &str, resource_glob: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["config:write"],
                    "resources": ["{resource_glob}"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn deny_config_write(id: &str, resource_glob: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "deny",
                    "actions": ["config:write"],
                    "resources": ["{resource_glob}"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn allow_all_config(id: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["config:*"],
                    "resources": ["*"]
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

    fn managed_key_draft(id: &str) -> ConfigRegistryDraft {
        ConfigRegistryDraft {
            id: id.to_string(),
            resource_type: RESOURCE_TYPE_CONFIG_KEY.into(),
            schema: "string".into(),
            mutability: Mutability::MutableViaGovernance,
            sensitivity: Sensitivity::Internal,
            managed: true,
            required_action: "config:write".into(),
            required_resource: format!("config:{id}"),
            evidence_requirement: EvidenceRequirement::Metadata,
        }
    }

    fn managed_namespace_draft(id: &str) -> ConfigRegistryDraft {
        ConfigRegistryDraft {
            id: id.to_string(),
            resource_type: RESOURCE_TYPE_CONFIG_NAMESPACE.into(),
            schema: "namespace".into(),
            mutability: Mutability::MutableViaGovernance,
            sensitivity: Sensitivity::Internal,
            managed: true,
            required_action: "config:write".into(),
            required_resource: format!("config:{id}.*"),
            evidence_requirement: EvidenceRequirement::Metadata,
        }
    }

    fn unmanaged_key_draft(id: &str) -> ConfigRegistryDraft {
        let mut d = managed_key_draft(id);
        d.managed = false;
        d
    }

    // ---- acceptance #1 ---------------------------------------------------

    #[test]
    fn registry_entry_can_mark_a_config_key_as_managed() {
        // The registry already supports the `managed` flag; this test
        // pins that the gate reads it as the trigger for enforcement.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let entry = reg
            .register(
                &store,
                &seeder,
                &registry_admin_ctx(),
                managed_key_draft("red.config.audit.enabled"),
                1_000,
            )
            .unwrap();
        assert!(entry.managed, "draft.managed must propagate to entry");

        // Sanity: an unmanaged sibling registers with managed=false.
        let unmanaged = reg
            .register(
                &store,
                &seeder,
                &registry_admin_ctx(),
                unmanaged_key_draft("app.feature_flag"),
                1_000,
            )
            .unwrap();
        assert!(!unmanaged.managed);
    }

    // ---- acceptance #2 ---------------------------------------------------

    #[test]
    fn ordinary_allow_all_user_is_allowed_on_managed_key() {
        // Managed config is policy-first: an ordinary principal with a
        // matching policy can write a managed key. Protection comes from
        // explicit Deny or missing permission, not a structural flag.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store.create_user("alice", "p", Role::Admin).unwrap();
        let alice = UserId::platform("alice");
        store.put_policy(allow_all_config("p-allow-cfg")).unwrap();
        store
            .attach_policy(PrincipalRef::User(alice.clone()), "p-allow-cfg")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_key_draft("red.config.audit.enabled"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_001,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        let decision = gate.check_write(&store, &alice, &ctx, "red.config.audit.enabled");
        assert!(
            matches!(decision, ManagedConfigDecision::Allow { .. }),
            "got {decision:?}"
        );
    }

    // ---- acceptance #3 ---------------------------------------------------

    #[test]
    fn caller_with_matching_policy_is_allowed() {
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Admin, None)
            .expect("create user");
        let ops = UserId::platform("ops");
        store
            .put_policy(allow_config_write(
                "p-cfg-write",
                "config:red.config.audit.*",
            ))
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(ops.clone()), "p-cfg-write")
            .unwrap();

        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_key_draft("red.config.audit.enabled"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_002,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        let decision = gate.check_write(&store, &ops, &ctx, "red.config.audit.enabled");
        assert!(
            matches!(decision, ManagedConfigDecision::Allow { .. }),
            "got {decision:?}"
        );
        assert!(decision.permitted());
    }

    #[test]
    fn caller_without_matching_policy_is_policy_denied() {
        // No allow grants `config:write` on the managed key —
        // DefaultDeny falls out of the evaluator.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Write, None)
            .unwrap();
        let ops = UserId::platform("ops");
        // Attach an unrelated policy so the evaluator runs through IAM
        // rather than short-circuiting on "no policies".
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
            managed_key_draft("red.config.audit.enabled"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_003,
            principal_is_admin_role: false,
            principal_is_platform_scoped: true,
        };
        let decision = gate.check_write(&store, &ops, &ctx, "red.config.audit.enabled");
        match decision {
            ManagedConfigDecision::Deny { reason, .. } => {
                assert!(matches!(reason, DenyReason::PolicyDenied), "got {reason:?}");
            }
            other => panic!("expected Deny(PolicyDenied), got {other:?}"),
        }
    }

    #[test]
    fn explicit_deny_overrides_allow() {
        // Principal with a broad config-write allow *and* an explicit
        // deny on the managed key — deny wins.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store
            .create_admin_user("ops", "p", Role::Admin, None)
            .unwrap();
        let ops = UserId::platform("ops");
        store.put_policy(allow_all_config("p-allow")).unwrap();
        store
            .put_policy(deny_config_write("p-deny", "config:red.config.audit.*"))
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
            managed_key_draft("red.config.audit.enabled"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_004,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        let decision = gate.check_write(&store, &ops, &ctx, "red.config.audit.enabled");
        match decision {
            ManagedConfigDecision::Deny { reason, .. } => {
                assert!(matches!(reason, DenyReason::PolicyDenied), "got {reason:?}");
            }
            other => panic!("expected Deny(PolicyDenied), got {other:?}"),
        }
    }

    // ---- acceptance #4 ---------------------------------------------------

    #[test]
    fn deny_carries_resource_and_reason_for_control_event() {
        // The Deny payload must give Control Event integration enough
        // detail to reconstruct what was attempted and why it failed.
        let store = store();
        let seeder = seed_registry_admin(&store);
        store.create_user("alice", "p", Role::Admin).unwrap();
        let alice = UserId::platform("alice");

        let reg = ConfigRegistry::new();
        let mut draft = managed_key_draft("red.config.backup.retention_days");
        draft.evidence_requirement = EvidenceRequirement::Full;
        reg.register(&store, &seeder, &registry_admin_ctx(), draft, 1_000)
            .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_005,
            principal_is_admin_role: true,
            principal_is_platform_scoped: true,
        };
        let decision = gate.check_write(&store, &alice, &ctx, "red.config.backup.retention_days");
        match decision {
            ManagedConfigDecision::Deny {
                entry_id,
                entry_version,
                matched_action,
                matched_resource,
                reason,
                ..
            } => {
                assert_eq!(entry_id, "red.config.backup.retention_days");
                assert_eq!(entry_version, 1);
                assert_eq!(matched_action, "config:write");
                assert_eq!(matched_resource, "config:red.config.backup.retention_days");
                // The reason is human-presentable and identifies the policy gate.
                let rendered = reason.to_string();
                assert!(
                    rendered.contains("policy permission"),
                    "reason should be Control-Event-renderable: {rendered}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    // ---- acceptance #5 ---------------------------------------------------

    #[test]
    fn non_managed_application_config_passes_through() {
        // No registry entry at all → PassThrough.
        let store = store();
        let _ = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext::default();
        let alice = UserId::platform("alice");
        let d = gate.check_write(&store, &alice, &ctx, "app.feature_flag");
        assert!(matches!(d, ManagedConfigDecision::PassThrough { .. }));
        assert!(d.permitted());
    }

    #[test]
    fn unmanaged_registry_entry_also_passes_through() {
        // Entry exists but `managed=false` — the registry knows about
        // the key (so schema/sensitivity metadata is available) but the
        // gate must not interfere with ordinary policy rules.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            unmanaged_key_draft("app.feature_flag"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let ctx = EvalContext::default();
        let alice = UserId::platform("alice");
        let d = gate.check_write(&store, &alice, &ctx, "app.feature_flag");
        assert!(matches!(d, ManagedConfigDecision::PassThrough { .. }));
    }

    // ---- namespace matching contract ------------------------------------

    #[test]
    fn namespace_entry_governs_descendant_keys() {
        // A `config_namespace` entry at `red.config.audit` must gate
        // writes to `red.config.audit.enabled` and any deeper key.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_namespace_draft("red.config.audit"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let alice = UserId::platform("alice");
        let ctx = EvalContext {
            principal_is_platform_scoped: true,
            ..EvalContext::default()
        };

        for key in [
            "red.config.audit.enabled",
            "red.config.audit.sink.kafka.brokers",
        ] {
            let d = gate.check_write(&store, &alice, &ctx, key);
            match d {
                ManagedConfigDecision::Deny {
                    entry_id,
                    matched_resource,
                    ..
                } => {
                    assert_eq!(entry_id, "red.config.audit");
                    assert_eq!(matched_resource, "config:red.config.audit.*");
                }
                other => panic!("expected Deny for {key}, got {other:?}"),
            }
        }

        // A sibling outside the namespace stays a PassThrough.
        let d = gate.check_write(&store, &alice, &ctx, "red.config.storage.tier");
        assert!(
            matches!(d, ManagedConfigDecision::PassThrough { .. }),
            "got {d:?}"
        );
    }

    #[test]
    fn exact_key_entry_wins_over_namespace_entry() {
        // When both a namespace and a more specific key entry exist,
        // the key entry is the one that gates the decision.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        // Namespace: non-managed.
        let mut ns = managed_namespace_draft("red.config.audit");
        ns.managed = false;
        reg.register(&store, &seeder, &registry_admin_ctx(), ns, 1_000)
            .unwrap();
        // Specific key: managed.
        reg.register(
            &store,
            &seeder,
            &registry_admin_ctx(),
            managed_key_draft("red.config.audit.enabled"),
            1_000,
        )
        .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let alice = UserId::platform("alice");
        let ctx = EvalContext {
            principal_is_platform_scoped: true,
            ..EvalContext::default()
        };
        // Specific key wins → managed → Deny.
        let d = gate.check_write(&store, &alice, &ctx, "red.config.audit.enabled");
        assert!(matches!(d, ManagedConfigDecision::Deny { .. }), "got {d:?}");
        // Sibling under the namespace falls through (namespace was
        // marked unmanaged).
        let d = gate.check_write(&store, &alice, &ctx, "red.config.audit.sink");
        assert!(
            matches!(d, ManagedConfigDecision::PassThrough { .. }),
            "got {d:?}"
        );
    }

    #[test]
    fn entry_with_unrelated_resource_type_does_not_gate_config_writes() {
        // Pin the contract: a registry entry whose `resource_type` is
        // not config_key / config_namespace (e.g. `vault_path` reusing a
        // dotted id) must not silently fire the config gate.
        let store = store();
        let seeder = seed_registry_admin(&store);
        let reg = ConfigRegistry::new();
        let mut d = managed_key_draft("red.config.audit.enabled");
        d.resource_type = "vault_path".into();
        reg.register(&store, &seeder, &registry_admin_ctx(), d, 1_000)
            .unwrap();

        let gate = ManagedConfigGate::new(&reg);
        let alice = UserId::platform("alice");
        let ctx = EvalContext::default();
        let dec = gate.check_write(&store, &alice, &ctx, "red.config.audit.enabled");
        assert!(
            matches!(dec, ManagedConfigDecision::PassThrough { .. }),
            "got {dec:?}"
        );
    }

    #[test]
    fn split_required_resource_handles_bare_string() {
        assert_eq!(
            split_required_resource("config:red.config.audit.enabled"),
            ("config", "red.config.audit.enabled")
        );
        assert_eq!(
            split_required_resource("red.config.audit.enabled"),
            ("config", "red.config.audit.enabled")
        );
        assert_eq!(
            split_required_resource(":only-name"),
            ("config", ":only-name")
        );
    }
}
