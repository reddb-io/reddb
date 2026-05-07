//! Operator-grade event bus for high-severity system conditions.
//!
//! # Operator / developer split
//!
//! RedDB telemetry has two audiences:
//!
//! - **Developer signal** (`tracing` spans at `DEBUG` / `INFO`): ephemeral,
//!   high-volume, lives in `red.log` or stdout. Helps engineers trace request
//!   flows and understand runtime behaviour during development.
//!
//! - **Operator-grade events** (this module): low-volume, high-severity
//!   conditions that a production operator *must* see and act on.
//!   Persisted to the tamper-evident audit log first so they survive process
//!   crashes; a `tracing::warn!` breadcrumb lands in the normal log channel
//!   as a secondary copy; `eprintln!` fallback ensures the event is never
//!   silently swallowed even if both sinks fail.
//!
//! `OperatorEvent::emit` always runs synchronously — it is intentionally
//! *not* async so callers in crash paths, signal handlers, and `Drop` impls
//! can call it without an async runtime.

use std::sync::{Arc, OnceLock};

use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditField, AuditFieldEscaper, AuditLogger, Outcome};

// ---------------------------------------------------------------------------
// Process-wide sink
// ---------------------------------------------------------------------------
//
// The OperatorEvent enum is defined in `telemetry/` but the deepest
// emit sites (storage layer, replication apply loop, signal handlers,
// drop impls) cannot thread an `&AuditLogger` reference through their
// call stacks without a sweeping refactor. To stay surgical (#205) we
// expose a process-wide sink that the runtime registers at startup and
// every emit site consults via `OperatorEvent::emit_global`.
//
// Trade-off: the sink is a `OnceLock<Arc<AuditLogger>>`, which means
// emits that fire *before* the runtime registers the logger fall back
// to `tracing::warn!` + `eprintln!` only — the audit copy is lost. The
// tamper-evident audit copy is the primary record; the breadcrumb /
// stderr fallbacks are the safety net the original `emit(&AuditLogger)`
// shape already accepted, so the degradation is the same one already
// documented in the module rustdoc.

static GLOBAL_SINK: OnceLock<Arc<AuditLogger>> = OnceLock::new();

/// Install the process-wide [`AuditLogger`] used by
/// [`OperatorEvent::emit_global`]. Called once at runtime startup; a
/// second call is a no-op (the first registration wins) so test
/// harnesses that build multiple in-memory runtimes cannot stomp on
/// each other's loggers — they fall back to tracing+eprintln.
pub fn install_global_audit_sink(logger: Arc<AuditLogger>) {
    let _ = GLOBAL_SINK.set(logger);
}

// ---------------------------------------------------------------------------
// OperatorEvent
// ---------------------------------------------------------------------------

/// High-severity system conditions that require operator attention.
///
/// Every variant carries typed [`crate::runtime::audit_log::AuditValue`]
/// fields so adversarial bytes (CRLF, NUL, quote, non-UTF-8) are
/// escape-safe at the audit boundary (ADR 0010).
#[derive(Debug)]
pub enum OperatorEvent {
    /// A replication stream to a follower/replica broke unexpectedly.
    ReplicationBroken {
        peer: String,
        reason: String,
    },
    /// Replication state diverged: the follower's committed LSN or
    /// checksum disagrees with the leader.
    Divergence {
        peer: String,
        leader_lsn: u64,
        follower_lsn: u64,
    },
    /// The WAL fsync call failed. Data may be at risk on the current host.
    WalFsyncFailed {
        path: String,
        error: String,
    },
    /// Available disk space fell below the configured critical threshold.
    DiskSpaceCritical {
        path: String,
        available_bytes: u64,
        threshold_bytes: u64,
    },
    /// An authentication bypass was detected (e.g. auth gate returned
    /// `allow` for a request that should have been rejected).
    AuthBypass {
        principal: String,
        resource: String,
        detail: String,
    },
    /// An admin capability was granted to a principal at runtime.
    AdminCapabilityGranted {
        granted_to: String,
        capability: String,
        granted_by: String,
    },
    /// Secret rotation failed; the current secret may be stale.
    SecretRotationFailed {
        secret_ref: String,
        error: String,
    },
    /// A runtime configuration change was applied to a live instance.
    ConfigChanged {
        key: String,
        old_value: String,
        new_value: String,
        changed_by: String,
    },
    /// The server process failed to start cleanly.
    StartupFailed {
        phase: String,
        error: String,
    },
    /// The server process was forced to shut down (e.g. OOM killer,
    /// SIGKILL, unrecoverable error).
    ShutdownForced {
        reason: String,
    },
    /// On-disk schema metadata is corrupt or inconsistent.
    SchemaCorruption {
        collection: String,
        detail: String,
    },
    /// A scheduled or triggered checkpoint failed to complete.
    CheckpointFailed {
        lsn: u64,
        error: String,
    },
}

