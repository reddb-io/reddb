//! Audit log rotation contract.
//!
//! When the active `.audit.log` exceeds `max_bytes`, the writer
//! renames it to `.audit.log.<ms>.zst`, zstd-compresses it, and
//! starts a fresh active file. The query endpoint reads across both
//! the active file and the rotated archives; that is verified in
//! `audit_query_endpoint.rs`.

use std::path::PathBuf;
use std::time::Duration;

use reddb::runtime::audit_log::{AuditEvent, AuditLogger};
use reddb::runtime::audit_query::{run_query, AuditQuery};

fn temp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "reddb-audit-rotation-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn rotates_at_threshold_and_compresses() {
    let dir = temp_dir("rot-threshold");
    let path = dir.join(".audit.log");
    let logger = AuditLogger::with_max_bytes(path.clone(), 1024);

    for i in 0..50 {
        logger.record_event(
            AuditEvent::builder(format!("test/rotate/{i}"))
                .principal("rotator")
                .detail(reddb::json::Value::String(
                    "padding-padding-padding-padding-padding".to_string(),
                ))
                .build(),
        );
    }
    assert!(logger.wait_idle(Duration::from_secs(3)));

    let mut rotated: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let n = entry.file_name();
        let s = n.to_string_lossy().to_string();
        if s.starts_with(".audit.log.") && s.ends_with(".zst") {
            rotated.push(entry.path());
        }
    }
    assert!(
        !rotated.is_empty(),
        "expected at least one rotated .zst archive"
    );

    // Active file still exists and is fresh (smaller than threshold-ish).
    let active_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert!(
        active_size > 0,
        "active file should still receive new lines"
    );
}

#[test]
fn query_reads_across_rotated_files() {
    let dir = temp_dir("rot-query");
    let path = dir.join(".audit.log");
    let logger = AuditLogger::with_max_bytes(path.clone(), 1024);

    // Drop enough events to force at least one rotation.
    for _ in 0..50 {
        logger.record_event(
            AuditEvent::builder("auth/login.ok")
                .principal("alice")
                .tenant("acme")
                .build(),
        );
    }
    // Tag a few outside-the-tenant events so the filter has
    // something to reject across rotation boundaries.
    logger.record_event(
        AuditEvent::builder("auth/login.ok")
            .principal("bob")
            .tenant("zenith")
            .build(),
    );
    for _ in 0..50 {
        logger.record_event(
            AuditEvent::builder("auth/login.ok")
                .principal("alice")
                .tenant("acme")
                .build(),
        );
    }
    assert!(logger.wait_idle(Duration::from_secs(3)));

    let q = AuditQuery {
        principal: Some("alice".to_string()),
        action_prefix: Some("auth/".to_string()),
        limit: 1000,
        ..Default::default()
    };
    let hits = run_query(&path, &q);
    assert_eq!(
        hits.len(),
        100,
        "expected all 100 alice events across rotations"
    );
    for ev in &hits {
        assert_eq!(ev.principal.as_deref(), Some("alice"));
        assert_eq!(ev.tenant.as_deref(), Some("acme"));
    }
}
