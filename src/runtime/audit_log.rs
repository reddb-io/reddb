//! Append-only audit log for admin mutations (PLAN.md Phase 6.5).
//!
//! Every operator-triggered state change goes through `record()`,
//! which:
//!   1. Writes a JSON line to `<data>/.audit.log` (append + sync).
//!   2. Emits the same line via `tracing::info!(target: "audit", …)`
//!      so log shippers (Vector, Fluent Bit, Loki, CloudWatch
//!      Agent) ingest it without touching the on-disk file.
//!
//! The record shape is stable so external tooling can build
//! dashboards / alerts directly:
//!
//! ```json
//! {
//!   "ts": "2026-04-25T12:34:56Z",
//!   "ts_unix_ms": 1730000000000,
//!   "action": "admin/shutdown",
//!   "principal": "operator",
//!   "target": "instance",
//!   "result": "ok",
//!   "details": {"backup_uploaded": true, "duration_ms": 412}
//! }
//! ```
//!
//! Best-effort: a failed audit append is logged but never blocks
//! the underlying mutation. Losing one audit line is preferable to
//! refusing a graceful shutdown because `/data` ran out of inodes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Map, Value as JsonValue};

#[derive(Debug)]
pub struct AuditLogger {
    path: PathBuf,
    inner: Mutex<()>,
}

impl AuditLogger {
    /// Place the audit log next to the primary `.rdb` file so
    /// snapshot + restore flows can ship it together. `None` when
    /// the runtime is in-memory; callers should still emit to
    /// stdout in that case.
    pub fn for_data_path(data_path: &Path) -> Self {
        let parent = data_path.parent().unwrap_or_else(|| Path::new("."));
        let path = parent.join(".audit.log");
        Self {
            path,
            inner: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. `details` is an arbitrary JSON object —
    /// pass `JsonValue::Null` when there's nothing structured to
    /// add. Result is the human-facing outcome (`"ok"`, `"err: …"`).
    pub fn record(
        &self,
        action: &str,
        principal: &str,
        target: &str,
        result: &str,
        details: JsonValue,
    ) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut object = Map::new();
        object.insert("ts_unix_ms".to_string(), JsonValue::Number(now_ms as f64));
        object.insert(
            "ts".to_string(),
            JsonValue::String(format_iso8601(now_ms)),
        );
        object.insert("action".to_string(), JsonValue::String(action.to_string()));
        object.insert(
            "principal".to_string(),
            JsonValue::String(principal.to_string()),
        );
        object.insert("target".to_string(), JsonValue::String(target.to_string()));
        object.insert("result".to_string(), JsonValue::String(result.to_string()));
        if !matches!(details, JsonValue::Null) {
            object.insert("details".to_string(), details);
        }

        let line = JsonValue::Object(object).to_string_compact();

        // Stdout-side first — even if the disk write fails, the log
        // shipper still gets a record.
        tracing::info!(target: "reddb::audit", "{line}");

        // File side — append + sync. A failure is logged but not
        // propagated; we never want a full disk to block a
        // graceful_shutdown that's trying to free space.
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = self.append_line(&line) {
            tracing::warn!(
                target: "reddb::audit",
                error = %err,
                path = %self.path.display(),
                "audit log file append failed; stdout copy stands"
            );
        }
    }

    fn append_line(&self, line: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }
}

/// Tiny ISO-8601 formatter that doesn't pull in `chrono`. Outputs
/// `YYYY-MM-DDTHH:MM:SS.mmmZ`. Same `civil_from_days` logic the
/// S3 backend uses for SigV4 timestamps — we just expose ms.
fn format_iso8601(ms_since_epoch: u64) -> String {
    let secs = ms_since_epoch / 1000;
    let ms = ms_since_epoch % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, mi, s, ms
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "reddb-audit-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.push("data.rdb");
        p
    }

    #[test]
    fn record_writes_one_line_per_call() {
        let data = temp_data_path("one-line");
        let logger = AuditLogger::for_data_path(&data);
        logger.record(
            "admin/readonly",
            "operator",
            "instance",
            "ok",
            JsonValue::Null,
        );
        let body = std::fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"action\":\"admin/readonly\""));
        assert!(lines[0].contains("\"result\":\"ok\""));
    }

    #[test]
    fn record_appends_across_calls() {
        let data = temp_data_path("append");
        let logger = AuditLogger::for_data_path(&data);
        logger.record("admin/drain", "op", "instance", "ok", JsonValue::Null);
        logger.record("admin/shutdown", "op", "instance", "ok", JsonValue::Null);
        let lines = std::fs::read_to_string(logger.path()).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn record_includes_details_object_when_present() {
        let data = temp_data_path("details");
        let logger = AuditLogger::for_data_path(&data);
        let mut object = Map::new();
        object.insert("ms".to_string(), JsonValue::Number(412.0));
        logger.record(
            "admin/shutdown",
            "operator",
            "instance",
            "ok",
            JsonValue::Object(object),
        );
        let body = std::fs::read_to_string(logger.path()).unwrap();
        assert!(body.contains("\"details\":{"));
        assert!(body.contains("\"ms\":412"));
    }

    #[test]
    fn iso8601_formats_known_epoch() {
        // 2024-02-29 12:34:56.789 UTC.
        // 19782 days since 1970-01-01 + 12*3600 + 34*60 + 56 = 1709210096
        assert_eq!(format_iso8601(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
    }
}
