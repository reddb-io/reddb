//! Issue #757 — graph analytics and traversal are policy-aware.
//!
//! Pins the granular action names exposed for Red UI (`graph:read`,
//! `graph:traverse`, `graph:algorithm:run`, `graph:*`) and the
//! structured UI-safe denial envelope (`principal=… action=graph:…
//! resource=graph:* denied by IAM policy`).
//!
//! The tests run under `PolicyOnly` enforcement so the IAM evaluator
//! is exercised directly — `LegacyRbac` would let the role-based
//! fallback mask a missing grant.
//!
//! Refs #757 — child of PRD #735.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (support::TempDataDir, RedDBRuntime, Arc<AuthStore>) {
    let dir = support::temp_data_dir("issue-757");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Read).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (dir, rt, store)
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_alice_policy(store: &AuthStore, id: &str, statements: &str) {
    let policy = format!(
        r#"{{
        "id":"{id}",
        "version":1,
        "statements":{statements}
    }}"#
    );
    store
        .put_policy(reddb::auth::policies::Policy::from_json_str(&policy).unwrap())
        .unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            id,
        )
        .unwrap();
}

fn err_string<T: std::fmt::Debug>(result: Result<T, reddb::RedDBError>) -> String {
    format!("{:?}", result.unwrap_err())
}

/// Seed a small graph (`alice -KNOWS-> bob`) so traversal /
/// neighborhood / properties queries have data to operate on. CREATE
/// + INSERT run as admin so the test focus stays on the read-side
/// policy check.
fn seed_graph(rt: &RedDBRuntime) {
    as_user("admin", Role::Admin, || {
        rt.execute_query("CREATE GRAPH tales").unwrap();
        rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
            .unwrap();
        rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('bob', 'Bob')")
            .unwrap();
        rt.execute_query(
            "INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', 'alice', 'bob')",
        )
        .unwrap();
    });
}

#[test]
fn catalog_advertises_graph_actions() {
    use reddb::auth::action_catalog::{is_valid_action, lookup};

    // The contract Red UI builds against: every verb in the action
    // class set is in the catalog and validates as a policy action.
    for action in &[
        "graph:read",
        "graph:traverse",
        "graph:algorithm:run",
        "graph:*",
    ] {
        assert!(is_valid_action(action), "missing catalog entry: {action}");
        let entry = lookup(action).expect("lookup entry");
        assert!(
            !entry.gates_description.is_empty(),
            "{action} missing gates_description",
        );
    }
}

#[test]
fn properties_allowed_with_graph_read_grant() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    attach_alice_policy(
        &store,
        "graph-read-only",
        r#"[
            {"effect":"allow","actions":["graph:read"],"resources":["graph:*"]}
        ]"#,
    );

    as_user("alice", Role::Read, || rt.execute_query("GRAPH PROPERTIES"))
        .expect("graph:read grant clears GRAPH PROPERTIES");
}

#[test]
fn properties_denied_returns_structured_reason() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    // Grant traversal but not metadata read — Red UI should see the
    // properties toolbar action rejected with a structured envelope.
    attach_alice_policy(
        &store,
        "graph-traverse-only",
        r#"[
            {"effect":"allow","actions":["graph:traverse"],"resources":["graph:*"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Read, || {
        rt.execute_query("GRAPH PROPERTIES")
    }));
    assert!(err.contains("action=`graph:read`"), "got {err}");
    assert!(err.contains("resource=`graph:*`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
}

#[test]
fn neighborhood_allowed_with_graph_traverse_grant() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    attach_alice_policy(
        &store,
        "graph-traverse-allow",
        r#"[
            {"effect":"allow","actions":["graph:traverse"],"resources":["graph:*"]}
        ]"#,
    );

    as_user("alice", Role::Read, || {
        rt.execute_query("GRAPH NEIGHBORHOOD 'alice'")
    })
    .expect("graph:traverse grant clears GRAPH NEIGHBORHOOD");
}

#[test]
fn match_pattern_query_uses_graph_traverse() {
    // The `MATCH … RETURN` surface maps to `QueryExpr::Graph` — it
    // must gate on `graph:traverse`, not `graph:read`, so the
    // explorer's pattern-walk toolbar action is independently
    // grantable from plain metadata reads.
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    attach_alice_policy(
        &store,
        "graph-read-only-no-traverse",
        r#"[
            {"effect":"allow","actions":["graph:read"],"resources":["graph:*"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Read, || {
        rt.execute_query("MATCH (a) RETURN a")
    }));
    assert!(err.contains("action=`graph:traverse`"), "got {err}");
    assert!(err.contains("resource=`graph:*`"), "got {err}");
}

#[test]
fn neighborhood_denied_without_traverse_grant() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    attach_alice_policy(
        &store,
        "graph-read-only-no-traverse-2",
        r#"[
            {"effect":"allow","actions":["graph:read"],"resources":["graph:*"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Read, || {
        rt.execute_query("GRAPH NEIGHBORHOOD 'alice'")
    }));
    assert!(err.contains("action=`graph:traverse`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
}

#[test]
fn algorithm_run_requires_dedicated_grant() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    // Grant read + traversal but NOT algorithm — the expensive
    // analytics toolbar must not be reachable through a plain
    // traversal grant.
    attach_alice_policy(
        &store,
        "graph-no-algo",
        r#"[
            {"effect":"allow","actions":["graph:read","graph:traverse"],"resources":["graph:*"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Read, || {
        rt.execute_query("GRAPH CENTRALITY")
    }));
    assert!(err.contains("action=`graph:algorithm:run`"), "got {err}");
    assert!(err.contains("resource=`graph:*`"), "got {err}");

    // Granting graph:algorithm:run unblocks the call.
    attach_alice_policy(
        &store,
        "graph-algo-run",
        r#"[
            {"effect":"allow","actions":["graph:algorithm:run"],"resources":["graph:*"]}
        ]"#,
    );
    as_user("alice", Role::Read, || rt.execute_query("GRAPH CENTRALITY"))
        .expect("graph:algorithm:run grant clears GRAPH CENTRALITY");
}

#[test]
fn graph_wildcard_grants_every_operation() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_graph(&rt);
    attach_alice_policy(
        &store,
        "graph-wildcard",
        r#"[
            {"effect":"allow","actions":["graph:*"],"resources":["graph:*"]}
        ]"#,
    );

    as_user("alice", Role::Read, || {
        rt.execute_query("GRAPH PROPERTIES").unwrap();
        rt.execute_query("GRAPH NEIGHBORHOOD 'alice'").unwrap();
        rt.execute_query("GRAPH CENTRALITY").unwrap();
    });
}
