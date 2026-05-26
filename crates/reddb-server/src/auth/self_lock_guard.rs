//! PolicySelfLockGuard — attach-time invariant + break-glass recovery
//! for issue #713.
//!
//! Operators can compose policies that, taken together, deny the system
//! the right to ever modify policies again — `deny policy:detach on *`,
//! for example. Such an attach permanently bricks the IAM surface. This
//! module catches it at the boundary and refuses, then offers a single
//! documented recovery path for the case where the invariant was
//! somehow bypassed.
//!
//! # Invariant
//!
//! On every audited `policy:attach`, we simulate the post-attach policy
//! graph against a synthetic principal called [`PLATFORM_OWNER_USERNAME`]
//! who carries an inert system-reserved unlock policy granting
//! `policy:*` and `admin:bootstrap` on `*`. If the simulation of
//! `policy:detach` against any policy resource does not return `Allow`,
//! we refuse the attach with an error naming the offending statement.
//!
//! The synthetic principal is *not* an admin role and *not* an
//! authentication identity — it exists purely as a policy-graph anchor
//! for the invariant check. It is filtered out of every catalog read
//! path and rejected at every authentication entry point.
//!
//! # Break-glass
//!
//! `REDDB_POLICY_BREAK_GLASS=1` at boot: install/refresh the unlock
//! policy, rebind it to the synthetic principal, emit a loud
//! `policy.break_glass` audit event, and warn on the log. After the
//! operator attaches a corrective policy by hand, they reboot without
//! the env var and the system continues normally.

use std::collections::HashMap;
use std::sync::Arc;

use super::policies::{self as iam_policies, Decision, EvalContext, Policy, ResourceRef};

/// Synthetic principal username. Reserved — must never appear in
/// `red.users`, must never authenticate. Lives only inside the policy
/// graph as the "ultimate admin" anchor for the invariant.
pub const PLATFORM_OWNER_USERNAME: &str = "__platform_owner__";

/// Policy id for the system-reserved unlock policy attached to the
/// synthetic principal.
pub const PLATFORM_OWNER_UNLOCK_POLICY_ID: &str = "__platform_owner_unlock__";

/// Env var that triggers the break-glass recovery path at boot.
pub const BREAK_GLASS_ENV: &str = "REDDB_POLICY_BREAK_GLASS";

/// Construct the inert unlock policy: allow `policy:*` and
/// `admin:bootstrap` on `*`. Returns the parsed `Policy` ready to be
/// fed to `AuthStore::put_policy`.
pub fn unlock_policy() -> Policy {
    let json = format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [
                {{
                    "sid": "unlock-policy-lifecycle",
                    "effect": "allow",
                    "actions": ["policy:*", "admin:bootstrap"],
                    "resources": ["*"]
                }}
            ]
        }}"#,
        id = PLATFORM_OWNER_UNLOCK_POLICY_ID,
    );
    Policy::from_json_str(&json).expect("unlock policy must compile")
}

/// True when the username equals the reserved synthetic principal.
pub fn is_synthetic_principal(username: &str) -> bool {
    username == PLATFORM_OWNER_USERNAME
}

/// True when the policy id matches the system-reserved unlock policy.
pub fn is_unlock_policy(policy_id: &str) -> bool {
    policy_id == PLATFORM_OWNER_UNLOCK_POLICY_ID
}

/// Build the `EvalContext` for the synthetic principal. Marks it as
/// system-owned, admin-role, and platform-scoped so policies with
/// conditions targeting "non-admin / tenant / non-system" principals
/// don't match and don't block the invariant.
pub fn synthetic_eval_context() -> EvalContext {
    EvalContext {
        principal_tenant: None,
        current_tenant: None,
        peer_ip: None,
        mfa_present: false,
        now_ms: 0,
        principal_is_admin_role: true,
        principal_is_system_owned: true,
        principal_is_platform_scoped: true,
    }
}

/// Outcome of the invariant check. Carries enough detail for the
/// caller to produce a clear error message naming the offending
/// statement.
#[derive(Debug, Clone)]
pub enum InvariantOutcome {
    Ok,
    Blocked {
        policy_id: String,
        sid: Option<String>,
        reason: String,
    },
}

