//! SQL DDL roundtrip for IAM policies.
//!
//! Parser-only assertions (no runtime). Validates the new IAM-flavoured
//! tokens land in the right `QueryExpr` variants and that arguments
//! survive the lexer + parser pipeline.

use reddb::storage::query::{Parser, PolicyPrincipalRef, QueryExpr};

fn parse(sql: &str) -> QueryExpr {
    let mut p = Parser::new(sql).expect("parser construct");
    p.parse().expect("parse")
}

#[test]
fn create_iam_policy_parses() {
    let json = r#"{"id":"p1","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.orders"]}]}"#;
    // Pass the body as a single-quoted SQL string literal — the lexer
    // tolerates inner double quotes inside `'...'` so we can embed JSON
    // verbatim.
    let sql = format!("CREATE POLICY 'p1' AS '{}'", json);
    let expr = parse(&sql);
    match expr {
        QueryExpr::CreateIamPolicy { id, json: body } => {
            assert_eq!(id, "p1");
            assert!(body.contains("\"id\":\"p1\""));
        }
        other => panic!("expected CreateIamPolicy, got {other:?}"),
    }
}

#[test]
fn drop_iam_policy_parses() {
    let expr = parse("DROP POLICY 'p1'");
    match expr {
        QueryExpr::DropIamPolicy { id } => assert_eq!(id, "p1"),
        other => panic!("expected DropIamPolicy, got {other:?}"),
    }
}

#[test]
fn attach_policy_to_user_parses() {
    let expr = parse("ATTACH POLICY 'p1' TO USER alice");
    match expr {
        QueryExpr::AttachPolicy {
            policy_id,
            principal,
        } => {
            assert_eq!(policy_id, "p1");
            match principal {
                PolicyPrincipalRef::User(u) => {
                    assert_eq!(u.username, "alice");
                    assert!(u.tenant.is_none());
                }
                other => panic!("expected user principal, got {other:?}"),
            }
        }
        other => panic!("expected AttachPolicy, got {other:?}"),
    }
}

#[test]
fn attach_policy_to_group_parses() {
    let expr = parse("ATTACH POLICY 'p1' TO GROUP analysts");
    match expr {
        QueryExpr::AttachPolicy {
            policy_id,
            principal,
        } => {
            assert_eq!(policy_id, "p1");
            match principal {
                PolicyPrincipalRef::Group(g) => assert_eq!(g, "analysts"),
                other => panic!("expected group principal, got {other:?}"),
            }
        }
        other => panic!("expected AttachPolicy, got {other:?}"),
    }
}

#[test]
fn detach_policy_parses() {
    let expr = parse("DETACH POLICY 'p1' FROM USER alice");
    matches!(expr, QueryExpr::DetachPolicy { .. });
    match expr {
        QueryExpr::DetachPolicy { policy_id, .. } => assert_eq!(policy_id, "p1"),
        other => panic!("expected DetachPolicy, got {other:?}"),
    }
}

#[test]
fn show_policies_parses() {
    let e = parse("SHOW POLICIES");
    matches!(e, QueryExpr::ShowPolicies { filter: None });
}

#[test]
fn show_policies_for_user_parses() {
    let e = parse("SHOW POLICIES FOR USER alice");
    match e {
        QueryExpr::ShowPolicies {
            filter: Some(PolicyPrincipalRef::User(u)),
        } => {
            assert_eq!(u.username, "alice");
        }
        other => panic!("expected ShowPolicies(User), got {other:?}"),
    }
}

#[test]
fn show_effective_permissions_parses() {
    let e = parse("SHOW EFFECTIVE PERMISSIONS FOR alice");
    matches!(e, QueryExpr::ShowEffectivePermissions { .. });
}

#[test]
fn simulate_parses() {
    let e = parse("SIMULATE alice ACTION select ON table:public.orders");
    match e {
        QueryExpr::SimulatePolicy {
            user,
            action,
            resource,
        } => {
            assert_eq!(user.username, "alice");
            assert_eq!(action, "select");
            assert_eq!(resource.kind, "table");
            assert_eq!(resource.name, "public.orders");
        }
        other => panic!("expected SimulatePolicy, got {other:?}"),
    }
}
