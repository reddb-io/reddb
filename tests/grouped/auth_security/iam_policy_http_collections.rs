//! E2E tests for policy-aware direct HTTP collection endpoints (#754).
//!
//! Spins a real `RedDBServer` with an `AuthStore` carrying a Policy
//! and exercises representative read/write endpoints on a real
//! collection. The legacy role gate stays satisfied (alice is Admin
//! so the middleware lets her through); the new policy gate then
//! decides allow vs deny based on installed policies.
//!
//! These tests pin the contract of issue #754:
//!  - HTTP collection endpoints share the `/auth/can` action vocabulary.
//!  - Allow-granted reads/writes succeed.
//!  - Deny-by-default-deny reads/writes return a structured 403
//!    envelope (`action`, `resource`, `reason`) without leaking
//!    forbidden resources.
//!  - When IAM is inactive (no policies installed), the gate is a
//!    no-op and existing clients keep their current behavior.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};
use serde_json::{json, Value};

struct Harness {
    _dir: support::TempDataDir,
    addr: String,
    token: String,
    store: Arc<AuthStore>,
    runtime: RedDBRuntime,
}

fn isolated_runtime(tag: &str) -> (support::TempDataDir, RedDBRuntime) {
    let dir = support::temp_data_dir(&format!("iam-coll-http-{tag}"));
    let opts = RedDBOptions::in_memory().with_data_path(dir.join("data.rdb"));
    let rt = RedDBRuntime::with_options(opts).expect("runtime");
    (dir, rt)
}

fn spawn(tag: &str) -> Harness {
    std::env::remove_var("RED_ADMIN_TOKEN");
    let (dir, rt) = isolated_runtime(tag);
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("alice", "p", Role::Admin).unwrap();
    let key = store.create_api_key("alice", "ci", Role::Admin).unwrap();

    let server = RedDBServer::new(rt.clone()).with_auth(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    Harness {
        _dir: dir,
        addr: addr.to_string(),
        token: key.key,
        store,
        runtime: rt,
    }
}

fn http(
    addr: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(token) = bearer {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            b.len(),
            b
        ));
    } else {
        req.push_str("\r\n");
    }
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let resp = String::from_utf8_lossy(&buf).to_string();
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_idx = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    let body_text = resp[body_idx..].to_string();
    (status, body_text)
}

fn install_collection_policy(
    store: &AuthStore,
    id: &str,
    effect: &str,
    actions: &[&str],
    collection: &str,
) {
    let actions_json = actions
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"id":"{id}","version":1,"statements":[{{"effect":"{effect}","actions":[{actions_json}],"resources":["collection:{collection}"]}}]}}"#
    );
    let policy = Policy::from_json_str(&json).expect("parse policy");
    store.put_policy(policy).expect("put_policy");
    store
        .attach_policy(PrincipalRef::User(UserId::from_parts(None, "alice")), id)
        .expect("attach_policy");
}

fn create_collection(addr: &str, token: &str, name: &str) {
    let body = format!(r#"{{"name":"{name}","model":"kv"}}"#);
    let (status, body_text) = http(addr, "POST", "/collections", Some(token), Some(&body));
    assert!(
        status == 200 || status == 201,
        "create collection {name}: status={status} body={body_text}"
    );
}

fn seed_security_journey_collections(runtime: &RedDBRuntime) {
    for sql in [
        "CREATE TABLE public_orders (id INT)",
        "INSERT INTO public_orders (id) VALUES (1)",
        "CREATE TABLE private_orders (id INT)",
        "INSERT INTO private_orders (id) VALUES (2)",
        "CREATE DOCUMENT public_docs",
        "INSERT INTO public_docs DOCUMENT VALUES ({scope:'public'})",
        "CREATE DOCUMENT private_docs",
        "INSERT INTO private_docs DOCUMENT VALUES ({scope:'private'})",
        "CREATE KV public_cache",
        "KV PUT public_cache.seed = 'public'",
        "CREATE KV private_cache",
        "KV PUT private_cache.seed = 'private'",
    ] {
        runtime
            .execute_query(sql)
            .unwrap_or_else(|error| panic!("fixture `{sql}` failed: {error}"));
    }
}

fn install_security_prefix_guard(store: &AuthStore) {
    let policy = Policy::from_json_str(
        r#"{
            "id":"security-prefix-guard",
            "version":1,
            "statements":[
                {
                    "effect":"deny",
                    "actions":["select","insert","update","delete"],
                    "resources":["collection:private_*","table:private_*"]
                },
                {
                    "effect":"deny",
                    "actions":["kv:read","kv:write"],
                    "resources":["kv:private.*"]
                }
            ]
        }"#,
    )
    .expect("parse prefix guard");
    store.put_policy(policy).expect("store prefix guard");
    store
        .attach_policy(
            PrincipalRef::User(UserId::platform("alice")),
            "security-prefix-guard",
        )
        .expect("attach prefix guard");
}

