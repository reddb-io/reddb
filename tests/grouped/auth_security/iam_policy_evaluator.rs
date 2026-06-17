//! Pure-evaluator regression tests on top of the IAM policy kernel.
//!
//! These tests exercise the precedence rules (`Deny > Allow > DefaultDeny`),
//! conditions, policy-first admin authority, and tenant scoping. They run
//! against the kernel directly (no AuthStore round-trip) so the assertions
//! stay tight to the algorithm in `crate::auth::policies`.

use reddb::auth::policies::{evaluate, Decision, EvalContext, Policy, ResourceRef};

fn parse(json: &str) -> Policy {
    Policy::from_json_str(json)
        .unwrap_or_else(|e| panic!("policy parse failed: {e} — body=\n{json}"))
}

fn ctx_user() -> EvalContext {
    EvalContext {
        principal_tenant: None,
        current_tenant: None,
        peer_ip: None,
        mfa_present: false,
        now_ms: 1_700_000_000_000,
        principal_is_admin_role: false,
        principal_is_system_owned: false,
        principal_is_platform_scoped: true,
    }
}

#[test]
fn allow_then_unrelated_deny_keeps_allow() {
    let p1 = parse(
        r#"{
            "id": "p-allow",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:public.orders"]
            }]
        }"#,
    );
    let p2 = parse(
        r#"{
            "id": "p-deny-other",
            "version": 1,
            "statements": [{
                "effect": "deny",
                "actions": ["select"],
                "resources": ["table:public.unrelated"]
            }]
        }"#,
    );
    let r = ResourceRef::new("table", "public.orders");
    let d = evaluate(&[&p1, &p2], "select", &r, &ctx_user());
    matches!(d, Decision::Allow { .. });
    assert!(matches!(d, Decision::Allow { .. }), "got {d:?}");
}

#[test]
fn deny_short_circuits_allow() {
    let p1 = parse(
        r#"{
            "id": "p-allow",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:public.orders"]
            }]
        }"#,
    );
    let p2 = parse(
        r#"{
            "id": "p-deny",
            "version": 1,
            "statements": [{
                "sid": "block",
                "effect": "deny",
                "actions": ["select"],
                "resources": ["table:public.orders"]
            }]
        }"#,
    );
    let r = ResourceRef::new("table", "public.orders");
    let d = evaluate(&[&p1, &p2], "select", &r, &ctx_user());
    assert!(
        matches!(&d, Decision::Deny { matched_policy_id, .. } if matched_policy_id == "p-deny"),
        "got {d:?}"
    );
}

#[test]
fn empty_policy_set_is_default_deny() {
    let r = ResourceRef::new("table", "public.x");
    let d = evaluate(&[], "select", &r, &ctx_user());
    assert!(matches!(d, Decision::DefaultDeny), "got {d:?}");
}

#[test]
fn admin_does_not_bypass_explicit_deny() {
    // Policy-first authorization: admin authority grants a broad allow but
    // an explicit Deny is a managed guardrail that wins even for admins.
    let p = parse(
        r#"{
            "id": "p-deny-all",
            "version": 1,
            "statements": [{
                "effect": "deny",
                "actions": ["*"],
                "resources": ["*"]
            }]
        }"#,
    );
    let mut ctx = ctx_user();
    ctx.principal_is_admin_role = true;
    let r = ResourceRef::new("table", "public.x");
    let d = evaluate(&[&p], "select", &r, &ctx);
    assert!(
        matches!(&d, Decision::Deny { matched_policy_id, .. } if matched_policy_id == "p-deny-all"),
        "explicit deny must win over admin authority, got {d:?}"
    );
}

#[test]
fn admin_without_matching_allow_falls_through_to_default_deny() {
    // After retiring the AdminBypass shortcut, admin authority is no longer
    // an evaluator-level fallback. An admin without a matching Allow (and
    // no matching Deny either) gets DefaultDeny like any other principal.
    let p = parse(
        r#"{
            "id": "p-allow-orders",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:public.orders"]
            }]
        }"#,
    );
    let mut ctx = ctx_user();
    ctx.principal_is_admin_role = true;
    let r = ResourceRef::new("table", "public.unrelated");
    let d = evaluate(&[&p], "delete", &r, &ctx);
    assert_eq!(d, Decision::DefaultDeny, "got {d:?}");
}

