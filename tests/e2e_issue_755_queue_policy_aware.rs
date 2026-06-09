//! Issue #755 — queue operations are policy-aware.
//!
//! Pins the granular action names exposed for Red UI (`queue:enqueue`,
//! `queue:read`, `queue:peek`, `queue:ack`, `queue:nack`, `queue:retry`,
//! `queue:dlq:move`, `queue:purge`, `queue:presence:read`) and the
//! structured UI-safe denial envelope (`principal=… action=queue:…
//! resource=queue:… denied by IAM policy`).
//!
//! The tests run under `PolicyOnly` enforcement so the IAM evaluator is
//! exercised directly — `LegacyRbac` would let the role-based fallback
//! mask a missing grant.
//!
//! Refs #755 — child of PRD #735.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>, support::TempDataDir) {
    let dir = support::temp_data_dir("e2e-issue-755");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (rt, store, dir)
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

fn setup_queue(rt: &RedDBRuntime, queue: &str) {
    // CREATE QUEUE is a Write-role DDL; do it as admin so the test focus
    // remains on the per-operation policy check.
    as_user("admin", Role::Admin, || {
        rt.execute_query(&format!("CREATE QUEUE {queue}")).unwrap();
    });
}

#[test]
fn catalog_advertises_granular_queue_actions() {
    use reddb::auth::action_catalog::{is_valid_action, lookup};

    // The contract Red UI builds against: every granular verb is in
    // the catalog and validates as a policy action.
    for action in &[
        "queue:enqueue",
        "queue:read",
        "queue:peek",
        "queue:ack",
        "queue:nack",
        "queue:retry",
        "queue:dlq:move",
        "queue:purge",
        "queue:presence:read",
        "queue:*",
    ] {
        assert!(is_valid_action(action), "missing catalog entry: {action}");
        let entry = lookup(action).expect("lookup entry");
        // Description is non-empty so /admin/policies/actions stays
        // useful as a discovery surface.
        assert!(
            !entry.gates_description.is_empty(),
            "{action} missing gates_description",
        );
    }
}

#[test]
fn enqueue_allowed_with_queue_enqueue_grant() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    attach_alice_policy(
        &store,
        "queue-enqueue-jobs",
        r#"[
            {"effect":"allow","actions":["queue:enqueue"],"resources":["queue:jobs"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE PUSH jobs 'work'")
    })
    .unwrap();
}

#[test]
fn enqueue_denied_returns_structured_reason() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    // Grant peek but not enqueue: Red UI should see the producer
    // toolbar action rejected with a structured, UI-safe reason.
    attach_alice_policy(
        &store,
        "queue-peek-only",
        r#"[
            {"effect":"allow","actions":["queue:peek"],"resources":["queue:jobs"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE PUSH jobs 'work'")
    }));
    assert!(err.contains("action=`queue:enqueue`"), "got {err}");
    assert!(err.contains("resource=`queue:jobs`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
}

#[test]
fn peek_allowed_with_queue_peek_grant() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    as_user("admin", Role::Admin, || {
        rt.execute_query("QUEUE PUSH jobs 'work'").unwrap();
    });
    attach_alice_policy(
        &store,
        "queue-peek-jobs",
        r#"[
            {"effect":"allow","actions":["queue:peek"],"resources":["queue:jobs"]}
        ]"#,
    );

    let r = as_user("alice", Role::Write, || rt.execute_query("QUEUE PEEK jobs")).unwrap();
    assert_eq!(r.result.records.len(), 1);
}