/// Simulate `policy:detach` against `policy:*` for the synthetic
/// principal, treating every entry in `existing_policies` as effective
/// for the principal (whoever attached one of these deny statements
/// implicitly proposed to make it apply to everyone, including the
/// platform owner; the simulator gates that by condition matching).
///
/// `existing_policies` should be the *full* policy set after the
/// proposed mutation has been applied (for an attach, that's just the
/// current set — attach doesn't add a policy, only a binding).
pub fn check_self_lock_invariant(existing_policies: &[Arc<Policy>]) -> InvariantOutcome {
    let unlock = unlock_policy();
    let mut refs: Vec<&Policy> = Vec::with_capacity(existing_policies.len() + 1);
    refs.push(&unlock);
    for p in existing_policies {
        // Skip the persisted unlock policy if it's already in the set —
        // we always evaluate against the freshly-built one to avoid a
        // tampered persisted copy weakening the invariant.
        if p.id == PLATFORM_OWNER_UNLOCK_POLICY_ID {
            continue;
        }
        refs.push(p.as_ref());
    }

    let ctx = synthetic_eval_context();
    let resource = ResourceRef::new("policy", "*");
    let outcome = iam_policies::simulate(&refs, "policy:detach", &resource, &ctx);

    match outcome.decision {
        Decision::Allow { .. } | Decision::AdminBypass => InvariantOutcome::Ok,
        Decision::Deny {
            matched_policy_id,
            matched_sid,
        } => InvariantOutcome::Blocked {
            policy_id: matched_policy_id,
            sid: matched_sid,
            reason: outcome.reason,
        },
        Decision::DefaultDeny => InvariantOutcome::Blocked {
            policy_id: PLATFORM_OWNER_UNLOCK_POLICY_ID.to_string(),
            sid: None,
            reason: outcome.reason,
        },
    }
}

/// Format a human-facing error message for a blocked invariant.
pub fn format_block_error(outcome: &InvariantOutcome) -> Option<String> {
    match outcome {
        InvariantOutcome::Ok => None,
        InvariantOutcome::Blocked {
            policy_id,
            sid,
            reason,
        } => {
            let sid_part = sid
                .as_ref()
                .map(|s| format!(" (sid={s})"))
                .unwrap_or_default();
            Some(format!(
                "self-lock invariant: attach would block `__platform_owner__` \
                 from detaching policies — offending statement in policy `{policy_id}`{sid_part}: {reason}"
            ))
        }
    }
}