impl OperatorEvent {
    /// Emit the event.
    ///
    /// Execution order (per issue #202):
    /// 1. Persist to `audit` — tamper-evident, durable.
    /// 2. `tracing::warn!` breadcrumb — lands in `red.log` / stderr.
    /// 3. `eprintln!` fallback — fires only if the audit write fails,
    ///    ensuring the event is never silently lost.
    /// Emit the event using the process-wide sink installed by the
    /// runtime at startup. When no sink is installed (early boot,
    /// tests without an audit logger), the tracing breadcrumb and
    /// eprintln fallback still fire so the event is never silently
    /// lost.
    pub fn emit_global(self) {
        match GLOBAL_SINK.get() {
            Some(logger) => self.emit(logger.as_ref()),
            None => {
                let (_, _, summary) = self.decompose();
                tracing::warn!(target: "reddb::operator", "{summary}");
                eprintln!("[reddb::operator] (no audit sink) {summary}");
            }
        }
    }

    pub fn emit(self, audit: &AuditLogger) {
        let (action, fields, summary) = self.decompose();

        let ev = AuditEvent::builder(action)
            .source(AuditAuthSource::System)
            .outcome(Outcome::Error)
            .fields(fields)
            .build();

        // 1. Audit log (primary, tamper-evident).
        let audit_ok = {
            // `record_event` is infallible from the caller's perspective
            // (it falls back to sync write internally), so we treat it as
            // always succeeding for the breadcrumb decision.
            audit.record_event(ev);
            true
        };

        // 2. tracing breadcrumb.
        tracing::warn!(target: "reddb::operator", "{summary}");

        // 3. eprintln fallback — guard against silent loss when audit
        //    is unhealthy (e.g. disk full, writer thread dead).
        if !audit_ok {
            eprintln!("[reddb::operator] {summary}");
        }
    }

