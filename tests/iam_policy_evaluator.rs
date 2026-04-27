//! Pure-evaluator regression tests on top of the IAM policy kernel.
//!
//! These tests exercise the precedence rules (`Deny > Allow > DefaultDeny`),
//! conditions, admin bypass, and tenant scoping. They run against the
//! kernel directly (no AuthStore round-trip) so the assertions stay
//! tight to the algorithm in `crate::auth::policies`.

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
fn admin_bypass_overrides_deny() {
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
    assert!(matches!(d, Decision::AdminBypass), "got {d:?}");
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
