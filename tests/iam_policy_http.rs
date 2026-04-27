//! Smoke tests for the IAM policy admin HTTP surface.
//!
//! Spins a real `RedDBServer` with an `AuthStore` attached, then drives
//! the admin endpoints (PUT/GET/DELETE policy, attach/detach,
//! simulator) over plain HTTP. The admin token gate is left unset so
//! the assertions exercise pure routing + handler behaviour.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use reddb::auth::{AuthConfig, AuthStore};
use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

fn isolated_runtime(tag: &str) -> RedDBRuntime {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "reddb-iam-http-{}-{}-{}",
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

fn spawn_server() -> String {
    std::env::remove_var("RED_ADMIN_TOKEN");
    let rt = isolated_runtime("base");
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: false,
        ..AuthConfig::default()
    }));
    store
        .create_user("alice", "p", reddb::auth::Role::Read)
        .unwrap();
    let server = RedDBServer::new(rt).with_auth(store);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    addr.to_string()
}

fn http_request(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n",);
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

#[test]
fn put_get_delete_policy_roundtrip() {
    let addr = spawn_server();
    let body = r#"{"id":"p1","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.orders"]}]}"#;

    // PUT (create)
    let (status, _) = http_request(&addr, "PUT", "/admin/policies/p1", Some(body));
    assert_eq!(status, 200);

    // GET
    let (status, body_text) = http_request(&addr, "GET", "/admin/policies/p1", None);
    assert_eq!(status, 200);
    assert!(body_text.contains("\"id\":\"p1\""), "body={body_text}");

    // LIST
    let (status, list_text) = http_request(&addr, "GET", "/admin/policies", None);
    assert_eq!(status, 200);
    assert!(list_text.contains("\"id\":\"p1\""), "list={list_text}");

    // DELETE
    let (status, _) = http_request(&addr, "DELETE", "/admin/policies/p1", None);
    assert_eq!(status, 204);

    // GET after delete
    let (status, _) = http_request(&addr, "GET", "/admin/policies/p1", None);
    assert_eq!(status, 404);
}

#[test]
fn attach_then_detach_user() {
    let addr = spawn_server();
    let body = r#"{"id":"p1","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.orders"]}]}"#;

    let (s1, _) = http_request(&addr, "PUT", "/admin/policies/p1", Some(body));
    assert_eq!(s1, 200);

    let (s2, _) = http_request(&addr, "PUT", "/admin/users/alice/policies/p1", None);
    assert_eq!(s2, 200);

    let (s3, eff) = http_request(
        &addr,
        "GET",
        "/admin/users/alice/effective-permissions",
        None,
    );
    assert_eq!(s3, 200);
    assert!(eff.contains("\"id\":\"p1\""), "eff={eff}");

    let (s4, _) = http_request(&addr, "DELETE", "/admin/users/alice/policies/p1", None);
    assert_eq!(s4, 204);
}

#[test]
fn group_membership_makes_group_policy_effective() {
    let addr = spawn_server();
    let body = r#"{"id":"p1","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.orders"]}]}"#;

    let (s1, _) = http_request(&addr, "PUT", "/admin/policies/p1", Some(body));
    assert_eq!(s1, 200);

    let (s2, _) = http_request(&addr, "PUT", "/admin/groups/analysts/policies/p1", None);
    assert_eq!(s2, 200);

    let (s3, _) = http_request(&addr, "PUT", "/admin/users/alice/groups/analysts", None);
    assert_eq!(s3, 200);

    let (s4, eff) = http_request(
        &addr,
        "GET",
        "/admin/users/alice/effective-permissions",
        None,
    );
    assert_eq!(s4, 200);
    assert!(eff.contains("\"id\":\"p1\""), "eff={eff}");

    let (s5, _) = http_request(&addr, "DELETE", "/admin/users/alice/groups/analysts", None);
    assert_eq!(s5, 204);

    let (s6, eff) = http_request(
        &addr,
        "GET",
        "/admin/users/alice/effective-permissions",
        None,
    );
    assert_eq!(s6, 200);
    assert!(!eff.contains("\"id\":\"p1\""), "eff={eff}");
}

#[test]
fn simulate_returns_decision_envelope() {
    let addr = spawn_server();
    let body = r#"{"id":"p1","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.orders"],"condition":{"source_ip":["10.0.0.5"]}}]}"#;

    let _ = http_request(&addr, "PUT", "/admin/policies/p1", Some(body));
    let _ = http_request(&addr, "PUT", "/admin/users/alice/policies/p1", None);

    let sim_body = r#"{"principal":"alice","action":"select","resource":{"kind":"table","name":"public.orders"},"ctx":{"source_ip":"10.0.0.5"}}"#;
    let (status, body_text) =
        http_request(&addr, "POST", "/admin/policies/simulate", Some(sim_body));
    assert_eq!(status, 200, "body={body_text}");
    // The kernel returns Decision::Allow → "allow".
    assert!(
        body_text.contains("\"decision\":\"allow\""),
        "body={body_text}"
    );
    assert!(
        body_text.contains("\"matched_policy_id\":\"p1\""),
        "body={body_text}"
    );
    assert!(body_text.contains("\"trail\""), "body={body_text}");
}

#[test]
fn simulate_default_deny_when_no_attachments() {
    let addr = spawn_server();
    let sim_body =
        r#"{"principal":"bob","action":"select","resource":{"kind":"table","name":"public.x"}}"#;
    let (status, body_text) =
        http_request(&addr, "POST", "/admin/policies/simulate", Some(sim_body));
    assert_eq!(status, 200, "body={body_text}");
    assert!(
        body_text.contains("\"decision\":\"default_deny\""),
        "body={body_text}"
    );
}
