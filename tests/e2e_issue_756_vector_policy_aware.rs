//! Issue #756 — vector operations are policy-aware.
//!
//! Pins the granular action names exposed for Red UI (`vector:read`,
//! `vector:search`, `vector:artifact:read`, `vector:artifact:rebuild`,
//! `vector:admin`) and the structured UI-safe denial envelope
//! (`principal=… action=vector:… resource=vector:… denied by IAM
//! policy`).
//!
//! The tests run under `PolicyOnly` enforcement so the IAM evaluator
//! is exercised directly — `LegacyRbac` would let the role-based
//! fallback mask a missing grant.
//!
//! Refs #756 — child of PRD #735.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (support::TempDataDir, RedDBRuntime, Arc<AuthStore>) {
    let dir = support::temp_data_dir("issue-756");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
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

fn setup_vector_collection(rt: &RedDBRuntime, name: &str) {
    as_user("admin", Role::Admin, || {
        rt.execute_query(&format!("CREATE VECTOR {name} DIM 2 METRIC cosine"))
            .unwrap();
        // Seed one row so SEARCH has data to operate on; if the IAM
        // gate denies first the row is irrelevant.
        rt.execute_query(&format!(
            "INSERT INTO {name} VECTOR (dense, content) VALUES ([1.0, 0.0], 'hello')"
        ))
        .unwrap();
    });
}

