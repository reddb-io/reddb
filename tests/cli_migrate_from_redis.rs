//! Public CLI coverage for the Redis-to-Blob-Cache migration command.
//! These tests spawn the real `red` binary so help text, flag parsing,
//! exit status, and JSON envelopes stay wired through main().

#[allow(dead_code)]
mod support;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn one_shot_tcp_listener() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = listener.accept();
    });
    format!("redis://{addr}/0")
}

#[test]
fn migrate_from_redis_help_declares_cli_status() {
    let output = Command::new(red_binary())
        .args(["migrate-from-redis", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red migrate-from-redis --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("red migrate-from-redis"));
    assert!(stdout.contains("--dry-run"));
    assert!(stdout.contains("dual-write"));
    assert!(stdout.contains("application-owned helper"));
}

#[test]
fn migrate_from_redis_dry_run_validates_connectivity_without_writes() {
    let redis_url = one_shot_tcp_listener();
    let path = support::temp_db_file("cli-migrate-dry-run");
    let output = Command::new(red_binary())
        .args([
            "migrate-from-redis",
            "--dry-run",
            "--redis-url",
            &redis_url,
            "--path",
            path.to_str().unwrap(),
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red migrate-from-redis --dry-run");

    assert!(
        output.status.success(),
        "dry-run failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["command"], "migrate-from-redis");
    assert_eq!(body["data"]["mode"], "dry-run");
    assert_eq!(body["data"]["redis_reachable"], true);
    assert_eq!(body["data"]["reddb_reachable"], true);
    assert_eq!(body["data"]["entries_written"], 0);
    assert_eq!(body["data"]["mismatch_count"], 0);
}

#[test]
fn migrate_from_redis_dual_write_mode_points_to_application_helper() {
    let path = support::temp_db_file("cli-migrate-dual-write");
    let output = Command::new(red_binary())
        .args([
            "migrate-from-redis",
            "--phase",
            "dual-write",
            "--redis-url",
            "redis://127.0.0.1:1/0",
            "--path",
            path.to_str().unwrap(),
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red migrate-from-redis --phase dual-write");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let body: serde_json::Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["command"], "migrate-from-redis");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("application-owned helper"));
}