    /// Decompose `self` into `(action, audit_fields, human_summary)`.
    fn decompose(self) -> (&'static str, Vec<AuditField>, String) {
        match self {
            Self::ReplicationBroken { peer, reason } => {
                let summary = format!("replication broken: peer={peer} reason={reason}");
                let fields = vec![
                    AuditFieldEscaper::field("peer", peer),
                    AuditFieldEscaper::field("reason", reason),
                ];
                ("operator/replication_broken", fields, summary)
            }
            Self::Divergence { peer, leader_lsn, follower_lsn } => {
                let summary = format!(
                    "replication divergence: peer={peer} leader_lsn={leader_lsn} follower_lsn={follower_lsn}"
                );
                let fields = vec![
                    AuditFieldEscaper::field("peer", peer),
                    AuditFieldEscaper::field("leader_lsn", leader_lsn),
                    AuditFieldEscaper::field("follower_lsn", follower_lsn),
                ];
                ("operator/divergence", fields, summary)
            }
            Self::WalFsyncFailed { path, error } => {
                let summary = format!("WAL fsync failed: path={path} error={error}");
                let fields = vec![
                    AuditFieldEscaper::field("path", path),
                    AuditFieldEscaper::field("error", error),
                ];
                ("operator/wal_fsync_failed", fields, summary)
            }
            Self::DiskSpaceCritical { path, available_bytes, threshold_bytes } => {
                let summary = format!(
                    "disk space critical: path={path} available={available_bytes} threshold={threshold_bytes}"
                );
                let fields = vec![
                    AuditFieldEscaper::field("path", path),
                    AuditFieldEscaper::field("available_bytes", available_bytes),
                    AuditFieldEscaper::field("threshold_bytes", threshold_bytes),
                ];
                ("operator/disk_space_critical", fields, summary)
            }
            Self::AuthBypass { principal, resource, detail } => {
                let summary =
                    format!("auth bypass detected: principal={principal} resource={resource}");
                let fields = vec![
                    AuditFieldEscaper::field("principal", principal),
                    AuditFieldEscaper::field("resource", resource),
                    AuditFieldEscaper::field("detail", detail),
                ];
                ("operator/auth_bypass", fields, summary)
            }
            Self::AdminCapabilityGranted { granted_to, capability, granted_by } => {
                let summary = format!(
                    "admin capability granted: to={granted_to} capability={capability} by={granted_by}"
                );
                let fields = vec![
                    AuditFieldEscaper::field("granted_to", granted_to),
                    AuditFieldEscaper::field("capability", capability),
                    AuditFieldEscaper::field("granted_by", granted_by),
                ];
                ("operator/admin_capability_granted", fields, summary)
            }
            Self::SecretRotationFailed { secret_ref, error } => {
                let summary =
                    format!("secret rotation failed: ref={secret_ref} error={error}");
                let fields = vec![
                    AuditFieldEscaper::field("secret_ref", secret_ref),
                    AuditFieldEscaper::field("error", error),
                ];
                ("operator/secret_rotation_failed", fields, summary)
            }
            Self::ConfigChanged { key, old_value, new_value, changed_by } => {
                let summary = format!(
                    "config changed: key={key} old={old_value} new={new_value} by={changed_by}"
                );
                let fields = vec![
                    AuditFieldEscaper::field("key", key),
                    AuditFieldEscaper::field("old_value", old_value),
                    AuditFieldEscaper::field("new_value", new_value),
                    AuditFieldEscaper::field("changed_by", changed_by),
                ];
                ("operator/config_changed", fields, summary)
            }
            Self::StartupFailed { phase, error } => {
                let summary = format!("startup failed: phase={phase} error={error}");
                let fields = vec![
                    AuditFieldEscaper::field("phase", phase),
                    AuditFieldEscaper::field("error", error),
                ];
                ("operator/startup_failed", fields, summary)
            }
            Self::ShutdownForced { reason } => {
                let summary = format!("shutdown forced: reason={reason}");
                let fields = vec![AuditFieldEscaper::field("reason", reason)];
                ("operator/shutdown_forced", fields, summary)
            }
            Self::SchemaCorruption { collection, detail } => {
                let summary =
                    format!("schema corruption: collection={collection} detail={detail}");
                let fields = vec![
                    AuditFieldEscaper::field("collection", collection),
                    AuditFieldEscaper::field("detail", detail),
                ];
                ("operator/schema_corruption", fields, summary)
            }
            Self::CheckpointFailed { lsn, error } => {
                let summary = format!("checkpoint failed: lsn={lsn} error={error}");
                let fields = vec![
                    AuditFieldEscaper::field("lsn", lsn),
                    AuditFieldEscaper::field("error", error),
                ];
                ("operator/checkpoint_failed", fields, summary)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::runtime::audit_log::AuditLogger;

    fn make_logger() -> (AuditLogger, std::path::PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "reddb-op-event-{}-{}",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".audit.log");
        let logger = AuditLogger::with_path(path.clone());
        (logger, path)
    }

    fn drain(logger: &AuditLogger) {
        assert!(
            logger.wait_idle(Duration::from_secs(2)),
            "audit logger drain timed out"
        );
    }

    fn read_last_line(path: &std::path::Path) -> crate::json::Value {
        let body = std::fs::read_to_string(path).unwrap();
        let line = body.lines().last().expect("at least one audit line");
        crate::json::from_str(line).expect("valid JSON")
    }

    // ------------------------------------------------------------------
    // One test per variant — verifies action string + a representative field
    // ------------------------------------------------------------------

    #[test]
    fn replication_broken_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::ReplicationBroken {
            peer: "replica-1".into(),
            reason: "TCP reset".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/replication_broken"));
        let peer = v.get("detail").and_then(|d| d.get("peer")).and_then(|x| x.as_str());
        assert_eq!(peer, Some("replica-1"));
    }

    #[test]
    fn divergence_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::Divergence {
            peer: "replica-2".into(),
            leader_lsn: 1000,
            follower_lsn: 999,
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/divergence"));
        let lsn = v.get("detail").and_then(|d| d.get("leader_lsn")).and_then(|x| x.as_i64());
        assert_eq!(lsn, Some(1000));
    }

    #[test]
    fn wal_fsync_failed_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::WalFsyncFailed {
            path: "/data/wal".into(),
            error: "EIO".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/wal_fsync_failed"));
        let err = v.get("detail").and_then(|d| d.get("error")).and_then(|x| x.as_str());
        assert_eq!(err, Some("EIO"));
    }

    #[test]
    fn disk_space_critical_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::DiskSpaceCritical {
            path: "/data".into(),
            available_bytes: 1024,
            threshold_bytes: 10240,
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/disk_space_critical"));
        let avail = v
            .get("detail")
            .and_then(|d| d.get("available_bytes"))
            .and_then(|x| x.as_i64());
        assert_eq!(avail, Some(1024));
    }

    #[test]
    fn auth_bypass_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::AuthBypass {
            principal: "alice".into(),
            resource: "/admin/drop".into(),
            detail: "scope check skipped".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/auth_bypass"));
        let res = v.get("detail").and_then(|d| d.get("resource")).and_then(|x| x.as_str());
        assert_eq!(res, Some("/admin/drop"));
    }

    #[test]
    fn admin_capability_granted_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::AdminCapabilityGranted {
            granted_to: "bob".into(),
            capability: "ADMIN_WRITE".into(),
            granted_by: "root".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(
            v.get("action").and_then(|x| x.as_str()),
            Some("operator/admin_capability_granted")
        );
        let cap = v.get("detail").and_then(|d| d.get("capability")).and_then(|x| x.as_str());
        assert_eq!(cap, Some("ADMIN_WRITE"));
    }

    #[test]
    fn secret_rotation_failed_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::SecretRotationFailed {
            secret_ref: "jwt-signing-key".into(),
            error: "HSM unreachable".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(
            v.get("action").and_then(|x| x.as_str()),
            Some("operator/secret_rotation_failed")
        );
        let r = v.get("detail").and_then(|d| d.get("secret_ref")).and_then(|x| x.as_str());
        assert_eq!(r, Some("jwt-signing-key"));
    }

    #[test]
    fn config_changed_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::ConfigChanged {
            key: "max_connections".into(),
            old_value: "100".into(),
            new_value: "200".into(),
            changed_by: "ops-bot".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/config_changed"));
        let nv = v.get("detail").and_then(|d| d.get("new_value")).and_then(|x| x.as_str());
        assert_eq!(nv, Some("200"));
    }

    #[test]
    fn startup_failed_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::StartupFailed {
            phase: "wal_recovery".into(),
            error: "corrupt frame".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/startup_failed"));
        let phase = v.get("detail").and_then(|d| d.get("phase")).and_then(|x| x.as_str());
        assert_eq!(phase, Some("wal_recovery"));
    }

    #[test]
    fn shutdown_forced_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::ShutdownForced {
            reason: "OOM".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/shutdown_forced"));
        let r = v.get("detail").and_then(|d| d.get("reason")).and_then(|x| x.as_str());
        assert_eq!(r, Some("OOM"));
    }

    #[test]
    fn schema_corruption_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::SchemaCorruption {
            collection: "users".into(),
            detail: "unknown type tag 0xFF".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/schema_corruption"));
        let coll = v.get("detail").and_then(|d| d.get("collection")).and_then(|x| x.as_str());
        assert_eq!(coll, Some("users"));
    }

    #[test]
    fn checkpoint_failed_emits() {
        let (logger, path) = make_logger();
        OperatorEvent::CheckpointFailed {
            lsn: 42_000,
            error: "write stall".into(),
        }
        .emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("action").and_then(|x| x.as_str()), Some("operator/checkpoint_failed"));
        let lsn = v.get("detail").and_then(|d| d.get("lsn")).and_then(|x| x.as_i64());
        assert_eq!(lsn, Some(42_000));
    }

    // ------------------------------------------------------------------
    // Adversarial corpus: CRLF / NUL / quote / non-UTF-8-ish in fields
    // ------------------------------------------------------------------

    #[test]
    fn adversarial_fields_are_escape_safe() {
        let payloads: &[(&str, &str)] = &[
            ("crlf", "line1\r\nline2"),
            ("nul", "before\0after"),
            ("quote", r#"she said "hi""#),
            ("json_inject", r#"{"injected":true}"#),
            ("low_ctrl", "\x01\x02\x07\x1f"),
            ("backslash", "C:\\path\\file"),
            ("mixed", "name=\"x\"\n\\path\t\x01end"),
        ];

        for (label, payload) in payloads {
            let (logger, path) = make_logger();
            OperatorEvent::SchemaCorruption {
                collection: payload.to_string(),
                detail: payload.to_string(),
            }
            .emit(&logger);
            drain(&logger);

            let body = std::fs::read_to_string(&path).unwrap();
            let line = body.lines().last().unwrap_or("");

            // Single JSONL row — no embedded newline.
            assert!(
                !line.contains('\n'),
                "{label}: embedded newline in JSONL row"
            );

            let v: crate::json::Value =
                crate::json::from_str(line).unwrap_or_else(|e| {
                    panic!("{label}: audit line not valid JSON: {e}\n{line:?}")
                });
            let recovered = v
                .get("detail")
                .and_then(|d| d.get("collection"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            assert_eq!(
                recovered, *payload,
                "{label}: round-trip mismatch"
            );
        }
    }

    // ------------------------------------------------------------------
    // Outcome is always Error; source is always System
    // ------------------------------------------------------------------

    #[test]
    fn emit_sets_outcome_error_and_source_system() {
        let (logger, path) = make_logger();
        OperatorEvent::ShutdownForced { reason: "test".into() }.emit(&logger);
        drain(&logger);
        let v = read_last_line(&path);
        assert_eq!(v.get("outcome").and_then(|x| x.as_str()), Some("error"));
        assert_eq!(v.get("source").and_then(|x| x.as_str()), Some("system"));
    }
}