#[test]
fn catalog_advertises_granular_vector_actions() {
    use reddb::auth::action_catalog::{is_valid_action, lookup};

    // The contract Red UI builds against: every granular verb is in
    // the catalog and validates as a policy action.
    for action in &[
        "vector:read",
        "vector:search",
        "vector:artifact:read",
        "vector:artifact:rebuild",
        "vector:admin",
        "vector:*",
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
fn search_allowed_with_vector_search_grant() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_vector_collection(&rt, "embeddings");
    // The vector executor checks a `select` projection on the
    // implicit `content` column (the snippet text shown alongside
    // similarity results) — grant it so this test pins the
    // `vector:search` gate rather than the unrelated column check.
    attach_alice_policy(
        &store,
        "vector-search-embeddings",
        r#"[
            {"effect":"allow","actions":["vector:search"],"resources":["vector:embeddings"]},
            {"effect":"allow","actions":["select"],"resources":["table:embeddings","column:embeddings.content"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1")
    })
    .unwrap();
}

#[test]
fn search_denied_returns_structured_reason() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_vector_collection(&rt, "embeddings");
    // Grant an unrelated vector verb so iam_authorization_enabled
    // flips on and the evaluator runs end-to-end.
    attach_alice_policy(
        &store,
        "vector-read-only",
        r#"[
            {"effect":"allow","actions":["vector:read"],"resources":["vector:embeddings"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1")
    }));
    assert!(err.contains("action=`vector:search`"), "got {err}");
    assert!(err.contains("resource=`vector:embeddings`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
}

#[test]
fn search_denied_when_grant_is_on_different_collection() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_vector_collection(&rt, "embeddings");
    setup_vector_collection(&rt, "other_vecs");
    // Grant only on `other_vecs` — the IAM gate must scope per
    // collection so a Red UI grant on one vector collection does not
    // leak into another.
    attach_alice_policy(
        &store,
        "vector-search-other",
        r#"[
            {"effect":"allow","actions":["vector:search"],"resources":["vector:other_vecs"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1")
    }));
    assert!(err.contains("action=`vector:search`"), "got {err}");
    assert!(err.contains("resource=`vector:embeddings`"), "got {err}");
}

#[test]
fn search_similar_text_form_uses_vector_search_action() {
    // `SEARCH SIMILAR [..] COLLECTION <c>` parses to the same
    // `QueryExpr::Vector` shape as `VECTOR SEARCH <c> SIMILAR TO […]`
    // so the IAM envelope must mention the same `vector:search` verb
    // — pin that contract for Red UI.
    let (_dir, rt, store) = runtime_with_auth();
    setup_vector_collection(&rt, "embeddings");
    attach_alice_policy(
        &store,
        "vector-search-text",
        r#"[
            {"effect":"allow","actions":["vector:read"],"resources":["vector:embeddings"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("SEARCH SIMILAR [1.0, 0.0] COLLECTION embeddings LIMIT 1")
    }));
    assert!(err.contains("action=`vector:search`"), "got {err}");
    assert!(err.contains("resource=`vector:embeddings`"), "got {err}");
}

#[test]
fn vector_wildcard_grants_search() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_vector_collection(&rt, "embeddings");
    attach_alice_policy(
        &store,
        "vector-wildcard",
        r#"[
            {"effect":"allow","actions":["vector:*"],"resources":["vector:embeddings"]},
            {"effect":"allow","actions":["select"],"resources":["table:embeddings","column:embeddings.content"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1")
            .unwrap();
    });
}

#[test]
fn search_grant_does_not_unlock_artifact_admin_verbs() {
    // Granting `vector:search` must not implicitly grant the
    // operational verbs `vector:artifact:read`,
    // `vector:artifact:rebuild`, or `vector:admin`. This is the
    // separation Red UI's blocker review pinned: artifact / admin
    // operations remain dedicated grants. We assert it through the
    // policy evaluator directly since the artifact / admin HTTP
    // surfaces have not yet been wired through the IAM gate (the
    // catalog entries land first; per-surface enforcement is a later
    // slice).
    use reddb::auth::policies::ResourceRef;

    let (_dir, _rt, store) = runtime_with_auth();
    attach_alice_policy(
        &store,
        "vector-search-only",
        r#"[
            {"effect":"allow","actions":["vector:search"],"resources":["vector:embeddings"]}
        ]"#,
    );

    let resource = ResourceRef::new("vector".to_string(), "embeddings".to_string());
    let ctx = reddb::auth::policies::EvalContext {
        principal_tenant: None,
        current_tenant: None,
        peer_ip: None,
        mfa_present: false,
        now_ms: 0,
        principal_is_admin_role: false,
        principal_is_system_owned: false,
        principal_is_platform_scoped: true,
    };
    let principal = reddb::auth::UserId::platform("alice");

    // The granted verb is honoured.
    assert!(store.check_policy_authz_with_role(
        &principal,
        "vector:search",
        &resource,
        &ctx,
        Role::Write
    ));

    // The other vector verbs are NOT honoured by a vector:search grant.
    for verb in &[
        "vector:artifact:read",
        "vector:artifact:rebuild",
        "vector:admin",
    ] {
        assert!(
            !store.check_policy_authz_with_role(&principal, verb, &resource, &ctx, Role::Write,),
            "{verb} must not be implied by a vector:search grant",
        );
    }
}

#[test]
fn vector_admin_verb_is_independently_grantable() {
    // Mirror of the queue ack/nack independence test (#755): a Red UI
    // toolbar that maps to `vector:admin` (clustering, maintenance)
    // must be grantable on its own without dragging search rights
    // along, and a grant that lacks it must reject the verb at the
    // policy evaluator.
    use reddb::auth::policies::ResourceRef;

    let (_dir, _rt, store) = runtime_with_auth();
    attach_alice_policy(
        &store,
        "vector-admin-only",
        r#"[
            {"effect":"allow","actions":["vector:admin"],"resources":["vector:embeddings"]}
        ]"#,
    );

    let resource = ResourceRef::new("vector".to_string(), "embeddings".to_string());
    let ctx = reddb::auth::policies::EvalContext {
        principal_tenant: None,
        current_tenant: None,
        peer_ip: None,
        mfa_present: false,
        now_ms: 0,
        principal_is_admin_role: false,
        principal_is_system_owned: false,
        principal_is_platform_scoped: true,
    };
    let principal = reddb::auth::UserId::platform("alice");

    assert!(store.check_policy_authz_with_role(
        &principal,
        "vector:admin",
        &resource,
        &ctx,
        Role::Write
    ));
    assert!(
        !store.check_policy_authz_with_role(
            &principal,
            "vector:search",
            &resource,
            &ctx,
            Role::Write
        ),
        "vector:admin grant must not imply vector:search",
    );
}