fn parse_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|error| panic!("invalid JSON: {error}; {body}"))
}

fn assert_no_internal_details(body: &str) {
    let lower = body.to_ascii_lowercase();
    for forbidden in [
        "security-prefix-guard",
        "alice-password",
        "password_hash",
        "argon2",
        "backtrace",
        "stack trace",
        "reddberror",
        "src/",
        "src\\",
        ".rs:",
        "target/debug",
        "target\\debug",
        "c:\\users\\",
        "/home/",
    ] {
        assert!(!lower.contains(forbidden), "leaked `{forbidden}`: {body}");
    }
}

fn assert_public_policy_deny(body: &str, action: &str, collection: &str) -> Value {
    let value = parse_json(body);
    assert_eq!(value["ok"], false, "body={body}");
    assert_eq!(value["error"], "forbidden", "body={body}");
    assert_eq!(value["action"], action, "body={body}");
    assert_eq!(value["reason"], "denied by IAM policy", "body={body}");
    assert_eq!(value["resource"]["kind"], "collection", "body={body}");
    assert_eq!(value["resource"]["name"], collection, "body={body}");

    let mut keys: Vec<&str> = value
        .as_object()
        .expect("deny envelope object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["action", "error", "ok", "reason", "resource"]);
    value
}

// ---------------------------------------------------------------------------
// Legacy preservation: no policies installed → existing behavior is intact.
// ---------------------------------------------------------------------------

#[test]
fn no_policy_installed_legacy_behavior_preserved() {
    let h = spawn("legacy");
    create_collection(&h.addr, &h.token, "legacy_coll");

    // No policy has been installed → iam_authorization_enabled() is false →
    // the new policy gate is a no-op. Alice (Admin) reads succeed via the
    // legacy role gate.
    let (status, body) = http(
        &h.addr,
        "GET",
        "/collections/legacy_coll/scan",
        Some(&h.token),
        None,
    );
    assert_eq!(status, 200, "scan should pass legacy gate; body={body}");
}

// ---------------------------------------------------------------------------
// Allow path: a matching policy grants `select` on the collection.
// ---------------------------------------------------------------------------

#[test]
fn allow_policy_grants_scan_and_metadata() {
    let h = spawn("allow_read");
    create_collection(&h.addr, &h.token, "orders");
    install_collection_policy(&h.store, "p_read_orders", "allow", &["select"], "orders");

    // Scan over an empty collection still returns 200.
    let (status, body) = http(
        &h.addr,
        "GET",
        "/collections/orders/scan",
        Some(&h.token),
        None,
    );
    assert_eq!(status, 200, "scan allowed; body={body}");

    // Catalog metadata endpoint also runs through the gate (`select`).
    let (status, body) = http(
        &h.addr,
        "GET",
        "/catalog/collections/orders",
        Some(&h.token),
        None,
    );
    assert_eq!(status, 200, "metadata allowed; body={body}");
}

// ---------------------------------------------------------------------------
// Deny path: a policy permits `select` only → an `insert` falls to
// DefaultDeny under PolicyEnforcementMode::LegacyRbac for a Write/Admin
// role only when the verb is not in the legacy_rbac_decision allow-set
// for that role; under LegacyRbac mode admin would normally still
// pass. To force a clean deny on the new gate without depending on the
// enforcement-mode fallback, install an explicit `Deny` for the verb.
// ---------------------------------------------------------------------------

#[test]
fn explicit_deny_blocks_insert_with_structured_body() {
    let h = spawn("deny_insert");
    create_collection(&h.addr, &h.token, "audit");
    // Explicit Deny on `insert` for `collection:audit` → policy gate must
    // short-circuit even an Admin caller, since an explicit Deny wins
    // over both Allow and the LegacyRbac fallback.
    install_collection_policy(
        &h.store,
        "p_deny_audit_insert",
        "deny",
        &["insert"],
        "audit",
    );

    let row_body = r#"{"fields":{"k":"v"}}"#;
    let (status, body) = http(
        &h.addr,
        "POST",
        "/collections/audit/rows",
        Some(&h.token),
        Some(row_body),
    );
    assert_eq!(status, 403, "insert denied; body={body}");
    // The deny body uses the shared action/resource vocabulary so the
    // UI can render it the same way it renders `/auth/can` denies.
    assert!(body.contains("\"action\":\"insert\""), "body={body}");
    assert!(body.contains("\"kind\":\"collection\""), "body={body}");
    assert!(body.contains("\"name\":\"audit\""), "body={body}");
    assert!(body.contains("\"reason\""), "body={body}");
    // Reason must not leak the collection name beyond the resource
    // envelope (no other forbidden-resource enumeration).
    assert!(body.contains("\"ok\":false"), "body={body}");
}

#[test]
fn explicit_deny_blocks_scan_with_structured_body() {
    let h = spawn("deny_scan");
    create_collection(&h.addr, &h.token, "vault_logs");
    install_collection_policy(
        &h.store,
        "p_deny_vault_logs_read",
        "deny",
        &["select"],
        "vault_logs",
    );

    let (status, body) = http(
        &h.addr,
        "GET",
        "/collections/vault_logs/scan",
        Some(&h.token),
        None,
    );
    assert_eq!(status, 403, "scan denied; body={body}");
    assert!(body.contains("\"action\":\"select\""), "body={body}");
    assert!(body.contains("\"name\":\"vault_logs\""), "body={body}");
}

// ---------------------------------------------------------------------------
// Unknown / missing token → 401, never a leak of forbidden resources.
// ---------------------------------------------------------------------------

#[test]
fn unknown_token_returns_401_when_iam_active() {
    let h = spawn("unknown_token");
    create_collection(&h.addr, &h.token, "orders2");
    install_collection_policy(&h.store, "p_orders2_read", "allow", &["select"], "orders2");

    // The legacy auth middleware already rejects unknown tokens with 401
    // when require_auth=true. We assert the surface is consistent.
    let (status, _) = http(
        &h.addr,
        "GET",
        "/collections/orders2/scan",
        Some("rk_not_a_real_token"),
        None,
    );
    assert_eq!(status, 401);
}

// ---------------------------------------------------------------------------
// KV PUT/DELETE pass through the gate with `insert`/`delete` actions.
// ---------------------------------------------------------------------------

#[test]
fn kv_write_denied_when_policy_denies_insert() {
    let h = spawn("kv_deny");
    create_collection(&h.addr, &h.token, "cache");
    install_collection_policy(
        &h.store,
        "p_cache_deny_insert",
        "deny",
        &["insert"],
        "cache",
    );

    let (status, body) = http(
        &h.addr,
        "PUT",
        "/collections/cache/kvs/hello",
        Some(&h.token),
        Some(r#"{"value":"world"}"#),
    );
    assert_eq!(status, 403, "kv put denied; body={body}");
    assert!(body.contains("\"action\":\"insert\""), "body={body}");
    assert!(body.contains("\"name\":\"cache\""), "body={body}");
}

#[test]
fn kv_watch_denied_when_policy_denies_select() {
    let h = spawn("kv_watch_deny");
    create_collection(&h.addr, &h.token, "watch_cache");
    install_collection_policy(
        &h.store,
        "p_watch_cache_deny_select",
        "deny",
        &["select"],
        "watch_cache",
    );

    let (status, body) = http(
        &h.addr,
        "GET",
        "/collections/watch_cache/kvs/hello/watch",
        Some(&h.token),
        None,
    );
    assert_eq!(status, 403, "kv watch denied; body={body}");
    assert!(body.contains("\"action\":\"select\""), "body={body}");
    assert!(body.contains("\"name\":\"watch_cache\""), "body={body}");
}

#[test]
fn kv_tag_invalidation_denied_when_policy_denies_delete() {
    let h = spawn("kv_invalidate_deny");
    create_collection(&h.addr, &h.token, "tagged_cache");
    install_collection_policy(
        &h.store,
        "p_tagged_cache_deny_delete",
        "deny",
        &["delete"],
        "tagged_cache",
    );

    let (status, body) = http(
        &h.addr,
        "POST",
        "/collections/tagged_cache/kvs/invalidate_tags",
        Some(&h.token),
        Some(r#"{"tags":["customer:1"]}"#),
    );
    assert_eq!(status, 403, "kv tag invalidation denied; body={body}");
    assert!(body.contains("\"action\":\"delete\""), "body={body}");
    assert!(body.contains("\"name\":\"tagged_cache\""), "body={body}");
}

#[test]
fn security_journey_login_failures_do_not_reveal_identity_state() {
    let h = spawn("login-enumeration");
    h.store
        .create_user_in_tenant(Some("acme"), "alice", "alice-password", Role::Read)
        .expect("create acme/alice");

    let login = |tenant: &str, username: &str, password: &str| {
        let payload = json!({
            "tenant_id": tenant,
            "username": username,
            "password": password,
        })
        .to_string();
        http(&h.addr, "POST", "/auth/login", None, Some(&payload))
    };
    let failures = [
        login("acme", "unknown-user", "alice-password"),
        login("globex", "alice", "alice-password"),
        login("acme", "alice", "wrong-password"),
    ];

    for (status, body) in &failures {
        assert_eq!(*status, 401, "body={body}");
        assert_eq!(
            parse_json(body),
            json!({"ok": false, "error": "invalid credentials"})
        );
        assert_no_internal_details(body);
    }
    assert_eq!(failures[0].1, failures[1].1);
    assert_eq!(failures[1].1, failures[2].1);

    let (status, body) = login("acme", "alice", "alice-password");
    assert_eq!(status, 200, "body={body}");
    let success = parse_json(&body);
    assert_eq!(success["username"], "alice");
    assert_eq!(success["tenant_id"], "acme");
    assert!(success["token"]
        .as_str()
        .is_some_and(|token| !token.is_empty()));
    assert!(!body.contains("alice-password"));
}

#[test]
fn security_journey_prefix_policy_covers_table_document_and_kv() {
    let h = spawn("collection-prefix");
    seed_security_journey_collections(&h.runtime);
    install_security_prefix_guard(&h.store);

    for collection in ["public_orders", "public_docs", "public_cache"] {
        let (status, body) = http(
            &h.addr,
            "GET",
            &format!("/collections/{collection}/scan"),
            Some(&h.token),
            None,
        );
        assert_eq!(status, 200, "public collection {collection}: {body}");
    }

    for collection in ["private_orders", "private_docs", "private_cache"] {
        let (status, body) = http(
            &h.addr,
            "GET",
            &format!("/collections/{collection}/scan"),
            Some(&h.token),
            None,
        );
        assert_eq!(status, 403, "private collection {collection}: {body}");
        assert_public_policy_deny(&body, "select", collection);
        assert_no_internal_details(&body);
    }

    let (_, existing_body) = http(
        &h.addr,
        "GET",
        "/collections/private_orders/scan",
        Some(&h.token),
        None,
    );
    let (missing_status, missing_body) = http(
        &h.addr,
        "GET",
        "/collections/private_missing/scan",
        Some(&h.token),
        None,
    );
    assert_eq!(missing_status, 403);
    let mut existing = assert_public_policy_deny(&existing_body, "select", "private_orders");
    let mut missing = assert_public_policy_deny(&missing_body, "select", "private_missing");
    existing["resource"]["name"] = json!("<requested>");
    missing["resource"]["name"] = json!("<requested>");
    assert_eq!(existing, missing, "existence leaked through deny envelope");

    for (method, path, body) in [
        (
            "POST",
            "/collections/private_orders/rows",
            r#"{"fields":{"id":3}}"#,
        ),
        (
            "POST",
            "/collections/private_docs/documents",
            r#"{"body":{"scope":"private"}}"#,
        ),
        (
            "PUT",
            "/collections/private_cache/kvs/secret",
            r#"{"value":"hidden"}"#,
        ),
    ] {
        let (status, response_body) = http(&h.addr, method, path, Some(&h.token), Some(body));
        assert_eq!(status, 403, "{method} {path}: {response_body}");
        assert_eq!(parse_json(&response_body)["action"], "insert");
        assert_no_internal_details(&response_body);
    }
}

#[test]
fn security_journey_kv_prefix_deny_is_403_and_does_not_mutate() {
    let h = spawn("kv-prefix");
    install_security_prefix_guard(&h.store);

    let (status, body) = http(
        &h.addr,
        "POST",
        "/query",
        Some(&h.token),
        Some(r#"{"query":"SET KV public.setting = 'visible'"}"#),
    );
    assert_eq!(status, 200, "public KV write: {body}");
    assert_eq!(
        h.store.plain_kv_get("public.setting").as_deref(),
        Some("visible")
    );

    let (status, body) = http(
        &h.addr,
        "POST",
        "/query",
        Some(&h.token),
        Some(r#"{"query":"SET KV private.setting = 'hidden'"}"#),
    );
    assert_eq!(status, 403, "private KV write: {body}");
    assert_eq!(h.store.plain_kv_get("private.setting"), None);
    assert_eq!(
        parse_json(&body),
        json!({"ok": false, "error": "forbidden"})
    );
    assert_no_internal_details(&body);
}
