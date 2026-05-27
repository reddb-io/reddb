//! Integration tests for the `AiProviderGate` (#711, S3).
//!
//! Acceptance criteria these tests pin:
//!
//! * `Deny ai:provider:openai` attached to a principal causes
//!   `ASK ... USING openai`, `INSERT ... WITH AUTO EMBED (...) USING
//!   openai`, and `SEARCH SIMILAR TEXT 'q' COLLECTION c USING openai`
//!   to fail at the planner with the typed policy error.
//! * The same principal can still run the same queries `USING
//!   huggingface` if no Deny matches huggingface (the gate lets the
//!   query through; downstream resolvers may then fail for other
//!   reasons like a missing API key — that's the back-compat shape).
//! * With no `ai:provider:*` policy attached, the gate is silent and
//!   the planner error (if any) comes from downstream layers, not
//!   from the gate.
//! * A planner-level `ai.provider.gate` audit event records the
//!   policy decision so operators can reconstruct both layers.
//! * No `ai.credential.resolve` audit event fires when the planner
//!   denied the query.

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>) {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "reddb-ai-provider-gate-{}-{now_nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("tempdir");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    // LegacyRbac (default) — Role::Admin grants the SQL surface so
    // statements reach the AI provider gate rather than failing at an
    // earlier RBAC check.
    store.create_user("alice", "p", Role::Admin).unwrap();
    store.create_user("bob", "p", Role::Admin).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (rt, store)
}

fn attach_user_policy(store: &AuthStore, user: &str, policy_json: &str) {
    let policy = reddb::auth::policies::Policy::from_json_str(policy_json).unwrap();
    let id = policy.id.clone();
    store.put_policy(policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(UserId::platform(user)),
            &id,
        )
        .unwrap();
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    clear_current_tenant();
    out
}

fn err_text<T: std::fmt::Debug>(r: Result<T, reddb::RedDBError>) -> String {
    r.expect_err("expected error").to_string()
}

fn gate_error_msg(principal: &str, token: &str) -> String {
    format!("policy: principal '{principal}' is not allowed to use AI provider '{token}'")
}

// --- ASK ----------------------------------------------------------------

#[test]
fn ask_using_denied_provider_fails_at_planner() {
    let (rt, store) = runtime_with_auth();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-no-openai",
            "version":1,
            "statements":[
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    let err = err_text(as_user("alice", Role::Admin, || {
        rt.execute_query("ASK 'what is going on' USING openai")
    }));
    assert!(
        err.contains(&gate_error_msg("alice", "openai")),
        "expected gate error, got: {err}"
    );
}

#[test]
fn ask_using_allowed_provider_passes_the_gate() {
    let (rt, store) = runtime_with_auth();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-no-openai",
            "version":1,
            "statements":[
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    // huggingface is not denied — the gate must let the planner pass.
    // The query will still fail downstream (no API key, no data) but
    // the error must NOT be the gate's typed policy error.
    let err = err_text(as_user("alice", Role::Admin, || {
        rt.execute_query("ASK 'what is going on' USING huggingface")
    }));
    assert!(
        !err.contains("is not allowed to use AI provider"),
        "huggingface should pass the gate; got gate error: {err}"
    );
}

#[test]
fn ask_back_compat_no_policy_attached_passes_the_gate() {
    let (rt, _store) = runtime_with_auth();
    // bob has no `ai:provider:*` policy attached — default-allow.
    let err = err_text(as_user("bob", Role::Admin, || {
        rt.execute_query("ASK 'hello' USING openai")
    }));
    assert!(
        !err.contains("is not allowed to use AI provider"),
        "default-allow back-compat must not produce a gate error; got: {err}"
    );
}

#[test]
fn ask_gate_records_planner_audit_event_on_deny() {
    let (rt, store) = runtime_with_auth();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-no-openai",
            "version":1,
            "statements":[
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    let _ = as_user("alice", Role::Admin, || {
        rt.execute_query("ASK 'what is going on' USING openai")
    });
    rt.audit_log().wait_idle(std::time::Duration::from_secs(2));

    let body = std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default();
    assert!(
        body.contains("ai.provider.gate"),
        "expected ai.provider.gate audit event; audit:\n{body}"
    );
    assert!(
        body.contains("\"outcome\":\"denied\""),
        "expected deny outcome in audit; audit:\n{body}"
    );
    // The credential resolver must NOT have been reached — no
    // `ai.credential.resolve` audit event should be present.
    assert!(
        !body.contains("ai.credential.resolve"),
        "credential resolver must not run when the gate denied; audit:\n{body}"
    );
}

// --- INSERT ... WITH AUTO EMBED USING -----------------------------------

#[test]
fn insert_auto_embed_using_denied_provider_fails_at_planner() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE notes (id INT, body TEXT)")
        .unwrap();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-insert-and-no-openai",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:notes"]},
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    let err = err_text(as_user("alice", Role::Admin, || {
        rt.execute_query(
            "INSERT INTO notes (id, body) VALUES (1, 'hello world') \
             WITH AUTO EMBED (body) USING openai",
        )
    }));
    assert!(
        err.contains(&gate_error_msg("alice", "openai")),
        "expected gate error, got: {err}"
    );
}

#[test]
fn insert_auto_embed_using_allowed_provider_passes_the_gate() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE notes (id INT, body TEXT)")
        .unwrap();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-insert-and-no-openai",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:notes"]},
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    let err = err_text(as_user("alice", Role::Admin, || {
        rt.execute_query(
            "INSERT INTO notes (id, body) VALUES (1, 'hello world') \
             WITH AUTO EMBED (body) USING huggingface",
        )
    }));
    assert!(
        !err.contains("is not allowed to use AI provider"),
        "huggingface should pass the gate; got gate error: {err}"
    );
}

// --- SEARCH SIMILAR ... USING -------------------------------------------

#[test]
fn search_similar_using_denied_provider_fails_at_planner() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE notes (id INT, body TEXT)")
        .unwrap();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-no-openai",
            "version":1,
            "statements":[
                {"effect":"deny","actions":["ai:provider:openai"],"resources":["ai-provider:openai"]}
            ]
        }"#,
    );

    let err = err_text(as_user("alice", Role::Admin, || {
        rt.execute_query("SEARCH SIMILAR TEXT 'hello world' COLLECTION notes USING openai")
    }));
    assert!(
        err.contains(&gate_error_msg("alice", "openai")),
        "expected gate error, got: {err}"
    );
}

// --- Wildcard coverage --------------------------------------------------

#[test]
fn wildcard_ai_provider_deny_blocks_all_providers() {
    let (rt, store) = runtime_with_auth();
    attach_user_policy(
        &store,
        "alice",
        r#"{
            "id":"alice-no-ai",
            "version":1,
            "statements":[
                {"effect":"deny","actions":["ai:provider:*"],"resources":["ai-provider:*"]}
            ]
        }"#,
    );

    for provider in ["openai", "huggingface", "anthropic", "groq"] {
        let err = err_text(as_user("alice", Role::Admin, || {
            rt.execute_query(&format!("ASK 'hello' USING {provider}"))
        }));
        assert!(
            err.contains(&gate_error_msg("alice", provider)),
            "expected gate error for {provider}, got: {err}"
        );
    }
}