#[test]
fn condition_mfa_blocks_when_absent() {
    let p = parse(
        r#"{
            "id": "p-mfa",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["table:public.orders"],
                "condition": { "mfa": true }
            }]
        }"#,
    );
    let r = ResourceRef::new("table", "public.orders");
    let d = evaluate(&[&p], "select", &r, &ctx_user());
    assert!(matches!(d, Decision::DefaultDeny), "got {d:?}");
    let mut ctx = ctx_user();
    ctx.mfa_present = true;
    let d2 = evaluate(&[&p], "select", &r, &ctx);
    assert!(matches!(d2, Decision::Allow { .. }), "got {d2:?}");
}

/// Build a context for the four-cell matrix of (platform-scoped?) ×
/// (system-owned?). `tenant` of `None` is platform-scoped.
fn ctx_matrix(tenant: Option<&str>, system_owned: bool) -> EvalContext {
    EvalContext {
        principal_tenant: tenant.map(|t| t.to_string()),
        current_tenant: tenant.map(|t| t.to_string()),
        peer_ip: None,
        mfa_present: false,
        now_ms: 1_700_000_000_000,
        principal_is_admin_role: false,
        principal_is_system_owned: system_owned,
        principal_is_platform_scoped: tenant.is_none(),
    }
}

#[test]
fn system_owned_condition_is_rejected() {
    let err = Policy::from_json_str(
        r#"{
            "id": "p-system-only",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["admin:reload"],
                "resources": ["*"],
                "condition": { "system_owned": true }
            }]
        }"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("system_owned"));
}

#[test]
fn platform_scoped_condition_gates_on_principal_attribute() {
    // A guardrail that denies platform-scoped principals — exercising
    // deny-on-attribute across the matrix. Resources are wildcards so the
    // assertions stay tight to the `platform_scoped` condition rather than
    // tenant resource-scoping.
    let allow = parse(
        r#"{
            "id": "p-allow",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["select"],
                "resources": ["*"]
            }]
        }"#,
    );
    let deny_platform = parse(
        r#"{
            "id": "p-deny-platform",
            "version": 1,
            "statements": [{
                "sid": "no-platform",
                "effect": "deny",
                "actions": ["select"],
                "resources": ["*"],
                "condition": { "platform_scoped": true }
            }]
        }"#,
    );
    let r = ResourceRef::new("table", "public.orders");
    let pols = [&allow, &deny_platform];

    // ordinary tenant user — allowed (deny condition does not match)
    let d = evaluate(&pols, "select", &r, &ctx_matrix(Some("acme"), false));
    assert!(
        matches!(d, Decision::Allow { .. }),
        "tenant user: got {d:?}"
    );
    // tenant-scoped system-owned user — still allowed (not platform-scoped)
    let d = evaluate(&pols, "select", &r, &ctx_matrix(Some("acme"), true));
    assert!(
        matches!(d, Decision::Allow { .. }),
        "tenant system-owned: got {d:?}"
    );
    // ordinary platform-scoped user — denied by guardrail
    let d = evaluate(&pols, "select", &r, &ctx_matrix(None, false));
    assert!(
        matches!(&d, Decision::Deny { matched_policy_id, .. } if matched_policy_id == "p-deny-platform"),
        "platform user: got {d:?}"
    );
    // platform-scoped system-owned user — also denied by guardrail
    let d = evaluate(&pols, "select", &r, &ctx_matrix(None, true));
    assert!(
        matches!(&d, Decision::Deny { matched_policy_id, .. } if matched_policy_id == "p-deny-platform"),
        "platform system-owned: got {d:?}"
    );
}

#[test]
fn wildcard_action_matches_select_and_insert() {
    let p = parse(
        r#"{
            "id": "p-wild",
            "version": 1,
            "statements": [{
                "effect": "allow",
                "actions": ["*"],
                "resources": ["table:public.orders"]
            }]
        }"#,
    );
    let r = ResourceRef::new("table", "public.orders");
    assert!(matches!(
        evaluate(&[&p], "select", &r, &ctx_user()),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        evaluate(&[&p], "insert", &r, &ctx_user()),
        Decision::Allow { .. }
    ));
}
