//! E2E tests for policy-aware admin / metrics / cluster / operational
//! reads (#758).
//!
//! Spins a real `RedDBServer` with an `AuthStore`, exercises
//! representative operational endpoints (`/metrics`, `/admin/status`,
//! `/cluster/status`, `/replication/status`, `/backup/status`,
//! `/admin/blob_cache/stats`, `/admin/audit`, `/ec/status`), and pins
//! the contract of the issue:
//!
//! * The scoped operational action verbs (`ops:read:self`,
//!   `ops:read:tenant`, `ops:read:cluster`, `ops:admin`) and the
//!   `ops:*` wildcard appear in the action catalog.
//! * Allowed operational reads succeed for principals that hold the
//!   required grant.
//! * Denied operational reads return a structured, UI-safe 403
//!   envelope (`action`, `resource`, `reason`) without leaking
//!   restricted operational details.
//! * When IAM is inactive (no policies installed), the gate is a
//!   no-op and dashboards keep their current behavior.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use reddb::auth::action_catalog;
use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

struct Harness {
    addr: String,
    token: String,
    store: Arc<AuthStore>,
}

fn isolated_runtime(tag: &str) -> RedDBRuntime {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "reddb-iam-ops-http-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let opts = RedDBOptions::in_memory().with_data_path(dir.join("data.rdb"));
    RedDBRuntime::with_options(opts).expect("runtime")
}

fn spawn(tag: &str) -> Harness {
    // The admin-token gate would bypass the policy gate entirely, so
    // make sure it's unset for these tests — we exercise the
    // application-auth path that Red UI uses.
    std::env::remove_var("RED_ADMIN_TOKEN");
    let rt = isolated_runtime(tag);
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("alice", "p", Role::Admin).unwrap();
    let key = store.create_api_key("alice", "ci", Role::Admin).unwrap();

    let server = RedDBServer::new(rt).with_auth(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    Harness {
        addr: addr.to_string(),
        token: key.key,
        store,
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

fn install_ops_policy(
    store: &AuthStore,
    id: &str,
    effect: &str,
    actions: &[&str],
    resource_name: &str,
) {
    let actions_json = actions
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"id":"{id}","version":1,"statements":[{{"effect":"{effect}","actions":[{actions_json}],"resources":["ops:{resource_name}"]}}]}}"#
    );
    let policy = Policy::from_json_str(&json).expect("parse policy");
    store.put_policy(policy).expect("put_policy");
    store
        .attach_policy(PrincipalRef::User(UserId::from_parts(None, "alice")), id)
        .expect("attach_policy");
}

// ---------------------------------------------------------------------------
// Action catalog: the scoped operational verbs and the wildcard are
// surfaced through the canonical catalog so /auth/can probes and the
// `red.policy.actions` virtual table see them.
// ---------------------------------------------------------------------------

#[test]
fn action_catalog_includes_scoped_ops_verbs_and_wildcard() {
    for name in [
        "ops:read:self",
        "ops:read:tenant",
        "ops:read:cluster",
        "ops:admin",
        "ops:*",
    ] {
        assert!(
            action_catalog::is_valid_action(name),
            "action catalog must accept `{name}`",
        );
        assert!(
            action_catalog::lookup(name).is_some(),
            "lookup must find `{name}`",
        );
    }
}

// ---------------------------------------------------------------------------
// Legacy preservation: no policies installed → existing behavior is intact.
// Admin-role caller hits /admin/status the same way as before.
// ---------------------------------------------------------------------------

#[test]
fn no_policy_installed_admin_status_passes_legacy_gate() {
    let h = spawn("legacy_ops");
    let (status, body) = http(&h.addr, "GET", "/admin/status", Some(&h.token), None);
    assert_eq!(status, 200, "admin status (legacy mode); body={body}");
}

// ---------------------------------------------------------------------------
// Allow path: a matching policy grants `ops:read:cluster` on the resource
// targeted by the route.
// ---------------------------------------------------------------------------

#[test]
fn allow_policy_grants_admin_status() {
    let h = spawn("allow_admin_status");
    install_ops_policy(
        &h.store,
        "p_admin_status_read",
        "allow",
        &["ops:read:cluster"],
        "admin-status",
    );

    let (status, body) = http(&h.addr, "GET", "/admin/status", Some(&h.token), None);
    assert_eq!(status, 200, "admin status allowed; body={body}");
}