#[test]
fn typed_queue_select_succeeds_with_queue_peek_grant() {
    // The typed `SELECT … FROM QUEUE <q>` projection maps to the
    // `QueueSelect` AST variant. It must succeed once the principal
    // carries a `queue:peek` grant on the target queue — pinning that
    // the `queue:peek` action covers the typed-select surface, not
    // just the `QUEUE PEEK` command.
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    as_user("admin", Role::Admin, || {
        rt.execute_query("QUEUE PUSH jobs 'work'").unwrap();
    });
    attach_alice_policy(
        &store,
        "queue-peek-select",
        r#"[
            {"effect":"allow","actions":["queue:peek"],"resources":["queue:jobs"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM QUEUE jobs")
    })
    .unwrap();
}

#[test]
fn read_pop_denied_without_queue_read_grant() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    as_user("admin", Role::Admin, || {
        rt.execute_query("QUEUE PUSH jobs 'work'").unwrap();
    });
    attach_alice_policy(
        &store,
        "queue-enqueue-only",
        r#"[
            {"effect":"allow","actions":["queue:enqueue"],"resources":["queue:jobs"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE POP jobs")
    }));
    assert!(err.contains("action=`queue:read`"), "got {err}");
    assert!(err.contains("resource=`queue:jobs`"), "got {err}");
}

#[test]
fn ack_and_nack_are_independently_grantable() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    // Seed two messages and read them as admin to acquire delivery
    // handles. The IAM check fires before runtime execution, so we only
    // need handles to exercise the ack/nack code paths under alice.
    as_user("admin", Role::Admin, || {
        rt.execute_query("QUEUE PUSH jobs 'a'").unwrap();
        rt.execute_query("QUEUE PUSH jobs 'b'").unwrap();
    });

    // alice can ack but not nack.
    attach_alice_policy(
        &store,
        "queue-ack-only",
        r#"[
            {"effect":"allow","actions":["queue:read","queue:ack"],"resources":["queue:jobs"]}
        ]"#,
    );

    // Ack must clear the IAM gate. With no live pending delivery the
    // runtime returns a NotFound — the error message must NOT mention
    // the IAM denial envelope.
    let ack_err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE ACK jobs GROUP g '1'")
    }));
    assert!(
        !ack_err.contains("denied by IAM policy"),
        "ack should pass the IAM gate, got {ack_err}"
    );

    // Nack must be rejected by the IAM gate before reaching the
    // runtime — error mentions the `queue:nack` verb.
    let nack_err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE NACK jobs GROUP g '1'")
    }));
    assert!(nack_err.contains("action=`queue:nack`"), "got {nack_err}");
}

#[test]
fn purge_requires_dedicated_grant() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    // Grant every consumer/producer verb but NOT purge — a destructive
    // toolbar action must not be reachable through a broad consumer
    // grant.
    attach_alice_policy(
        &store,
        "queue-no-purge",
        r#"[
            {"effect":"allow","actions":[
                "queue:enqueue","queue:read","queue:peek","queue:ack","queue:nack"
            ],"resources":["queue:jobs"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE PURGE jobs")
    }));
    assert!(err.contains("action=`queue:purge`"), "got {err}");

    // Granting queue:purge unblocks the operation.
    attach_alice_policy(
        &store,
        "queue-purge-jobs",
        r#"[
            {"effect":"allow","actions":["queue:purge"],"resources":["queue:jobs"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE PURGE jobs")
    })
    .unwrap();
}

#[test]
fn dlq_move_uses_dedicated_action() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    setup_queue(&rt, "jobs_dlq");

    // Broad consumer grant (no dlq:move): the DLQ replay toolbar must
    // refuse.
    attach_alice_policy(
        &store,
        "queue-no-dlq",
        r#"[
            {"effect":"allow","actions":["queue:read","queue:peek","queue:enqueue"],"resources":["queue:jobs","queue:jobs_dlq"]}
        ]"#,
    );
    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE MOVE FROM jobs TO jobs_dlq")
    }));
    assert!(err.contains("action=`queue:dlq:move`"), "got {err}");
    assert!(err.contains("resource=`queue:jobs`"), "got {err}");
}

#[test]
fn queue_wildcard_grants_every_operation() {
    let (rt, store, _dir) = runtime_with_auth();
    setup_queue(&rt, "jobs");
    attach_alice_policy(
        &store,
        "queue-wildcard",
        r#"[
            {"effect":"allow","actions":["queue:*"],"resources":["queue:jobs"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("QUEUE PUSH jobs 'a'").unwrap();
        rt.execute_query("QUEUE PEEK jobs").unwrap();
        rt.execute_query("QUEUE PURGE jobs").unwrap();
    });
}