/// Marker carried in the `fields` map of the break-glass audit event.
pub fn break_glass_audit_fields(boot_ts_ms: u128) -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert("boot_ts_ms".into(), boot_ts_ms.to_string());
    map.insert("env_var".into(), BREAK_GLASS_ENV.into());
    map.insert("policy_id".into(), PLATFORM_OWNER_UNLOCK_POLICY_ID.into());
    map.insert("principal".into(), PLATFORM_OWNER_USERNAME.into());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Arc<Policy> {
        Arc::new(Policy::from_json_str(json).expect("policy parses"))
    }

    #[test]
    fn unlock_policy_compiles() {
        let p = unlock_policy();
        assert_eq!(p.id, PLATFORM_OWNER_UNLOCK_POLICY_ID);
        assert_eq!(p.statements.len(), 1);
    }

    #[test]
    fn synthetic_principal_check() {
        assert!(is_synthetic_principal(PLATFORM_OWNER_USERNAME));
        assert!(!is_synthetic_principal("alice"));
        assert!(is_unlock_policy(PLATFORM_OWNER_UNLOCK_POLICY_ID));
        assert!(!is_unlock_policy("p-something-else"));
    }

    #[test]
    fn invariant_ok_when_only_unlock() {
        let outcome = check_self_lock_invariant(&[]);
        assert!(matches!(outcome, InvariantOutcome::Ok));
    }

    #[test]
    fn invariant_blocks_deny_detach_on_wildcard() {
        let p = parse(
            r#"{
                "id": "p-self-lock",
                "version": 1,
                "statements": [{
                    "sid": "lock",
                    "effect": "deny",
                    "actions": ["policy:detach"],
                    "resources": ["*"]
                }]
            }"#,
        );
        let outcome = check_self_lock_invariant(&[p]);
        match outcome {
            InvariantOutcome::Blocked { policy_id, sid, .. } => {
                assert_eq!(policy_id, "p-self-lock");
                assert_eq!(sid.as_deref(), Some("lock"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn invariant_blocks_deny_detach_on_policy_wildcard() {
        let p = parse(
            r#"{
                "id": "p-policy-lock",
                "version": 1,
                "statements": [{
                    "effect": "deny",
                    "actions": ["policy:detach"],
                    "resources": ["policy:*"]
                }]
            }"#,
        );
        let outcome = check_self_lock_invariant(&[p]);
        assert!(matches!(outcome, InvariantOutcome::Blocked { .. }));
    }

    #[test]
    fn invariant_allows_narrower_deny() {
        // Deny only for non-system-owned principals. The synthetic
        // principal is system-owned, so the condition skips this
        // statement and the unlock policy still grants Allow.
        let p = parse(
            r#"{
                "id": "p-narrow",
                "version": 1,
                "statements": [{
                    "effect": "deny",
                    "actions": ["policy:detach"],
                    "resources": ["*"],
                    "condition": { "system_owned": false }
                }]
            }"#,
        );
        let outcome = check_self_lock_invariant(&[p]);
        assert!(matches!(outcome, InvariantOutcome::Ok));
    }

    #[test]
    fn format_block_error_for_blocked() {
        let outcome = InvariantOutcome::Blocked {
            policy_id: "p-x".into(),
            sid: Some("s-y".into()),
            reason: "deny at p-x.statement[0] (sid=s-y)".into(),
        };
        let msg = format_block_error(&outcome).expect("blocked carries a message");
        assert!(msg.contains("p-x"));
        assert!(msg.contains("s-y"));
        assert!(msg.contains("__platform_owner__"));
    }

    #[test]
    fn format_block_error_none_for_ok() {
        assert!(format_block_error(&InvariantOutcome::Ok).is_none());
    }

    #[test]
    fn break_glass_audit_fields_carries_marker() {
        let fields = break_glass_audit_fields(123_456);
        assert_eq!(fields.get("boot_ts_ms").unwrap(), "123456");
        assert_eq!(fields.get("env_var").unwrap(), BREAK_GLASS_ENV);
        assert_eq!(
            fields.get("policy_id").unwrap(),
            PLATFORM_OWNER_UNLOCK_POLICY_ID
        );
        assert_eq!(fields.get("principal").unwrap(), PLATFORM_OWNER_USERNAME);
    }

    // -----------------------------------------------------------------
    // End-to-end against the real AuthStore — covers the attach
    // refusal, narrower-deny pass, and break-glass recovery paths.
    // -----------------------------------------------------------------

    mod end_to_end {
        use super::*;
        use crate::auth::store::{PolicyMutationControl, PrincipalRef};
        use crate::auth::{AuthConfig, AuthError, AuthStore, Role, UserId};
        use crate::runtime::control_events::{
            ActorRef, ControlEvent, ControlEventConfig, ControlEventCtx, ControlEventError,
            ControlEventLedger, EventId, EventKind,
        };

        struct NoopLedger;
        impl ControlEventLedger for NoopLedger {
            fn emit(
                &self,
                _: &ControlEventCtx<'_>,
                _: ControlEvent,
            ) -> Result<EventId, ControlEventError> {
                Ok(EventId("test".into()))
            }
        }

        fn fresh_store() -> std::sync::Arc<AuthStore> {
            std::sync::Arc::new(AuthStore::new(AuthConfig::default()))
        }

        fn attach_with_ledger(
            store: &std::sync::Arc<AuthStore>,
            principal: PrincipalRef,
            policy_id: &str,
            actor: &UserId,
        ) -> Result<(), AuthError> {
            let ledger = NoopLedger;
            let ctx = ControlEventCtx {
                actor: ActorRef::User(actor),
                scope: None,
                request_id: None,
                trace_id: None,
            };
            let eval_ctx = EvalContext {
                principal_tenant: None,
                current_tenant: None,
                peer_ip: None,
                mfa_present: false,
                now_ms: 0,
                principal_is_admin_role: true,
                principal_is_system_owned: true,
                principal_is_platform_scoped: true,
            };
            let control = PolicyMutationControl {
                ctx: &ctx,
                ledger: &ledger,
                config: ControlEventConfig::default(),
                registry: None,
                actor,
                eval_ctx: &eval_ctx,
            };
            store.attach_policy_with_control_events(principal, policy_id, &control)
        }

        #[test]
        fn attach_refuses_self_lock_policy() {
            let store = fresh_store();
            store.create_user("alice", "p", Role::Admin).unwrap();
            let alice = UserId::platform("alice");

            let lock = Policy::from_json_str(
                r#"{
                    "id": "p-self-lock",
                    "version": 1,
                    "statements": [{
                        "sid": "brick",
                        "effect": "deny",
                        "actions": ["policy:detach"],
                        "resources": ["*"]
                    }]
                }"#,
            )
            .unwrap();
            store.put_policy(lock).unwrap();

            let err = attach_with_ledger(
                &store,
                PrincipalRef::User(alice.clone()),
                "p-self-lock",
                &alice,
            )
            .expect_err("self-lock attach must be refused");
            let msg = err.to_string();
            assert!(msg.contains("self-lock invariant"), "msg={msg}");
            assert!(msg.contains("p-self-lock"), "msg={msg}");
            assert!(msg.contains("brick"), "msg={msg}");
        }

        #[test]
        fn attach_allows_narrower_deny() {
            let store = fresh_store();
            store.create_user("alice", "p", Role::Admin).unwrap();
            let alice = UserId::platform("alice");

            let narrow = Policy::from_json_str(
                r#"{
                    "id": "p-narrow",
                    "version": 1,
                    "statements": [{
                        "effect": "deny",
                        "actions": ["policy:detach"],
                        "resources": ["*"],
                        "condition": { "system_owned": false }
                    }]
                }"#,
            )
            .unwrap();
            store.put_policy(narrow).unwrap();

            attach_with_ledger(
                &store,
                PrincipalRef::User(alice.clone()),
                "p-narrow",
                &alice,
            )
            .expect("narrower deny attaches normally");
        }

        #[test]
        fn break_glass_installs_rebinds_and_hides_synthetic() {
            let store = fresh_store();
            store
                .apply_policy_break_glass(123_000)
                .expect("break-glass");

            assert!(store.get_policy(PLATFORM_OWNER_UNLOCK_POLICY_ID).is_some());

            let users = store.list_users();
            assert!(
                !users.iter().any(|u| u.username == PLATFORM_OWNER_USERNAME),
                "synthetic principal must not appear in list_users"
            );

            let scoped = store.list_users_scoped(Some(None));
            assert!(
                !scoped.iter().any(|u| u.username == PLATFORM_OWNER_USERNAME),
                "synthetic principal must not appear in list_users_scoped"
            );

            let err = store
                .authenticate_in_tenant(None, PLATFORM_OWNER_USERNAME, "")
                .expect_err("synthetic principal must not authenticate");
            assert!(matches!(err, AuthError::InvalidCredentials));

            // Idempotent.
            store.apply_policy_break_glass(124_000).expect("idempotent");
            assert!(store.get_policy(PLATFORM_OWNER_UNLOCK_POLICY_ID).is_some());
        }

        #[test]
        fn break_glass_recovers_from_tampered_attachment() {
            // Simulates the brief's "rebind it in case a malicious
            // attach detached it earlier" scenario.
            let store = fresh_store();
            store.apply_policy_break_glass(100).expect("install");
            let owner = UserId::platform(PLATFORM_OWNER_USERNAME);
            // Force the binding away.
            store
                .detach_policy(
                    PrincipalRef::User(owner.clone()),
                    PLATFORM_OWNER_UNLOCK_POLICY_ID,
                )
                .expect("detach");

            // Boot again with the env var → reattaches.
            store.apply_policy_break_glass(200).expect("rebind");
            let effective = store.effective_policies(&owner);
            assert!(
                effective
                    .iter()
                    .any(|p| p.id == PLATFORM_OWNER_UNLOCK_POLICY_ID),
                "break-glass must rebind unlock policy to synthetic principal"
            );
        }

        #[test]
        fn event_kind_break_glass_str() {
            assert_eq!(EventKind::PolicyBreakGlass.as_str(), "policy.break_glass");
        }
    }
}