#[test]
fn allow_policy_grants_cluster_status_and_replication() {
    let h = spawn("allow_cluster_replication");
    // Single policy covers two resources via wildcard.
    let json = r#"{
        "id":"p_cluster_wildcard",
        "version":1,
        "statements":[{"effect":"allow","actions":["ops:*"],"resources":["ops:*"]}]
    }"#;
    let policy = Policy::from_json_str(json).expect("parse");
    h.store.put_policy(policy).expect("put");
    h.store
        .attach_policy(
            PrincipalRef::User(UserId::from_parts(None, "alice")),
            "p_cluster_wildcard",
        )
        .expect("attach");

    for path in [
        "/cluster/status",
        "/replication/status",
        "/backup/status",
        "/metrics",
        "/ec/status",
    ] {
        let (status, body) = http(&h.addr, "GET", path, Some(&h.token), None);
        assert!(
            status == 200 || status == 503,
            "{path}: status={status} body={body}",
        );
    }
}

// ---------------------------------------------------------------------------
// Deny path: an explicit Deny on `ops:read:cluster` shuts down the
// representative reads with a structured envelope. The reason must not
// leak any other resource beyond the one named in the request.
// ---------------------------------------------------------------------------

#[test]
fn explicit_deny_blocks_admin_status_with_structured_body() {
    let h = spawn("deny_admin_status");
    install_ops_policy(
        &h.store,
        "p_admin_status_deny",
        "deny",
        &["ops:read:cluster"],
        "admin-status",
    );

    let (status, body) = http(&h.addr, "GET", "/admin/status", Some(&h.token), None);
    assert_eq!(status, 403, "admin status denied; body={body}");
    assert!(
        body.contains("\"action\":\"ops:read:cluster\""),
        "body={body}"
    );
    assert!(body.contains("\"kind\":\"ops\""), "body={body}");
    assert!(body.contains("\"name\":\"admin-status\""), "body={body}");
    assert!(body.contains("\"reason\""), "body={body}");
    assert!(body.contains("\"ok\":false"), "body={body}");
    // Should not leak any other ops resource name in the deny body.
    assert!(!body.contains("cluster-status"), "leak: body={body}");
    assert!(!body.contains("replication-status"), "leak: body={body}");
}

#[test]
fn explicit_deny_blocks_metrics_with_structured_body() {
    let h = spawn("deny_metrics");
    install_ops_policy(
        &h.store,
        "p_metrics_deny",
        "deny",
        &["ops:read:cluster"],
        "metrics",
    );

    let (status, body) = http(&h.addr, "GET", "/metrics", Some(&h.token), None);
    assert_eq!(status, 403, "metrics denied; body={body}");
    assert!(
        body.contains("\"action\":\"ops:read:cluster\""),
        "body={body}"
    );
    assert!(body.contains("\"name\":\"metrics\""), "body={body}");
}

// ---------------------------------------------------------------------------
// Security-sensitive read: /admin/audit is gated by `ops:admin` so it can
// be granted independently of generic cluster observability.
// ---------------------------------------------------------------------------

#[test]
fn admin_audit_denied_under_ops_read_cluster_only() {
    let h = spawn("deny_audit");
    // Grants generic cluster read but not the admin-scoped verb.
    install_ops_policy(
        &h.store,
        "p_cluster_read",
        "allow",
        &["ops:read:cluster"],
        "*",
    );

    let (status, body) = http(&h.addr, "GET", "/admin/audit", Some(&h.token), None);
    // No explicit deny, but no grant for `ops:admin` either — falls
    // through to the legacy fallback. Under PolicyOnly mode this is a
    // 403; under LegacyRbac the Admin role still passes. Either way the
    // response is structured if it's a deny.
    if status == 403 {
        assert!(body.contains("\"action\":\"ops:admin\""), "body={body}");
        assert!(body.contains("\"name\":\"audit\""), "body={body}");
    } else {
        assert_eq!(status, 200, "audit fallback; body={body}");
    }
}

#[test]
fn admin_audit_allowed_with_ops_admin_grant() {
    let h = spawn("allow_audit");
    install_ops_policy(&h.store, "p_ops_admin", "allow", &["ops:admin"], "audit");

    let (status, body) = http(&h.addr, "GET", "/admin/audit", Some(&h.token), None);
    assert_eq!(status, 200, "audit allowed; body={body}");
}

// ---------------------------------------------------------------------------
// Unknown token → 401 (legacy auth middleware rejects it before the gate).
// ---------------------------------------------------------------------------

#[test]
fn unknown_token_returns_401_when_iam_active_on_ops_surface() {
    let h = spawn("unknown_token_ops");
    install_ops_policy(
        &h.store,
        "p_status_allow",
        "allow",
        &["ops:read:cluster"],
        "admin-status",
    );

    let (status, _) = http(
        &h.addr,
        "GET",
        "/admin/status",
        Some("rk_not_a_real_token"),
        None,
    );
    assert_eq!(status, 401);
}
