//! Structured audit log: every emit site outputs valid JSONL with the
//! new schema (action / principal / outcome / event_id / source / ts).
//!
//! This guards the SOC 2 / HIPAA contract: a downstream SIEM that
//! parses one event must be able to parse all events without
//! per-site exceptions.

use std::path::PathBuf;
use std::time::Duration;

use reddb::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditLogger, Outcome};

fn temp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "reddb-audit-structured-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.push("data.rdb");
    p
}

#[test]
fn record_event_emits_jsonl_with_required_fields() {
    let data = temp_path("required-fields");
    let logger = AuditLogger::for_data_path(&data);
    logger.record_event(
        AuditEvent::builder("auth/login.ok")
            .principal("alice@acme")
            .source(AuditAuthSource::Password)
            .tenant("acme")
            .outcome(Outcome::Success)
            .remote_addr("203.0.113.5")
            .correlation_id("req-1234")
            .build(),
    );
    assert!(logger.wait_idle(Duration::from_secs(2)));

    let body = std::fs::read_to_string(logger.path()).unwrap();
    let line = body.lines().next().expect("at least one line");

    let parsed = AuditEvent::parse_line(line).expect("line parses");
    assert_eq!(parsed.action, "auth/login.ok");
    assert_eq!(parsed.principal.as_deref(), Some("alice@acme"));
    assert_eq!(parsed.tenant.as_deref(), Some("acme"));
    assert_eq!(parsed.outcome, Outcome::Success);
    assert_eq!(parsed.source, AuditAuthSource::Password);
    assert_eq!(parsed.remote_addr.as_deref(), Some("203.0.113.5"));
    assert_eq!(parsed.correlation_id.as_deref(), Some("req-1234"));
    assert!(!parsed.event_id.is_empty());
    assert!(parsed.ts > 0);
}

#[test]
fn legacy_record_path_still_emits_structured_jsonl() {
    // Existing emit sites that haven't been migrated still call the
    // 5-arg `record(action, principal, target, result, details)` API.
    // The back-compat shim wraps them into the new schema so a SIEM
    // querying by `action` / `outcome` finds them all.
    let data = temp_path("legacy-shim");
    let logger = AuditLogger::for_data_path(&data);
    logger.record(
        "admin/shutdown",
        "operator",
        "instance",
        "ok",
        reddb::json::Value::Null,
    );
    logger.record(
        "admin/restore",
        "operator",
        "instance",
        "err: disk full",
        reddb::json::Value::Null,
    );
    assert!(logger.wait_idle(Duration::from_secs(2)));

    let body = std::fs::read_to_string(logger.path()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2);

    let ev0 = AuditEvent::parse_line(lines[0]).unwrap();
    assert_eq!(ev0.action, "admin/shutdown");
    assert_eq!(ev0.outcome, Outcome::Success);

    let ev1 = AuditEvent::parse_line(lines[1]).unwrap();
    assert_eq!(ev1.action, "admin/restore");
    assert_eq!(ev1.outcome, Outcome::Error);
}

#[test]
fn hash_chain_is_present_after_first_event() {
    let data = temp_path("hash-chain");
    let logger = AuditLogger::for_data_path(&data);
    for i in 0..3 {
        logger.record_event(
            AuditEvent::builder(format!("test/n/{i}"))
                .principal("system")
                .build(),
        );
    }
    assert!(logger.wait_idle(Duration::from_secs(2)));

    let body = std::fs::read_to_string(logger.path()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 3);
    // First line must NOT carry a prev_hash; subsequent lines must.
    assert!(!lines[0].contains("\"prev_hash\""));
    assert!(lines[1].contains("\"prev_hash\""));
    assert!(lines[2].contains("\"prev_hash\""));
}

#[test]
fn outcome_denied_distinct_from_error() {
    // Compliance reports often distinguish "explicitly denied"
    // (auth said no) from "system error" (the request failed for
    // other reasons). The schema must surface that distinction.
    let data = temp_path("denied");
    let logger = AuditLogger::for_data_path(&data);
    logger.record_event(
        AuditEvent::builder("auth/login.deny")
            .principal("eve")
            .source(AuditAuthSource::Password)
            .outcome(Outcome::Denied)
            .build(),
    );
    logger.record_event(
        AuditEvent::builder("admin/restore")
            .principal("operator")
            .outcome(Outcome::Error)
            .build(),
    );
    assert!(logger.wait_idle(Duration::from_secs(2)));

    let body = std::fs::read_to_string(logger.path()).unwrap();
    assert!(body.contains("\"outcome\":\"denied\""));
    assert!(body.contains("\"outcome\":\"error\""));
}

#[test]
fn event_ids_are_unique_across_burst() {
    let data = temp_path("ids");
    let logger = AuditLogger::for_data_path(&data);
    for i in 0..200 {
        logger.record_event(
            AuditEvent::builder(format!("test/burst/{i}"))
                .principal("burst")
                .build(),
        );
    }
    assert!(logger.wait_idle(Duration::from_secs(2)));

    let body = std::fs::read_to_string(logger.path()).unwrap();
    let mut ids: Vec<String> = Vec::new();
    for line in body.lines() {
        let ev = AuditEvent::parse_line(line).expect("parse");
        ids.push(ev.event_id);
    }
    ids.sort();
    let len = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), len, "event_id collisions detected");
}
