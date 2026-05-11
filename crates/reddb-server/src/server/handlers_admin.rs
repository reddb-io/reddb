//! Lifecycle / admin HTTP endpoints (PLAN.md Phase 1).
//!
//! Universal contract surface consumed by orchestrators (K8s preStop,
//! Fly autostop, ECS drain, systemd, custom).
//!
//! - `POST /admin/shutdown` — flush + checkpoint + optional backup,
//!   200 only when safe to die. Idempotent.
//! - `POST /admin/drain` — stop accepting new writes, in-flight finish,
//!   200 once drain complete. Soft pre-shutdown step.
//! - `GET  /health/live` — process responsive (always cheap).
//! - `GET  /health/ready` — accepts queries (WAL replay + restore done).
//! - `GET  /health/startup` — same logic as ready, K8s-style longer
//!   timeout window.

use super::*;
use crate::runtime::lifecycle::Phase;
use std::path::{Path, PathBuf};

/// Path to the persistent runtime-toggle file kept beside the
/// `.rdb` data file. Operators can prep a fresh deploy by writing
/// `{"read_only": true}` before first boot to come up locked.
pub(crate) fn runtime_state_path(data_path: &Path) -> PathBuf {
    let parent = data_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(".runtime-state.json")
}

/// Atomically persist the read_only toggle. Writes to a sibling
/// `.tmp` file then renames to defeat torn writes — same pattern
/// the snapshot publish path uses.
pub(crate) fn persist_runtime_readonly(state_path: &Path, enabled: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut object = crate::json::Map::new();
    object.insert("read_only".to_string(), crate::json::Value::Bool(enabled));
    let body = crate::serde_json::to_string_pretty(&crate::json::Value::Object(object))
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    if let Some(parent) = state_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = state_path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, state_path)?;
    Ok(())
}

/// Read a previously-persisted read_only toggle. Returns `None`
/// when the file doesn't exist or doesn't parse — boot continues
/// from the env-var / RedDBOptions value in that case.
pub fn load_runtime_readonly(data_path: &Path) -> Option<bool> {
    let state_path = runtime_state_path(data_path);
    let bytes = std::fs::read(&state_path).ok()?;
    let parsed: crate::json::Value = crate::json::from_slice(&bytes).ok()?;
    parsed.get("read_only").and_then(|v| v.as_bool())
}

/// PLAN.md Phase 11.6 — default lease holder id when the operator
/// doesn't pin one in the promotion request body. Mirrors the boot
/// loop's resolution (`RED_LEASE_HOLDER_ID` → `<hostname>-<pid>`).
fn default_holder_id() -> String {
    if let Some(explicit) = crate::utils::env_with_file_fallback("RED_LEASE_HOLDER_ID") {
        return explicit;
    }
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown-host".to_string());
    format!("{host}-{}", std::process::id())
}

/// Sanitize replica IDs for use as Prometheus label values.
/// Replaces double quotes, backslashes, and newlines so the resulting
/// metric line stays parseable. Operators picking aggressive replica
/// IDs is rare but malicious input must not break /metrics.
fn sanitize_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

/// Standard base64 decode (RFC 4648 §4, alphabet `A-Za-z0-9+/`).
/// Returns `Err` on any invalid character; padding `=` is optional.
fn b64_decode(input: &str) -> Result<Vec<u8>, String> {
    let input = input.trim_end_matches('=');
    let mut buf = Vec::with_capacity(input.len() * 3 / 4 + 1);

    let lookup = |c: u8| -> Result<u32, String> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            other => Err(format!("invalid base64 character: {}", other as char)),
        }
    };

    let bytes: Vec<u8> = input.bytes().collect();
    for chunk in bytes.chunks(4) {
        let v: Vec<u32> = chunk.iter().map(|&b| lookup(b)).collect::<Result<_, _>>()?;
        match v.len() {
            4 => {
                let n = (v[0] << 18) | (v[1] << 12) | (v[2] << 6) | v[3];
                buf.push((n >> 16) as u8);
                buf.push((n >> 8) as u8);
                buf.push(n as u8);
            }
            3 => {
                let n = (v[0] << 18) | (v[1] << 12) | (v[2] << 6);
                buf.push((n >> 16) as u8);
                buf.push((n >> 8) as u8);
            }
            2 => {
                let n = (v[0] << 18) | (v[1] << 12);
                buf.push((n >> 16) as u8);
            }
            _ => {}
        }
    }
    Ok(buf)
}

/// Reject CR, LF, and NUL bytes in caller-controlled strings that flow into
/// audit logs and response envelopes (ADR 0010).
fn reject_smuggling_bytes(field: &str, value: &str) -> Option<HttpResponse> {
    for (idx, byte) in value.as_bytes().iter().enumerate() {
        match *byte {
            b'\0' => {
                return Some(json_error(
                    400,
                    format!("field `{field}` contains forbidden NUL byte at index {idx}"),
                ));
            }
            b'\r' | b'\n' => {
                return Some(json_error(
                    400,
                    format!("field `{field}` contains forbidden CR/LF byte at index {idx}"),
                ));
            }
            _ => {}
        }
    }
    None
}

impl RedDBServer {
    /// `POST /admin/shutdown` — graceful shutdown coordinator.
    /// Returns 200 with the shutdown report when complete; 200 with
    /// the cached report when already shut down (idempotent); 500
    /// on flush failure (process should still exit afterwards).
    ///
    /// The HTTP layer does not own process exit — that's the
    /// signal-handler / `run_server` driver. This handler reports
    /// state; orchestrators that posted SIGTERM separately will see
    /// the process die when their grace window elapses.
    pub(crate) fn handle_admin_shutdown(&self) -> HttpResponse {
        let backup_on_shutdown = std::env::var("RED_BACKUP_ON_SHUTDOWN")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(true);

        match self.runtime.graceful_shutdown(backup_on_shutdown) {
            Ok(report) => {
                // PLAN.md Phase 6.5 — audit operator-triggered
                // shutdown. Recorded as "ok" + duration so the log
                // shipper can graph shutdown latency over time.
                let mut details = Map::new();
                details.insert(
                    "backup_uploaded".to_string(),
                    JsonValue::Bool(report.backup_uploaded),
                );
                details.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(report.duration_ms as f64),
                );
                self.runtime.audit_log().record(
                    "admin/shutdown",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "phase".to_string(),
                    JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
                );
                object.insert(
                    "flushed_wal".to_string(),
                    JsonValue::Bool(report.flushed_wal),
                );
                object.insert(
                    "final_checkpoint".to_string(),
                    JsonValue::Bool(report.final_checkpoint),
                );
                object.insert(
                    "backup_uploaded".to_string(),
                    JsonValue::Bool(report.backup_uploaded),
                );
                object.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(report.duration_ms as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(500, err.to_string()),
        }
    }

    /// `POST /admin/restore` — operator-triggered restore from the
    /// configured remote backend (PLAN.md Phase 3.2). Refuses unless
    /// the runtime is read_only / replica so live writes can't race
    /// the swap. Body fields are optional:
    /// `{"to_lsn": u64, "to_timestamp_ms": u64, "snapshot_id": str}`.
    /// Empty body restores to latest.
    pub(crate) fn handle_admin_restore(&self, body: Vec<u8>) -> HttpResponse {
        if !self.runtime.write_gate().is_read_only() {
            return json_error(
                409,
                "POST /admin/restore requires the runtime to be read_only or replica-role; \
                 toggle via RED_READONLY=true or POST /admin/readonly first",
            );
        }
        let db = self.runtime.db();
        let Some(backend) = db.options().remote_backend.clone() else {
            return json_error(412, "no remote backend configured (RED_BACKEND=none)");
        };
        let Some(local_path) = db.path().map(|p| p.to_path_buf()) else {
            return json_error(412, "in-memory runtime cannot be restored from remote");
        };
        let snapshot_prefix = db.options().default_snapshot_prefix();
        let wal_prefix = db.options().default_wal_archive_prefix();
        let target_time_ms = if body.is_empty() {
            0u64
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => v
                    .get("to_timestamp_ms")
                    .and_then(|n| n.as_u64())
                    .or_else(|| {
                        v.get("to_timestamp")
                            .and_then(|n| n.as_u64())
                            .map(|s| s.saturating_mul(1000))
                    })
                    .unwrap_or(0),
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };
        let recovery =
            crate::storage::wal::PointInTimeRecovery::new(backend, snapshot_prefix, wal_prefix);
        match recovery.restore_to(target_time_ms, &local_path) {
            Ok(report) => {
                let mut details = Map::new();
                details.insert(
                    "snapshot_used".to_string(),
                    JsonValue::Number(report.snapshot_used as f64),
                );
                details.insert(
                    "wal_segments_replayed".to_string(),
                    JsonValue::Number(report.wal_segments_replayed as f64),
                );
                details.insert(
                    "records_applied".to_string(),
                    JsonValue::Number(report.records_applied as f64),
                );
                details.insert(
                    "recovered_to_lsn".to_string(),
                    JsonValue::Number(report.recovered_to_lsn as f64),
                );
                details.insert(
                    "recovered_to_time".to_string(),
                    JsonValue::Number(report.recovered_to_time as f64),
                );
                self.runtime.audit_log().record(
                    "admin/restore",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details.clone()),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                for (k, v) in details {
                    object.insert(k, v);
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => {
                self.runtime.audit_log().record(
                    "admin/restore",
                    "operator",
                    "instance",
                    &format!("err: {err}"),
                    JsonValue::Null,
                );
                json_error(500, err.to_string())
            }
        }
    }

    /// `POST /admin/backup` — operator-triggered backup, alias of
    /// `/backup/trigger` placed under the universal `/admin/*`
    /// namespace per PLAN.md Phase 3.3.
    pub(crate) fn handle_admin_backup(
        &self,
        _query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        match self.runtime.trigger_backup() {
            Ok(result) => {
                let mut details = Map::new();
                details.insert(
                    "snapshot_id".to_string(),
                    JsonValue::Number(result.snapshot_id as f64),
                );
                details.insert("uploaded".to_string(), JsonValue::Bool(result.uploaded));
                details.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(result.duration_ms as f64),
                );
                self.runtime.audit_log().record(
                    "admin/backup",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details.clone()),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                for (k, v) in details {
                    object.insert(k, v);
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => {
                self.runtime.audit_log().record(
                    "admin/backup",
                    "operator",
                    "instance",
                    &format!("err: {err}"),
                    JsonValue::Null,
                );
                json_error(500, err.to_string())
            }
        }
    }

    /// `POST /admin/blob_cache/sweep` — bounded sweep of expired L1
    /// entries on the runtime result Blob Cache (issue #148 follow-up,
    /// closing the deferred half wired by sweeper.rs flag #4).
    ///
    /// Body (JSON, all fields optional):
    ///
    /// ```json
    /// { "limit_entries": 1000, "limit_millis": 100 }
    /// ```
    ///
    /// - Both null / missing → unbounded sweep (`SweepLimit::Either`
    ///   with `usize::MAX` / `u32::MAX` so the sweeper still has the
    ///   single composite-bound code path).
    /// - One field set → `SweepLimit::Entries` or `SweepLimit::Millis`.
    /// - Both set → `SweepLimit::Either { entries, millis }` (first
    ///   bound to fire wins).
    ///
    /// Returns the [`SweepReport`](crate::storage::cache::sweeper::SweepReport)
    /// fields plus `ok:true`. Caller-influenced strings (none today —
    /// the report holds only numeric fields) would round-trip through
    /// `SerializedJsonField::tainted` per ADR 0010 §3.
    pub(crate) fn handle_admin_blob_cache_sweep(&self, body: Vec<u8>) -> HttpResponse {
        use crate::storage::cache::sweeper::{BlobCacheSweeper, SweepLimit};

        let (limit_entries, limit_millis) = if body.is_empty() {
            (None, None)
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => {
                    let entries = v
                        .get("limit_entries")
                        .and_then(|n| n.as_u64())
                        .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
                    let millis = v
                        .get("limit_millis")
                        .and_then(|n| n.as_u64())
                        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
                    (entries, millis)
                }
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };

        let limit = match (limit_entries, limit_millis) {
            (None, None) => SweepLimit::Either {
                entries: usize::MAX,
                millis: u32::MAX,
            },
            (Some(e), None) => SweepLimit::Entries(e),
            (None, Some(m)) => SweepLimit::Millis(m),
            (Some(e), Some(m)) => SweepLimit::Either {
                entries: e,
                millis: m,
            },
        };

        let report = BlobCacheSweeper::sweep_expired(self.runtime.result_blob_cache(), limit);

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "entries_scanned".to_string(),
            JsonValue::Number(report.entries_scanned as f64),
        );
        object.insert(
            "entries_evicted".to_string(),
            JsonValue::Number(report.entries_evicted as f64),
        );
        object.insert(
            "bytes_reclaimed".to_string(),
            JsonValue::Number(report.bytes_reclaimed as f64),
        );
        object.insert(
            "elapsed_ms".to_string(),
            JsonValue::Number(report.elapsed_ms as f64),
        );
        object.insert(
            "truncated_due_to_limit".to_string(),
            JsonValue::Bool(report.truncated_due_to_limit),
        );

        // Audit. Operator-driven sweep is rare; logging it gives the
        // log shipper a primary-key on which to graph cache-sweep cadence.
        let mut details = Map::new();
        details.insert(
            "entries_evicted".to_string(),
            JsonValue::Number(report.entries_evicted as f64),
        );
        details.insert(
            "bytes_reclaimed".to_string(),
            JsonValue::Number(report.bytes_reclaimed as f64),
        );
        details.insert(
            "elapsed_ms".to_string(),
            JsonValue::Number(report.elapsed_ms as f64),
        );
        self.runtime.audit_log().record(
            "admin/blob_cache/sweep",
            "operator",
            "instance",
            "ok",
            JsonValue::Object(details),
        );

        json_response(200, JsonValue::Object(object))
    }

    /// `POST /admin/blob_cache/flush_namespace` — foreground-fast
    /// namespace flush on the runtime result Blob Cache (issue #148
    /// follow-up, closing sweeper.rs flag #4).
    ///
    /// Body (JSON):
    ///
    /// ```json
    /// { "namespace": "tenant-42:results" }
    /// ```
    ///
    /// Validation contract:
    ///
    /// - `namespace` is **required**, **non-empty**, and must contain
    ///   no NUL or CR/LF bytes — the same constraints
    ///   [`crate::server::header_escape_guard::HeaderEscapeGuard`]
    ///   enforces on response headers, applied here on the request side
    ///   so a CRLF-laden namespace cannot smuggle audit-log lines or
    ///   sneak past the JSON-envelope guard. Reflected back into the
    ///   response through
    ///   [`crate::json_field::SerializedJsonField::tainted`] per ADR 0010 §3.
    ///
    /// Returns the [`NamespaceFlushReport`](crate::storage::cache::sweeper::NamespaceFlushReport)
    /// fields plus `ok:true`.
    pub(crate) fn handle_admin_blob_cache_flush_namespace(&self, body: Vec<u8>) -> HttpResponse {
        use crate::storage::cache::sweeper::BlobCacheSweeper;

        if body.is_empty() {
            return json_error(400, "missing JSON body with required `namespace` field");
        }
        let parsed: crate::serde_json::Value = match crate::serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
        };
        let namespace = match parsed.get("namespace").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return json_error(400, "field `namespace` is required and must be a string"),
        };
        if namespace.is_empty() {
            return json_error(400, "field `namespace` must not be empty");
        }
        // Adversarial-byte rejection. The namespace string is
        // caller-controlled and may end up in audit logs, dashboards,
        // and the response envelope. Reject CR/LF/NUL on the request
        // side rather than relying on every downstream sink to escape.
        for (idx, byte) in namespace.as_bytes().iter().enumerate() {
            match *byte {
                b'\0' => {
                    return json_error(
                        400,
                        format!("field `namespace` contains forbidden NUL byte at index {idx}"),
                    );
                }
                b'\r' | b'\n' => {
                    return json_error(
                        400,
                        format!("field `namespace` contains forbidden CR/LF byte at index {idx}"),
                    );
                }
                _ => {}
            }
        }

        let report =
            BlobCacheSweeper::flush_namespace(self.runtime.result_blob_cache(), &namespace);

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        // Reflect the caller-supplied namespace back through the
        // JSON-boundary guard so any high-bit / Unicode bytes round-trip
        // through canonical RFC-8259 escaping.
        object.insert(
            "namespace".to_string(),
            crate::json_field::SerializedJsonField::tainted(&report.namespace),
        );
        object.insert(
            "generation_before".to_string(),
            JsonValue::Number(report.generation_before as f64),
        );
        object.insert(
            "generation_after".to_string(),
            JsonValue::Number(report.generation_after as f64),
        );
        object.insert(
            "elapsed_micros".to_string(),
            JsonValue::Number(report.elapsed_micros as f64),
        );

        let mut details = Map::new();
        details.insert(
            "namespace".to_string(),
            crate::json_field::SerializedJsonField::tainted(&report.namespace),
        );
        details.insert(
            "elapsed_micros".to_string(),
            JsonValue::Number(report.elapsed_micros as f64),
        );
        self.runtime.audit_log().record(
            "admin/blob_cache/flush_namespace",
            "operator",
            "instance",
            "ok",
            JsonValue::Object(details),
        );

        json_response(200, JsonValue::Object(object))
    }

    /// `POST /admin/cache/compare-and-set` — optimistic-lock put on the
    /// runtime result Blob Cache (issue #195).
    ///
    /// Body (JSON):
    ///
    /// ```json
    /// {
    ///   "namespace":      "tenant-42:results",
    ///   "key":            "query-abc",
    ///   "expected_version": 3,
    ///   "new_value_b64":  "<standard base64>",
    ///   "new_version":    4,
    ///   "ttl_ms":         60000
    /// }
    /// ```
    ///
    /// `ttl_ms` is optional. `expected_version` is informational —
    /// the atomic guard comes from `BlobCache::put`'s internal
    /// `check_version`: if the stored version ≥ `new_version` the
    /// write is rejected with 409.
    ///
    /// Returns:
    /// - 200 `{ committed: true, current_version }`
    /// - 409 `{ committed: false, current_version, reason: "VersionMismatch" }`
    /// - 400 malformed body / CRLF/NUL injection / bad base64
    /// - 401 missing or wrong admin bearer token
    pub(crate) fn handle_admin_blob_cache_compare_and_set(&self, body: Vec<u8>) -> HttpResponse {
        use crate::storage::cache::blob::{BlobCachePolicy, BlobCachePut, CacheError};

        if body.is_empty() {
            return json_error(400, "missing JSON body");
        }
        let parsed: crate::serde_json::Value = match crate::serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
        };

        let namespace = match parsed.get("namespace").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            Some(_) => return json_error(400, "field `namespace` must not be empty"),
            None => return json_error(400, "field `namespace` is required and must be a string"),
        };
        let key = match parsed.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k.to_string(),
            Some(_) => return json_error(400, "field `key` must not be empty"),
            None => return json_error(400, "field `key` is required and must be a string"),
        };
        let new_value_b64 = match parsed.get("new_value_b64").and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => {
                return json_error(
                    400,
                    "field `new_value_b64` is required and must be a string",
                )
            }
        };
        let new_version = match parsed.get("new_version").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => {
                return json_error(
                    400,
                    "field `new_version` is required and must be a non-negative integer",
                )
            }
        };
        // expected_version is optional and informational; validate type if present.
        if let Some(ev) = parsed.get("expected_version") {
            if ev.as_u64().is_none() {
                return json_error(
                    400,
                    "field `expected_version` must be a non-negative integer",
                );
            }
        }
        let ttl_ms = parsed.get("ttl_ms").and_then(|v| v.as_u64());

        // Adversarial-byte rejection on caller-controlled strings.
        if let Some(err) = reject_smuggling_bytes("namespace", &namespace) {
            return err;
        }
        if let Some(err) = reject_smuggling_bytes("key", &key) {
            return err;
        }

        // Base64 decode the payload.
        let bytes = match b64_decode(&new_value_b64) {
            Ok(b) => b,
            Err(e) => return json_error(400, format!("invalid base64 in `new_value_b64`: {e}")),
        };

        // Build and execute the versioned put.
        let policy = if let Some(ttl) = ttl_ms {
            BlobCachePolicy::default().version(new_version).ttl_ms(ttl)
        } else {
            BlobCachePolicy::default().version(new_version)
        };
        let put = BlobCachePut::new(bytes).with_policy(policy);

        match self.runtime.result_blob_cache().put(&namespace, &key, put) {
            Ok(()) => {
                let mut obj = Map::new();
                obj.insert("committed".to_string(), JsonValue::Bool(true));
                obj.insert(
                    "current_version".to_string(),
                    JsonValue::Number(new_version as f64),
                );

                let mut details = Map::new();
                details.insert(
                    "namespace".to_string(),
                    crate::json_field::SerializedJsonField::tainted(&namespace),
                );
                details.insert(
                    "key".to_string(),
                    crate::json_field::SerializedJsonField::tainted(&key),
                );
                details.insert(
                    "new_version".to_string(),
                    JsonValue::Number(new_version as f64),
                );
                self.runtime.audit_log().record(
                    "admin/cache/compare_and_set",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details),
                );

                json_response(200, JsonValue::Object(obj))
            }
            Err(CacheError::VersionMismatch { existing, .. }) => {
                let mut obj = Map::new();
                obj.insert("committed".to_string(), JsonValue::Bool(false));
                obj.insert(
                    "current_version".to_string(),
                    JsonValue::Number(existing as f64),
                );
                obj.insert(
                    "reason".to_string(),
                    JsonValue::String("VersionMismatch".to_string()),
                );
                json_response(409, JsonValue::Object(obj))
            }
            Err(err) => json_error(500, format!("cache put failed: {err:?}")),
        }
    }

    /// `GET /admin/blob_cache/stats` — blob cache telemetry snapshot.
    ///
    /// Returns a JSON object with the global [`BlobCacheStats`] counters.
    /// Optional query parameter `namespace` is reserved for future
    /// per-namespace filtering; the current implementation ignores it
    /// and always returns instance-global counters.
    pub(crate) fn handle_admin_blob_cache_stats(
        &self,
        _query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        let s = self.runtime.result_blob_cache().stats();
        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("hits".to_string(), JsonValue::Number(s.hits() as f64));
        obj.insert("misses".to_string(), JsonValue::Number(s.misses() as f64));
        obj.insert(
            "insertions".to_string(),
            JsonValue::Number(s.insertions() as f64),
        );
        obj.insert(
            "evictions".to_string(),
            JsonValue::Number(s.evictions() as f64),
        );
        obj.insert(
            "expirations".to_string(),
            JsonValue::Number(s.expirations() as f64),
        );
        obj.insert(
            "invalidations".to_string(),
            JsonValue::Number(s.invalidations() as f64),
        );
        obj.insert(
            "namespace_flushes".to_string(),
            JsonValue::Number(s.namespace_flushes() as f64),
        );
        obj.insert(
            "version_mismatches".to_string(),
            JsonValue::Number(s.version_mismatches() as f64),
        );
        obj.insert("entries".to_string(), JsonValue::Number(s.entries() as f64));
        obj.insert(
            "bytes_in_use".to_string(),
            JsonValue::Number(s.bytes_in_use() as f64),
        );
        obj.insert(
            "l1_bytes_max".to_string(),
            JsonValue::Number(s.l1_bytes_max() as f64),
        );
        obj.insert(
            "l2_bytes_in_use".to_string(),
            JsonValue::Number(s.l2_bytes_in_use() as f64),
        );
        obj.insert(
            "l2_bytes_max".to_string(),
            JsonValue::Number(s.l2_bytes_max() as f64),
        );
        obj.insert(
            "l2_full_rejections".to_string(),
            JsonValue::Number(s.l2_full_rejections() as f64),
        );
        obj.insert(
            "l2_metadata_reads".to_string(),
            JsonValue::Number(s.l2_metadata_reads() as f64),
        );
        obj.insert(
            "l2_negative_skips".to_string(),
            JsonValue::Number(s.l2_negative_skips() as f64),
        );
        obj.insert(
            "synopsis_metadata_reads".to_string(),
            JsonValue::Number(s.synopsis_metadata_reads() as f64),
        );
        obj.insert(
            "synopsis_bytes".to_string(),
            JsonValue::Number(s.synopsis_bytes() as f64),
        );
        obj.insert(
            "namespaces".to_string(),
            JsonValue::Number(s.namespaces() as f64),
        );
        obj.insert(
            "max_namespaces".to_string(),
            JsonValue::Number(s.max_namespaces() as f64),
        );
        obj.insert(
            "promotion_queued".to_string(),
            JsonValue::Number(s.promotion_queued() as f64),
        );
        obj.insert(
            "promotion_dropped".to_string(),
            JsonValue::Number(s.promotion_dropped() as f64),
        );
        obj.insert(
            "promotion_completed".to_string(),
            JsonValue::Number(s.promotion_completed() as f64),
        );
        obj.insert(
            "promotion_queue_depth".to_string(),
            JsonValue::Number(s.promotion_queue_depth() as f64),
        );
        obj.insert(
            "l2_compression_ratio_observed".to_string(),
            JsonValue::Number(s.l2_compression_ratio_observed()),
        );
        obj.insert(
            "l2_compression_skipped_total".to_string(),
            JsonValue::Number(s.l2_compression_skipped_total() as f64),
        );
        obj.insert(
            "l2_bytes_saved_total".to_string(),
            JsonValue::Number(s.l2_bytes_saved_total() as f64),
        );
        json_response(200, JsonValue::Object(obj))
    }

    /// `POST /admin/readonly` — flip the public-mutation gate
    /// (PLAN.md Phase 4.3).
    ///
    /// Body: `{"enabled": true|false}`. Returns the new state. Useful
    /// for orchestrators that need to suspend writes (maintenance,
    /// billing suspension, hot key rotation) without killing the
    /// process or detaching the volume. Replicas reject writes
    /// regardless of this flag — the replication-role gate fires
    /// first.
    ///
    /// Persistence: the new state is written to
    /// `<data_dir>/.runtime-state.json` so a subsequent restart
    /// re-applies it. Failure to persist returns 500 — the in-memory
    /// flag is reverted so caller and disk stay consistent.
    pub(crate) fn handle_admin_readonly(&self, body: Vec<u8>) -> HttpResponse {
        let enabled = if body.is_empty() {
            true
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => v.get("enabled").and_then(|n| n.as_bool()).unwrap_or(true),
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };

        let previous = self.runtime.write_gate().set_read_only(enabled);

        // Persist the toggle so a subsequent restart re-applies it
        // before any client surface comes online. Best-effort: on
        // failure we revert the in-memory flag so disk and runtime
        // agree (operator can then re-issue once the storage issue
        // is resolved).
        if let Some(data_path) = self.runtime.db().path() {
            let state_path = runtime_state_path(data_path);
            if let Err(err) = persist_runtime_readonly(&state_path, enabled) {
                self.runtime.write_gate().set_read_only(previous);
                return json_error(
                    500,
                    format!("read_only persisted to {state_path:?} failed: {err}"),
                );
            }
        }

        let mut details = Map::new();
        details.insert("enabled".to_string(), JsonValue::Bool(enabled));
        details.insert("previous".to_string(), JsonValue::Bool(previous));
        self.runtime.audit_log().record(
            "admin/readonly",
            "operator",
            "instance",
            "ok",
            JsonValue::Object(details),
        );
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("read_only".to_string(), JsonValue::Bool(enabled));
        object.insert("previous".to_string(), JsonValue::Bool(previous));
        json_response(200, JsonValue::Object(object))
    }

    /// `GET /metrics` — Prometheus / OpenMetrics exposition.
    ///
    /// Initial metric set (PLAN.md Phase 5.1) covers the
    /// orchestrator-relevant signals: uptime, health phase, read-
    /// only state, replication role, last-backup outcome, on-disk
    /// size when known. Counters that need request-path
    /// instrumentation (ops_total, query_duration_seconds_bucket)
    /// land in a follow-up commit so this endpoint can ship today
    /// against the existing data sources.
    pub(crate) fn handle_metrics(&self) -> HttpResponse {
        use std::fmt::Write;
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let uptime_secs = (now_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0;
        let cold_start_secs = lifecycle
            .ready_at_ms()
            .map(|ready_ms| (ready_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0);
        let health_status: u8 = match phase {
            Phase::Stopped => 0,
            Phase::Starting | Phase::ShuttingDown => 0,
            Phase::Draining => 1,
            Phase::Ready => 2,
        };
        let read_only = self.runtime.write_gate().is_read_only();
        let role = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => "standalone",
            crate::replication::ReplicationRole::Primary => "primary",
            crate::replication::ReplicationRole::Replica { .. } => "replica",
        };
        let db_size_bytes = self
            .runtime
            .db()
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let runtime_stats = self.runtime.stats();
        let result_blob_stats = runtime_stats.result_blob_cache;
        let kv_stats = runtime_stats.kv;

        let mut body = String::with_capacity(1024);
        let _ = writeln!(
            body,
            "# HELP reddb_uptime_seconds Seconds since the runtime was constructed."
        );
        let _ = writeln!(body, "# TYPE reddb_uptime_seconds gauge");
        let _ = writeln!(body, "reddb_uptime_seconds {}", uptime_secs);

        let _ = writeln!(
            body,
            "# HELP reddb_health_status 0=down/starting, 1=degraded/draining, 2=ready."
        );
        let _ = writeln!(body, "# TYPE reddb_health_status gauge");
        let _ = writeln!(body, "reddb_health_status {}", health_status);

        let _ = writeln!(
            body,
            "# HELP reddb_phase Lifecycle phase as a labeled gauge (always 1; phase in label)."
        );
        let _ = writeln!(body, "# TYPE reddb_phase gauge");
        let _ = writeln!(body, "reddb_phase{{phase=\"{}\"}} 1", phase.as_str());

        let _ = writeln!(
            body,
            "# HELP reddb_read_only 1 when public mutations are gated, 0 otherwise."
        );
        let _ = writeln!(body, "# TYPE reddb_read_only gauge");
        let _ = writeln!(body, "reddb_read_only {}", if read_only { 1 } else { 0 });

        let _ = writeln!(
            body,
            "# HELP reddb_replication_role Replication role of this instance."
        );
        let _ = writeln!(body, "# TYPE reddb_replication_role gauge");
        let _ = writeln!(body, "reddb_replication_role{{role=\"{}\"}} 1", role);

        // PLAN.md Phase 5 / W6 — serverless writer lease state.
        // `not_required` for instances that opted out of lease fencing;
        // `held` / `not_held` for instances behind the fence so dashboards
        // can alert on lease loss without scraping logs.
        let lease_state = self.runtime.write_gate().lease_state();
        let _ = writeln!(
            body,
            "# HELP reddb_writer_lease_state Serverless writer-lease gate state (label)."
        );
        let _ = writeln!(body, "# TYPE reddb_writer_lease_state gauge");
        let _ = writeln!(
            body,
            "reddb_writer_lease_state{{state=\"{}\"}} 1",
            lease_state.label()
        );

        // PLAN.md Phase 5.1 — backup + WAL archive lag.
        // These are the SRE signals an orchestrator alerts on when a
        // serverless instance is healthy on the surface but its DR
        // posture has degraded silently.
        let backup_status = self.runtime.backup_status();
        if let Some(last) = backup_status.last_backup.as_ref() {
            let last_ts_secs = (last.timestamp as f64) / 1000.0;
            let _ = writeln!(
                body,
                "# HELP reddb_backup_last_success_timestamp_seconds Unix ts (s) of the most recent successful backup."
            );
            let _ = writeln!(
                body,
                "# TYPE reddb_backup_last_success_timestamp_seconds gauge"
            );
            let _ = writeln!(
                body,
                "reddb_backup_last_success_timestamp_seconds {}",
                last_ts_secs
            );
            let age_secs = ((now_ms.saturating_sub(last.timestamp)) as f64) / 1000.0;
            let _ = writeln!(
                body,
                "# HELP reddb_backup_age_seconds Seconds since last successful backup."
            );
            let _ = writeln!(body, "# TYPE reddb_backup_age_seconds gauge");
            let _ = writeln!(body, "reddb_backup_age_seconds {}", age_secs);
            let _ = writeln!(
                body,
                "# HELP reddb_backup_last_duration_seconds Wall-clock duration of the most recent backup."
            );
            let _ = writeln!(body, "# TYPE reddb_backup_last_duration_seconds gauge");
            let _ = writeln!(
                body,
                "reddb_backup_last_duration_seconds {}",
                (last.duration_ms as f64) / 1000.0
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_backup_failures_total Total backup failures since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_backup_failures_total counter");
        let _ = writeln!(
            body,
            "reddb_backup_failures_total {}",
            backup_status.total_failures
        );
        let _ = writeln!(
            body,
            "# HELP reddb_backup_total_total Total successful backups since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_backup_total_total counter");
        let _ = writeln!(
            body,
            "reddb_backup_total_total {}",
            backup_status.total_backups
        );

        // WAL archive lag — distance between the engine's current LSN
        // and the last archived LSN. Operators alert when this grows
        // unbounded; it means archive uploads are failing or paused
        // (e.g. backend unreachable, lease lost).
        let (current_lsn, last_archived_lsn) = self.runtime.wal_archive_progress();
        let lag = current_lsn.saturating_sub(last_archived_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_current_lsn Current local LSN (most recent record visible to writers)."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_current_lsn gauge");
        let _ = writeln!(body, "reddb_wal_current_lsn {}", current_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_last_archived_lsn LSN of the most recently archived WAL segment."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_last_archived_lsn gauge");
        let _ = writeln!(body, "reddb_wal_last_archived_lsn {}", last_archived_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_archive_lag_records Records between current LSN and last archived LSN."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_archive_lag_records gauge");
        let _ = writeln!(body, "reddb_wal_archive_lag_records {}", lag);

        // PLAN.md Phase 11.4 — per-replica lag visibility. Emitted
        // when this primary has registered replicas; replicas that
        // haven't ack'd anything yet (`last_acked_lsn == 0`) still
        // show up so dashboards can detect "registered but stuck".
        let replicas = self.runtime.primary_replica_snapshots();
        let _ = writeln!(
            body,
            "# HELP reddb_replica_count Currently registered replicas."
        );
        let _ = writeln!(body, "# TYPE reddb_replica_count gauge");
        let _ = writeln!(body, "reddb_replica_count {}", replicas.len());
        if !replicas.is_empty() {
            let replica_lag_budget_secs = std::env::var("RED_SLO_REPLICA_LAG_BUDGET_SECONDS")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(60.0);
            let _ = writeln!(
                body,
                "# HELP reddb_replica_ack_lsn Most recent LSN acked by each replica."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_ack_lsn gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_ack_lsn{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.last_acked_lsn
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_lag_records Distance from primary current LSN to replica acked LSN."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_lag_records gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_lag_records{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    current_lsn.saturating_sub(r.last_acked_lsn)
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_lag_seconds Wall-clock seconds since the replica was last seen."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_lag_seconds gauge");
            let _ = writeln!(
                body,
                "# HELP reddb_slo_lag_budget_remaining_seconds Remaining per-replica lag budget; negative means SLO breach."
            );
            let _ = writeln!(body, "# TYPE reddb_slo_lag_budget_remaining_seconds gauge");
            for r in &replicas {
                let lag_ms = (now_ms as u128).saturating_sub(r.last_seen_at_unix_ms);
                let lag_secs = (lag_ms as f64) / 1000.0;
                let _ = writeln!(
                    body,
                    "reddb_replica_lag_seconds{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    lag_secs
                );
                let _ = writeln!(
                    body,
                    "reddb_slo_lag_budget_remaining_seconds{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    replica_lag_budget_secs - lag_secs
                );
            }
        }

        // PLAN.md Phase 11.5 — replica apply error counters and
        // current health label. Counters are global across the
        // instance lifetime; the health label reflects whatever the
        // replica loop last persisted (`ok`, `connecting`, `gap`,
        // `divergence`, `apply_error`, `stalled_gap`).
        let _ = writeln!(
            body,
            "# HELP reddb_replica_apply_errors_total Replica WAL apply errors since process start, by kind."
        );
        let _ = writeln!(body, "# TYPE reddb_replica_apply_errors_total counter");
        for (kind, count) in self.runtime.replica_apply_error_counts() {
            let _ = writeln!(
                body,
                "reddb_replica_apply_errors_total{{kind=\"{}\"}} {}",
                kind.label(),
                count
            );
        }
        if let Some(health) = self.runtime.replica_apply_health() {
            let _ = writeln!(
                body,
                "# HELP reddb_replica_apply_health Replica apply state (label, value=1)."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_apply_health gauge");
            let _ = writeln!(
                body,
                "reddb_replica_apply_health{{state=\"{}\"}} 1",
                sanitize_label(&health)
            );
        }

        // PLAN.md Phase 4.4 — per-caller quota rejections. Empty
        // when the quota is unconfigured or no caller has been
        // throttled yet. Opportunistic eviction here keeps the
        // rejection map bounded on long-lived processes.
        self.runtime.quota_bucket().evict_idle();
        let rejections = self.runtime.quota_bucket().rejection_snapshot();
        if !rejections.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_quota_rejected_total Requests rejected by per-caller QPS quota."
            );
            let _ = writeln!(body, "# TYPE reddb_quota_rejected_total counter");
            for (principal, count) in &rejections {
                let _ = writeln!(
                    body,
                    "reddb_quota_rejected_total{{principal=\"{}\"}} {}",
                    sanitize_label(principal),
                    count
                );
            }
        }

        // PLAN.md Phase 11.4 — commit waiter outcome counters and
        // last-wait gauge. Operators alert when `timed_out` rises
        // (policy too tight or replicas stalled) and watch the
        // last-wait gauge for p95 trends.
        let (reached, timed_out, not_required, last_micros) =
            self.runtime.commit_waiter_metrics_snapshot();
        let _ = writeln!(
            body,
            "# HELP reddb_commit_wait_total Commit-wait outcomes by kind."
        );
        let _ = writeln!(body, "# TYPE reddb_commit_wait_total counter");
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"reached\"}} {}",
            reached
        );
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"timed_out\"}} {}",
            timed_out
        );
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"not_required\"}} {}",
            not_required
        );
        let _ = writeln!(
            body,
            "# HELP reddb_commit_wait_last_seconds Wall-clock seconds of the most recent commit wait."
        );
        let _ = writeln!(body, "# TYPE reddb_commit_wait_last_seconds gauge");
        let _ = writeln!(
            body,
            "reddb_commit_wait_last_seconds {}",
            (last_micros as f64) / 1_000_000.0
        );

        // PLAN.md Phase 11.4 — declared commit policy as a labeled
        // gauge so dashboards can confirm what the operator pinned.
        // The default `local` is emitted even when no replication is
        // configured, so the metric is always present.
        let policy = self.runtime.commit_policy();
        let _ = writeln!(
            body,
            "# HELP reddb_primary_commit_policy Active commit policy on the primary."
        );
        let _ = writeln!(body, "# TYPE reddb_primary_commit_policy gauge");
        let _ = writeln!(
            body,
            "reddb_primary_commit_policy{{policy=\"{}\"}} 1",
            policy.label()
        );

        // Blob Cache observability for the SQL result-cache adapter.
        // Per-namespace label cardinality is acceptable while the MVP namespace
        // cap stays near 256; raising that cap should move per-namespace detail
        // to an on-demand admin query and keep scrape metrics rolled up.
        let blob_ns = "runtime.result_cache";
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_get_total Blob Cache get outcomes by namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_get_total counter");
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"hit_l1\"}} {}",
            blob_ns,
            result_blob_stats.hits()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"hit_l2\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"miss\"}} {}",
            blob_ns,
            result_blob_stats.misses()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_put_total Blob Cache put outcomes by namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_put_total counter");
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"ok\"}} {}",
            blob_ns,
            result_blob_stats.insertions()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"version_mismatch\"}} {}",
            blob_ns,
            result_blob_stats.version_mismatches()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"too_large\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"metadata_too_large\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_invalidate_total Blob Cache invalidations by namespace and kind."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_invalidate_total counter");
        for (kind, count) in [
            ("key", 0),
            ("prefix", 0),
            ("tag", 0),
            ("dependency", result_blob_stats.invalidations()),
            ("namespace", result_blob_stats.namespace_flushes()),
        ] {
            let _ = writeln!(
                body,
                "reddb_cache_blob_invalidate_total{{namespace=\"{}\",kind=\"{}\"}} {}",
                blob_ns, kind, count
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_evict_total Blob Cache evictions by namespace and reason."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_evict_total counter");
        for (reason, count) in [
            ("capacity", result_blob_stats.evictions()),
            ("expiry", result_blob_stats.expirations()),
            ("policy", 0),
        ] {
            let _ = writeln!(
                body,
                "reddb_cache_blob_evict_total{{namespace=\"{}\",reason=\"{}\"}} {}",
                blob_ns, reason, count
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l1_bytes_in_use L1 bytes currently used by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l1_bytes_in_use gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l1_bytes_in_use{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.bytes_in_use()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l1_entries L1 entries currently held by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l1_entries gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l1_entries{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.entries()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l2_bytes_in_use L2 bytes currently used by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l2_bytes_in_use gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l2_bytes_in_use{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.l2_bytes_in_use()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l2_full_rejections_total Blob Cache puts rejected because L2 is full."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_cache_blob_l2_full_rejections_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_l2_full_rejections_total{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.l2_full_rejections()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_version_mismatch_total Blob Cache CAS version mismatches by namespace."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_cache_blob_version_mismatch_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_version_mismatch_total{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.version_mismatches()
        );

        let _ = writeln!(
            body,
            "# HELP reddb_kv_ops_total Normal-KV operations since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_ops_total counter");
        for (verb, count) in [
            ("put", kv_stats.puts),
            ("get", kv_stats.gets),
            ("delete", kv_stats.deletes),
            ("incr", kv_stats.incrs),
        ] {
            let _ = writeln!(body, "reddb_kv_ops_total{{verb=\"{}\"}} {}", verb, count);
        }
        let _ = writeln!(
            body,
            "# HELP reddb_kv_cas_total Normal-KV CAS outcomes since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_cas_total counter");
        let _ = writeln!(
            body,
            "reddb_kv_cas_total{{outcome=\"success\"}} {}",
            kv_stats.cas_success
        );
        let _ = writeln!(
            body,
            "reddb_kv_cas_total{{outcome=\"conflict\"}} {}",
            kv_stats.cas_conflict
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_streams_active Active normal-KV WATCH streams."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_streams_active gauge");
        let _ = writeln!(
            body,
            "reddb_kv_watch_streams_active {}",
            kv_stats.watch_streams_active
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_events_emitted_total Normal-KV WATCH events emitted since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_events_emitted_total counter");
        let _ = writeln!(
            body,
            "reddb_kv_watch_events_emitted_total {}",
            kv_stats.watch_events_emitted
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_drops_total Normal-KV WATCH events dropped by bounded subscriber buffers."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_drops_total counter");
        let _ = writeln!(body, "reddb_kv_watch_drops_total {}", kv_stats.watch_drops);

        let _ = writeln!(
            body,
            "# HELP reddb_db_size_bytes On-disk size of the primary database file."
        );
        let _ = writeln!(body, "# TYPE reddb_db_size_bytes gauge");
        let _ = writeln!(body, "reddb_db_size_bytes {}", db_size_bytes);

        if let Some(secs) = cold_start_secs {
            let _ = writeln!(
                body,
                "# HELP reddb_cold_start_duration_seconds Seconds from process start to /health/ready 200."
            );
            let _ = writeln!(body, "# TYPE reddb_cold_start_duration_seconds gauge");
            let _ = writeln!(body, "reddb_cold_start_duration_seconds {}", secs);
        }

        // PLAN.md Phase 9.1 — per-phase cold-start breakdown.
        // Operators use this to identify which phase dominates the
        // cold-start budget (restore, WAL replay, index warmup).
        // Phases that haven't fired yet are simply absent — no zero
        // entries to confuse alert rules.
        let phases = lifecycle.cold_start_phases().durations_ms();
        if !phases.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_cold_start_phase_seconds Per-phase cold-start duration."
            );
            let _ = writeln!(body, "# TYPE reddb_cold_start_phase_seconds gauge");
            for (name, dur_ms) in phases {
                let _ = writeln!(
                    body,
                    "reddb_cold_start_phase_seconds{{phase=\"{}\"}} {}",
                    name,
                    (dur_ms as f64) / 1000.0
                );
            }
        }

        // Operator-imposed limits (PLAN.md Phase 4.1). Emitted as
        // gauges so external dashboards can graph headroom against
        // current usage. `0` means "no cap pinned at boot"; we
        // still emit it so absence vs presence is unambiguous.
        let limits = self.runtime.resource_limits();
        if let Some(v) = limits.max_db_size_bytes {
            let _ = writeln!(
                body,
                "# HELP reddb_limit_db_size_bytes Operator-pinned cap on the primary DB file size."
            );
            let _ = writeln!(body, "# TYPE reddb_limit_db_size_bytes gauge");
            let _ = writeln!(body, "reddb_limit_db_size_bytes {}", v);
        }
        if let Some(v) = limits.max_connections {
            let _ = writeln!(body, "# TYPE reddb_limit_connections gauge");
            let _ = writeln!(body, "reddb_limit_connections {}", v);
        }
        if let Some(v) = limits.max_qps {
            let _ = writeln!(body, "# TYPE reddb_limit_qps gauge");
            let _ = writeln!(body, "reddb_limit_qps {}", v);
        }
        if let Some(v) = limits.max_batch_size {
            let _ = writeln!(body, "# TYPE reddb_limit_batch_size gauge");
            let _ = writeln!(body, "reddb_limit_batch_size {}", v);
        }
        if let Some(v) = limits.max_memory_bytes {
            let _ = writeln!(body, "# TYPE reddb_limit_memory_bytes gauge");
            let _ = writeln!(body, "reddb_limit_memory_bytes {}", v);
        }

        // Events outbox metrics — issue #299
        {
            use crate::runtime::impl_queue::{
                EVENTS_DLQ_TOTAL, EVENTS_DRAIN_RETRIES_TOTAL, EVENTS_ENQUEUED_TOTAL,
            };
            let enqueued = EVENTS_ENQUEUED_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
            let retries = EVENTS_DRAIN_RETRIES_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
            let dlq_total = EVENTS_DLQ_TOTAL.load(std::sync::atomic::Ordering::Relaxed);

            let _ = writeln!(
                body,
                "# HELP reddb_events_enqueued_total Total events successfully pushed to target queues."
            );
            let _ = writeln!(body, "# TYPE reddb_events_enqueued_total counter");
            let _ = writeln!(body, "reddb_events_enqueued_total {enqueued}");

            let _ = writeln!(
                body,
                "# HELP reddb_events_drain_retries_total Total event push failures that triggered DLQ routing."
            );
            let _ = writeln!(body, "# TYPE reddb_events_drain_retries_total counter");
            let _ = writeln!(
                body,
                "reddb_events_drain_retries_total{{reason=\"queue_full\"}} {retries}"
            );

            let _ = writeln!(
                body,
                "# HELP reddb_events_dlq_total Total events routed to dead-letter queues."
            );
            let _ = writeln!(body, "# TYPE reddb_events_dlq_total counter");
            let _ = writeln!(body, "reddb_events_dlq_total {dlq_total}");
        }

        // AI provider and embedding metrics — issue #280.
        crate::runtime::ai::metrics::render_ai_metrics(&mut body);

        HttpResponse {
            status: 200,
            content_type: "text/plain; version=0.0.4",
            body: body.into_bytes(),
            extra_headers: Vec::new(),
        }
    }

    /// `GET /admin/status` — full structured snapshot of operator-
    /// relevant state (PLAN.md Phase 5.4). One JSON object that
    /// frontend dashboards / control-plane sidecars can poll
    /// without scraping multiple endpoints.
    pub(crate) fn handle_admin_status(&self) -> HttpResponse {
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let uptime_secs = (now_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0;
        let read_only = self.runtime.write_gate().is_read_only();
        let role = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => "standalone",
            crate::replication::ReplicationRole::Primary => "primary",
            crate::replication::ReplicationRole::Replica { .. } => "replica",
        };
        let db = self.runtime.db();
        let db_size_bytes = db
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let backend_kind = db
            .options()
            .remote_backend
            .as_ref()
            .map(|b| b.name().to_string());

        let mut object = Map::new();
        object.insert(
            "version".to_string(),
            JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
        );
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        object.insert(
            "uptime_secs".to_string(),
            JsonValue::Number((uptime_secs * 1000.0).round() / 1000.0),
        );
        object.insert(
            "started_at_unix_ms".to_string(),
            JsonValue::Number(lifecycle.started_at_ms() as f64),
        );
        if let Some(ready_at) = lifecycle.ready_at_ms() {
            object.insert(
                "ready_at_unix_ms".to_string(),
                JsonValue::Number(ready_at as f64),
            );
        }
        object.insert(
            "db_size_bytes".to_string(),
            JsonValue::Number(db_size_bytes as f64),
        );
        object.insert("read_only".to_string(), JsonValue::Bool(read_only));
        object.insert(
            "replication_role".to_string(),
            JsonValue::String(role.to_string()),
        );
        object.insert(
            "writer_lease".to_string(),
            JsonValue::String(self.runtime.write_gate().lease_state().label().to_string()),
        );

        // PLAN.md Phase 6.3 — surface encryption-at-rest configuration
        // so dashboards / `red doctor` can flag a misconfigured key
        // (Err on parse) before it silently leaves data plaintext.
        let (enc_state, enc_error) = self.runtime.encryption_at_rest_status();
        let mut enc_obj = Map::new();
        enc_obj.insert(
            "state".to_string(),
            JsonValue::String(enc_state.to_string()),
        );
        if let Some(err) = enc_error {
            enc_obj.insert("error".to_string(), JsonValue::String(err));
        }
        object.insert("encryption_at_rest".to_string(), JsonValue::Object(enc_obj));

        // Backup posture (PLAN.md Phase 5.1). `last_backup` carries
        // the same shape /metrics emits so dashboards and alert rules
        // share a single contract.
        let backup = self.runtime.backup_status();
        let mut backup_obj = Map::new();
        if let Some(last) = backup.last_backup.as_ref() {
            backup_obj.insert(
                "last_success_unix_ms".to_string(),
                JsonValue::Number(last.timestamp as f64),
            );
            backup_obj.insert(
                "last_duration_ms".to_string(),
                JsonValue::Number(last.duration_ms as f64),
            );
            backup_obj.insert(
                "age_seconds".to_string(),
                JsonValue::Number(((now_ms.saturating_sub(last.timestamp)) as f64) / 1000.0),
            );
        }
        backup_obj.insert(
            "total_successes".to_string(),
            JsonValue::Number(backup.total_backups as f64),
        );
        backup_obj.insert(
            "total_failures".to_string(),
            JsonValue::Number(backup.total_failures as f64),
        );
        backup_obj.insert(
            "interval_secs".to_string(),
            JsonValue::Number(backup.interval_secs as f64),
        );
        object.insert("backup".to_string(), JsonValue::Object(backup_obj));

        // WAL archive lag.
        let (current_lsn, last_archived_lsn) = self.runtime.wal_archive_progress();
        let mut wal_obj = Map::new();
        wal_obj.insert(
            "current_lsn".to_string(),
            JsonValue::Number(current_lsn as f64),
        );
        wal_obj.insert(
            "last_archived_lsn".to_string(),
            JsonValue::Number(last_archived_lsn as f64),
        );
        wal_obj.insert(
            "archive_lag_records".to_string(),
            JsonValue::Number(current_lsn.saturating_sub(last_archived_lsn) as f64),
        );
        object.insert("wal".to_string(), JsonValue::Object(wal_obj));

        // PLAN.md Phase 11.5 — replica apply health + counters.
        // Always emit so dashboards have a stable shape; missing
        // health label means this isn't a replica or no apply has
        // happened yet.
        let mut replica_obj = Map::new();
        if let Some(health) = self.runtime.replica_apply_health() {
            replica_obj.insert("apply_health".to_string(), JsonValue::String(health));
        }
        let mut errors_obj = Map::new();
        for (kind, count) in self.runtime.replica_apply_error_counts() {
            errors_obj.insert(kind.label().to_string(), JsonValue::Number(count as f64));
        }
        replica_obj.insert("apply_errors".to_string(), JsonValue::Object(errors_obj));
        // Per-replica array (primary view). Empty on replica/standalone.
        let snaps = self.runtime.primary_replica_snapshots();
        if !snaps.is_empty() {
            let arr: Vec<JsonValue> = snaps
                .iter()
                .map(|r| {
                    let mut o = Map::new();
                    o.insert("id".to_string(), JsonValue::String(r.id.clone()));
                    o.insert(
                        "last_acked_lsn".to_string(),
                        JsonValue::Number(r.last_acked_lsn as f64),
                    );
                    o.insert(
                        "last_sent_lsn".to_string(),
                        JsonValue::Number(r.last_sent_lsn as f64),
                    );
                    o.insert(
                        "last_durable_lsn".to_string(),
                        JsonValue::Number(r.last_durable_lsn as f64),
                    );
                    o.insert(
                        "last_seen_at_unix_ms".to_string(),
                        JsonValue::Number(r.last_seen_at_unix_ms as f64),
                    );
                    o.insert(
                        "lag_records".to_string(),
                        JsonValue::Number(current_lsn.saturating_sub(r.last_acked_lsn) as f64),
                    );
                    if let Some(region) = &r.region {
                        o.insert("region".to_string(), JsonValue::String(region.clone()));
                    }
                    JsonValue::Object(o)
                })
                .collect();
            replica_obj.insert("primary_view".to_string(), JsonValue::Array(arr));
        }
        replica_obj.insert(
            "commit_policy".to_string(),
            JsonValue::String(self.runtime.commit_policy().label().to_string()),
        );
        // PLAN.md Phase 11.4 — durable-LSN map per replica for
        // ack_n debugging. Empty until at least one ack lands.
        let durable = self.runtime.commit_waiter_snapshot();
        if !durable.is_empty() {
            let arr: Vec<JsonValue> = durable
                .into_iter()
                .map(|(id, lsn)| {
                    let mut o = Map::new();
                    o.insert("replica_id".to_string(), JsonValue::String(id));
                    o.insert("durable_lsn".to_string(), JsonValue::Number(lsn as f64));
                    JsonValue::Object(o)
                })
                .collect();
            replica_obj.insert("durable_view".to_string(), JsonValue::Array(arr));
        }
        object.insert("replica".to_string(), JsonValue::Object(replica_obj));
        if let Some(backend) = backend_kind {
            object.insert("remote_backend".to_string(), JsonValue::String(backend));
        }
        // PLAN.md Phase 4.1 — operator-imposed limits surface so
        // external dashboards can show headroom alongside usage.
        let limits = self.runtime.resource_limits();
        let mut limits_obj = Map::new();
        if let Some(v) = limits.max_db_size_bytes {
            limits_obj.insert("max_db_size_bytes".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_connections {
            limits_obj.insert("max_connections".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_qps {
            limits_obj.insert("max_qps".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_batch_size {
            limits_obj.insert("max_batch_size".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_memory_bytes {
            limits_obj.insert("max_memory_bytes".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(d) = limits.max_query_duration {
            limits_obj.insert(
                "max_query_duration_ms".to_string(),
                JsonValue::Number(d.as_millis() as f64),
            );
        }
        if let Some(v) = limits.max_result_bytes {
            limits_obj.insert("max_result_bytes".to_string(), JsonValue::Number(v as f64));
        }
        object.insert("limits".to_string(), JsonValue::Object(limits_obj));

        if let Some(report) = lifecycle.shutdown_report() {
            let mut shutdown_obj = Map::new();
            shutdown_obj.insert(
                "duration_ms".to_string(),
                JsonValue::Number(report.duration_ms as f64),
            );
            shutdown_obj.insert(
                "flushed_wal".to_string(),
                JsonValue::Bool(report.flushed_wal),
            );
            shutdown_obj.insert(
                "backup_uploaded".to_string(),
                JsonValue::Bool(report.backup_uploaded),
            );
            object.insert("shutdown".to_string(), JsonValue::Object(shutdown_obj));
        }
        json_response(200, JsonValue::Object(object))
    }

    /// `POST /admin/drain` — flip to Draining phase. Subsequent
    /// `WriteGate`-checked writes will be rejected until shutdown
    /// completes or another phase override re-enables Ready.
    /// Idempotent.
    /// `POST /admin/failover/promote` — manual replica → primary
    /// promotion (PLAN.md Phase 11.6).
    ///
    /// Hard checks before bumping the lease generation:
    ///   * Caller is currently a replica (role guard) — primaries
    ///     don't promote themselves.
    ///   * Remote backend is configured (lease lives there).
    ///   * Replica apply health is `ok` — no unresolved WAL gap or
    ///     divergence. A replica that's behind cannot become the
    ///     authoritative writer.
    ///   * Lease can be acquired — `try_acquire` returns success.
    ///     Failure surfaces the existing holder so the operator
    ///     understands why.
    ///
    /// Body: `{"holder_id": "...", "ttl_ms": <u64>}`. `holder_id`
    /// defaults to `RED_LEASE_HOLDER_ID` env / `<hostname>-<pid>`.
    /// `ttl_ms` defaults to 60_000.
    ///
    /// On success the response includes the new lease's generation
    /// and acquired_at. **Promotion does NOT flip the running role
    /// to primary** — the operator's runbook is to restart the
    /// process with `RED_REPLICATION_MODE=primary` after a
    /// successful promotion. Auto-role-flip is a Phase 11.6 follow-
    /// up that requires draining live read traffic safely.
    pub(crate) fn handle_admin_failover_promote(&self, body: Vec<u8>) -> HttpResponse {
        // Role guard.
        if !matches!(
            self.runtime.write_gate().role(),
            crate::replication::ReplicationRole::Replica { .. }
        ) {
            return json_error(
                409,
                "promotion only allowed on a replica (current role is not Replica)",
            );
        }

        // Backend guard.
        let Some(backend) = self.runtime.db().options().remote_backend_atomic.clone() else {
            return json_error(
                412,
                "promotion requires a CAS-capable remote backend (use s3, fs, or http with RED_HTTP_CONDITIONAL_WRITES=true)",
            );
        };

        // Apply health guard. Anything other than `ok` / `healthy`
        // / `connecting` indicates the replica isn't current.
        let health = self.runtime.replica_apply_health().unwrap_or_default();
        if matches!(
            health.as_str(),
            "stalled_gap" | "divergence" | "apply_error"
        ) {
            return json_error(
                409,
                format!(
                    "promotion refused — replica apply state is `{health}`; resolve before promoting"
                ),
            );
        }

        // Body parsing.
        let (holder_id, ttl_ms) = if body.is_empty() {
            (default_holder_id(), 60_000u64)
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => {
                    let holder = v
                        .get("holder_id")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(default_holder_id);
                    let ttl = v
                        .get("ttl_ms")
                        .and_then(|n| n.as_u64())
                        .filter(|t| *t > 0)
                        .unwrap_or(60_000);
                    (holder, ttl)
                }
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };

        let database_key = self
            .runtime
            .db()
            .options()
            .remote_key
            .clone()
            .unwrap_or_else(|| "main".to_string());
        let store = crate::replication::LeaseStore::new(backend);

        match crate::runtime::lease_lifecycle::admin_promote_lease(
            &store,
            self.runtime.audit_log(),
            &database_key,
            &holder_id,
            ttl_ms,
        ) {
            Ok(lease) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("holder_id".to_string(), JsonValue::String(lease.holder_id));
                object.insert(
                    "generation".to_string(),
                    JsonValue::Number(lease.generation as f64),
                );
                object.insert(
                    "acquired_at_ms".to_string(),
                    JsonValue::Number(lease.acquired_at_ms as f64),
                );
                object.insert(
                    "expires_at_ms".to_string(),
                    JsonValue::Number(lease.expires_at_ms as f64),
                );
                object.insert(
                    "next_step".to_string(),
                    JsonValue::String(
                        "restart with RED_REPLICATION_MODE=primary to start accepting writes"
                            .to_string(),
                    ),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(409, format!("promotion refused: {err}")),
        }
    }

    /// `GET /admin/audit` — structured audit log query for compliance
    /// (SOC 2 / HIPAA / ISO 27001). Reads the active `.audit.log`
    /// plus rotated `.audit.log.<ms>.zst` archives, applies the
    /// query filters, and returns the matching events as a JSON
    /// object: `{"count": n, "events": [...]}`.
    ///
    /// Supported query params:
    ///   * `since` / `until` — RFC 3339 (`...Z`) or ms epoch.
    ///   * `principal` — exact match (e.g. `alice@acme`).
    ///   * `tenant` — exact match.
    ///   * `action` — prefix match (e.g. `auth/`, `admin/`).
    ///   * `outcome` — `success` / `denied` / `error`.
    ///   * `limit` — default 100, max 1000.
    ///   * `format` — `json` (default) or `jsonl`.
    ///
    /// Auth: relies on the `RED_ADMIN_TOKEN` gate already enforced
    /// for every `/admin/*` path in `is_authorized`. When that env
    /// var is unset the endpoint is open — same posture as every
    /// other admin endpoint.
    pub(crate) fn handle_admin_audit_query(
        &self,
        query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        use crate::runtime::audit_log::Outcome;
        use crate::runtime::audit_query::{
            events_to_json_array, parse_time_arg, run_query, AuditQuery,
        };

        let mut q = AuditQuery::new();
        if let Some(s) = query.get("since") {
            q.since_ms = parse_time_arg(s);
            if q.since_ms.is_none() {
                return json_error(400, format!("invalid 'since' value: {s}"));
            }
        }
        if let Some(u) = query.get("until") {
            q.until_ms = parse_time_arg(u);
            if q.until_ms.is_none() {
                return json_error(400, format!("invalid 'until' value: {u}"));
            }
        }
        if let Some(p) = query.get("principal") {
            if !p.is_empty() {
                q.principal = Some(p.clone());
            }
        }
        if let Some(t) = query.get("tenant") {
            if !t.is_empty() {
                q.tenant = Some(t.clone());
            }
        }
        if let Some(a) = query.get("action") {
            if !a.is_empty() {
                q.action_prefix = Some(a.clone());
            }
        }
        if let Some(o) = query.get("outcome") {
            if let Some(parsed) = Outcome::parse(o) {
                q.outcome = Some(parsed);
            } else {
                return json_error(
                    400,
                    format!("invalid 'outcome' value: {o} (expected success|denied|error)"),
                );
            }
        }
        if let Some(l) = query.get("limit") {
            match l.parse::<usize>() {
                Ok(n) if n > 0 => q.limit = n.min(1000),
                _ => return json_error(400, format!("invalid 'limit' value: {l}")),
            }
        } else {
            q.limit = 100;
        }

        let format = query
            .get("format")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        let path = self.runtime.audit_log().path().to_path_buf();
        let events = run_query(&path, &q);

        if format == "jsonl" || format == "ndjson" {
            let mut body = String::new();
            for ev in &events {
                body.push_str(&ev.to_json_line(None));
                body.push('\n');
            }
            return HttpResponse {
                status: 200,
                content_type: "application/x-ndjson",
                body: body.into_bytes(),
                extra_headers: Vec::new(),
            };
        }

        json_response(200, events_to_json_array(&events))
    }

    pub(crate) fn handle_admin_drain(&self) -> HttpResponse {
        self.runtime.lifecycle().mark_draining();
        self.runtime.audit_log().record(
            "admin/drain",
            "operator",
            "instance",
            "ok",
            JsonValue::Null,
        );
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "phase".to_string(),
            JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
        );
        json_response(200, JsonValue::Object(object))
    }

    /// `GET /health/live` — process is alive and responsive. Always
    /// 200 once the runtime is constructed; 503 only after Stopped.
    /// Never touches I/O.
    pub(crate) fn handle_health_live(&self) -> HttpResponse {
        let phase = self.runtime.lifecycle().phase();
        let alive = !matches!(phase, Phase::Stopped);
        let status = if alive { 200 } else { 503 };
        let mut object = Map::new();
        object.insert(
            "status".to_string(),
            JsonValue::String(if alive { "alive" } else { "stopped" }.to_string()),
        );
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        json_response(status, JsonValue::Object(object))
    }

    /// `GET /health/ready` — runtime is fully past WAL replay /
    /// restore-from-remote and accepts queries.
    pub(crate) fn handle_health_ready(&self) -> HttpResponse {
        self.health_ready_response("ready")
    }

    /// `GET /health/startup` — Kubernetes startup probe variant.
    /// Same readiness logic as `/health/ready`; orchestrator gives
    /// it a longer grace window before failing the pod.
    pub(crate) fn handle_health_startup(&self) -> HttpResponse {
        self.health_ready_response("startup")
    }

    fn health_ready_response(&self, probe: &str) -> HttpResponse {
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let started_at = lifecycle.started_at_ms();
        let since_secs = (now.saturating_sub(started_at) as f64) / 1000.0;
        let mut object = Map::new();
        object.insert("probe".to_string(), JsonValue::String(probe.to_string()));
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        object.insert(
            "since_secs".to_string(),
            JsonValue::Number((since_secs * 1000.0).round() / 1000.0),
        );
        if let Some(ready_at) = lifecycle.ready_at_ms() {
            object.insert(
                "ready_at_unix_ms".to_string(),
                JsonValue::Number(ready_at as f64),
            );
        }

        if phase.accepts_queries() {
            object.insert("status".to_string(), JsonValue::String("ready".to_string()));
            json_response(200, JsonValue::Object(object))
        } else {
            object.insert(
                "status".to_string(),
                JsonValue::String(phase.as_str().to_string()),
            );
            if let Some(reason) = lifecycle.not_ready_reason() {
                object.insert("reason".to_string(), JsonValue::String(reason));
            } else {
                object.insert(
                    "reason".to_string(),
                    JsonValue::String(match phase {
                        Phase::Starting => "starting".to_string(),
                        Phase::ShuttingDown => "shutting_down".to_string(),
                        Phase::Stopped => "stopped".to_string(),
                        Phase::Draining => "draining".to_string(),
                        Phase::Ready => "ready".to_string(),
                    }),
                );
            }
            json_response(503, JsonValue::Object(object))
        }
    }

    // -----------------------------------------------------------------
    // IAM policy admin endpoints
    // -----------------------------------------------------------------

    fn iam_audit(&self, action: &str, target: &str, outcome: &str) {
        self.runtime
            .audit_log()
            .record(action, "operator", target, outcome, JsonValue::Null);
    }

    /// `PUT /admin/policies/:id` — install or replace an IAM policy.
    pub(crate) fn handle_iam_policy_put(&self, id: &str, body: Vec<u8>) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let Ok(text) = std::str::from_utf8(&body) else {
            return json_error(400, "body must be utf-8 JSON");
        };
        let mut policy = match crate::auth::policies::Policy::from_json_str(text) {
            Ok(p) => p,
            Err(e) => return json_error(400, format!("policy parse: {e}")),
        };
        if policy.id != id {
            policy.id = id.to_string();
        }
        if let Err(e) = store.put_policy(policy) {
            return json_error(400, e.to_string());
        }
        self.runtime.invalidate_result_cache();
        self.iam_audit("iam/policy.put", id, "ok");
        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("id".to_string(), JsonValue::String(id.to_string()));
        json_response(200, JsonValue::Object(obj))
    }

    /// `GET /admin/policies/:id` — fetch a single policy as JSON.
    pub(crate) fn handle_iam_policy_get(&self, id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let Some(p) = store.get_policy(id) else {
            return json_error(404, format!("policy `{id}` not found"));
        };
        let body = p.to_json_string();
        HttpResponse {
            status: 200,
            content_type: "application/json",
            body: body.into_bytes(),
            extra_headers: Vec::new(),
        }
    }

    /// `GET /admin/policies` — list policies (id-sorted summary).
    pub(crate) fn handle_iam_policy_list(&self) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let pols = store.list_policies();
        let items: Vec<JsonValue> = pols
            .iter()
            .map(|p| {
                let mut obj = Map::new();
                obj.insert("id".to_string(), JsonValue::String(p.id.clone()));
                obj.insert("version".to_string(), JsonValue::Number(p.version as f64));
                obj.insert(
                    "statements".to_string(),
                    JsonValue::Number(p.statements.len() as f64),
                );
                obj.insert(
                    "tenant".to_string(),
                    p.tenant
                        .as_deref()
                        .map(|t| JsonValue::String(t.to_string()))
                        .unwrap_or(JsonValue::Null),
                );
                JsonValue::Object(obj)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("items".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `DELETE /admin/policies/:id` — drop a policy.
    pub(crate) fn handle_iam_policy_delete(&self, id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        match store.delete_policy(id) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit("iam/policy.drop", id, "ok");
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(404, e.to_string()),
        }
    }

    /// `PUT /admin/users/:user/policies/:policy_id`. `:user` may
    /// optionally be tenant-qualified as `tenant.username`.
    pub(crate) fn handle_iam_attach_user(&self, user: &str, policy_id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.attach_policy(
            crate::auth::store::PrincipalRef::User(uid.clone()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.attach",
                    &format!("user:{uid}::{policy_id}"),
                    "ok",
                );
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/users/:user/policies/:policy_id`.
    pub(crate) fn handle_iam_detach_user(&self, user: &str, policy_id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.detach_policy(
            crate::auth::store::PrincipalRef::User(uid.clone()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.detach",
                    &format!("user:{uid}::{policy_id}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `PUT /admin/users/:user/groups/:group`.
    pub(crate) fn handle_iam_add_user_group(&self, user: &str, group: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.add_user_to_group(&uid, group) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit("iam/group.add", &format!("user:{uid}::group:{group}"), "ok");
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/users/:user/groups/:group`.
    pub(crate) fn handle_iam_remove_user_group(&self, user: &str, group: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.remove_user_from_group(&uid, group) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/group.remove",
                    &format!("user:{uid}::group:{group}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `PUT /admin/groups/:group/policies/:policy_id`.
    pub(crate) fn handle_iam_attach_group(&self, group: &str, policy_id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        match store.attach_policy(
            crate::auth::store::PrincipalRef::Group(group.to_string()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.attach",
                    &format!("group:{group}::{policy_id}"),
                    "ok",
                );
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/groups/:group/policies/:policy_id`.
    pub(crate) fn handle_iam_detach_group(&self, group: &str, policy_id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        match store.detach_policy(
            crate::auth::store::PrincipalRef::Group(group.to_string()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.detach",
                    &format!("group:{group}::{policy_id}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `GET /admin/users/:user/effective-permissions[?resource=kind:name]`.
    pub(crate) fn handle_iam_effective_permissions(
        &self,
        user: &str,
        query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        let pols = store.effective_policies(&uid);

        // Build a JSON array of policy summaries scoped to the user.
        // The optional `resource` query string parameter is parsed but
        // currently only echoed back — fine-grained matching falls
        // through to `simulate`.
        let resource_echo = query.get("resource").cloned();
        let items: Vec<JsonValue> = pols
            .iter()
            .map(|p| {
                let mut obj = Map::new();
                obj.insert("id".to_string(), JsonValue::String(p.id.clone()));
                obj.insert(
                    "statements".to_string(),
                    JsonValue::Number(p.statements.len() as f64),
                );
                JsonValue::Object(obj)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("user".to_string(), JsonValue::String(uid.to_string()));
        if let Some(r) = resource_echo {
            envelope.insert("resource".to_string(), JsonValue::String(r));
        }
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("policies".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `POST /admin/policies/simulate` —
    /// body: `{principal, action, resource: {kind, name, tenant?}, ctx?}`.
    pub(crate) fn handle_iam_simulate(&self, body: Vec<u8>) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let parsed = match crate::serde_json::from_str::<crate::serde_json::Value>(
            std::str::from_utf8(&body).unwrap_or(""),
        ) {
            Ok(v) => v,
            Err(e) => return json_error(400, format!("invalid JSON body: {e}")),
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => return json_error(400, "body must be a JSON object"),
        };
        let principal = match obj.get("principal").and_then(|v| v.as_str()) {
            Some(s) => decode_user_arg(s),
            None => return json_error(400, "missing `principal`"),
        };
        let action = match obj.get("action").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json_error(400, "missing `action`"),
        };
        let resource = match obj.get("resource") {
            Some(JsonValue::Object(r)) => {
                let kind = r
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if kind.is_empty() || name.is_empty() {
                    return json_error(400, "resource needs kind+name");
                }
                let mut rr = crate::auth::policies::ResourceRef::new(kind, name);
                if let Some(t) = r.get("tenant").and_then(|v| v.as_str()) {
                    rr = rr.with_tenant(t.to_string());
                }
                rr
            }
            Some(JsonValue::String(s)) => match s.split_once(':') {
                Some((k, n)) => crate::auth::policies::ResourceRef::new(k, n),
                None => return json_error(400, "resource string must be `kind:name`"),
            },
            _ => return json_error(400, "missing `resource`"),
        };
        let mut sim_ctx = crate::auth::store::SimCtx::default();
        if let Some(c) = obj.get("ctx").and_then(|v| v.as_object()) {
            if let Some(t) = c.get("current_tenant").and_then(|v| v.as_str()) {
                sim_ctx.current_tenant = Some(t.to_string());
            }
            if let Some(true) = c.get("mfa").and_then(|v| v.as_bool()) {
                sim_ctx.mfa_present = true;
            }
            if let Some(ip) = c
                .get("source_ip")
                .or_else(|| c.get("peer_ip"))
                .and_then(|v| v.as_str())
            {
                if let Ok(addr) = ip.parse() {
                    sim_ctx.peer_ip = Some(addr);
                }
            }
            if let Some(ms) = c.get("now_ms").and_then(|v| v.as_u64()) {
                sim_ctx.now_ms = Some(ms as u128);
            }
        }
        let outcome = store.simulate(&principal, &action, &resource, sim_ctx);
        let (decision_str, matched_pid, matched_sid) =
            crate::runtime::impl_core::decision_to_strings(&outcome.decision);

        self.iam_audit("iam/policy.simulate", &principal.to_string(), &decision_str);

        let mut envelope = Map::new();
        envelope.insert("decision".to_string(), JsonValue::String(decision_str));
        envelope.insert(
            "matched_policy_id".to_string(),
            matched_pid
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        );
        envelope.insert(
            "matched_sid".to_string(),
            matched_sid
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        );
        envelope.insert("reason".to_string(), JsonValue::String(outcome.reason));
        let trail: Vec<JsonValue> = outcome
            .trail
            .into_iter()
            .map(|t| {
                let mut obj = Map::new();
                obj.insert("policy_id".to_string(), JsonValue::String(t.policy_id));
                obj.insert(
                    "sid".to_string(),
                    t.sid.map(JsonValue::String).unwrap_or(JsonValue::Null),
                );
                obj.insert("matched".to_string(), JsonValue::Bool(t.matched));
                obj.insert(
                    "effect".to_string(),
                    JsonValue::String(
                        match t.effect {
                            crate::auth::policies::Effect::Allow => "allow",
                            crate::auth::policies::Effect::Deny => "deny",
                        }
                        .to_string(),
                    ),
                );
                obj.insert(
                    "why_skipped".to_string(),
                    t.why_skipped
                        .map(|s| JsonValue::String(s.to_string()))
                        .unwrap_or(JsonValue::Null),
                );
                JsonValue::Object(obj)
            })
            .collect();
        envelope.insert("trail".to_string(), JsonValue::Array(trail));
        json_response(200, JsonValue::Object(envelope))
    }
}

fn decode_user_arg(raw: &str) -> crate::auth::UserId {
    // Accepts `username` (platform tenant), `tenant.username` or
    // `tenant/username` to align with the SQL path / display form.
    if let Some((tenant, name)) = raw.split_once('/') {
        return crate::auth::UserId::scoped(tenant.to_string(), name.to_string());
    }
    if let Some((tenant, name)) = raw.split_once('.') {
        return crate::auth::UserId::scoped(tenant.to_string(), name.to_string());
    }
    crate::auth::UserId::platform(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_expose_result_blob_cache_label_set() {
        let runtime =
            crate::runtime::RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory())
                .expect("runtime");
        runtime
            .db()
            .store()
            .set_config_tree("runtime.result_cache.backend", &crate::json!("blob_cache"));

        runtime.execute_query("SELECT 1").expect("populate miss");
        runtime.execute_query("SELECT 1").expect("blob hit");
        runtime.invalidate_result_cache();

        let server = RedDBServer::new(runtime);
        let response = server.handle_metrics();
        let body = String::from_utf8(response.body).expect("utf8 metrics");

        for needle in [
            "reddb_cache_blob_get_total{namespace=\"runtime.result_cache\",result=\"hit_l1\"}",
            "reddb_cache_blob_get_total{namespace=\"runtime.result_cache\",result=\"hit_l2\"}",
            "reddb_cache_blob_get_total{namespace=\"runtime.result_cache\",result=\"miss\"}",
            "reddb_cache_blob_put_total{namespace=\"runtime.result_cache\",outcome=\"ok\"}",
            "reddb_cache_blob_put_total{namespace=\"runtime.result_cache\",outcome=\"version_mismatch\"}",
            "reddb_cache_blob_put_total{namespace=\"runtime.result_cache\",outcome=\"too_large\"}",
            "reddb_cache_blob_put_total{namespace=\"runtime.result_cache\",outcome=\"metadata_too_large\"}",
            "reddb_cache_blob_invalidate_total{namespace=\"runtime.result_cache\",kind=\"dependency\"}",
            "reddb_cache_blob_invalidate_total{namespace=\"runtime.result_cache\",kind=\"namespace\"}",
            "reddb_cache_blob_evict_total{namespace=\"runtime.result_cache\",reason=\"capacity\"}",
            "reddb_cache_blob_evict_total{namespace=\"runtime.result_cache\",reason=\"expiry\"}",
            "reddb_cache_blob_evict_total{namespace=\"runtime.result_cache\",reason=\"policy\"}",
            "reddb_cache_blob_l1_bytes_in_use{namespace=\"runtime.result_cache\"}",
            "reddb_cache_blob_l1_entries{namespace=\"runtime.result_cache\"}",
            "reddb_cache_blob_l2_bytes_in_use{namespace=\"runtime.result_cache\"}",
            "reddb_cache_blob_l2_full_rejections_total{namespace=\"runtime.result_cache\"}",
            "reddb_cache_blob_version_mismatch_total{namespace=\"runtime.result_cache\"}",
        ] {
            assert!(body.contains(needle), "missing metric line for {needle}");
        }
    }

    // -------------------------------------------------------------------
    // Issue #148 — Blob Cache admin endpoints (smoke + adversarial input)
    // -------------------------------------------------------------------

    fn test_server() -> RedDBServer {
        let runtime =
            crate::runtime::RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory())
                .expect("runtime");
        RedDBServer::new(runtime)
    }

    fn parse_body(resp: &HttpResponse) -> JsonValue {
        let s = std::str::from_utf8(&resp.body).expect("utf8 body");
        crate::serde_json::from_str::<JsonValue>(s).expect("JSON body")
    }

    #[test]
    fn admin_blob_cache_sweep_happy_path_returns_well_formed_report() {
        let server = test_server();
        let body = br#"{"limit_entries": 100, "limit_millis": 50}"#.to_vec();
        let resp = server.handle_admin_blob_cache_sweep(body);
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(parsed.get("ok").and_then(|v| v.as_bool()), Some(true));
        // Sweeper today is bounded scaffolding; report must still be
        // well-formed with all expected fields present.
        for field in [
            "entries_scanned",
            "entries_evicted",
            "bytes_reclaimed",
            "elapsed_ms",
            "truncated_due_to_limit",
        ] {
            assert!(
                parsed.get(field).is_some(),
                "missing field {field} in response: {parsed:?}"
            );
        }
    }

    #[test]
    fn admin_blob_cache_sweep_empty_body_uses_unbounded_default() {
        let server = test_server();
        let resp = server.handle_admin_blob_cache_sweep(Vec::new());
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(parsed.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn admin_blob_cache_sweep_invalid_json_returns_400() {
        let server = test_server();
        let resp = server.handle_admin_blob_cache_sweep(b"not json".to_vec());
        assert_eq!(resp.status, 400);
    }

    #[test]
    fn admin_blob_cache_flush_namespace_happy_path() {
        let server = test_server();
        let body = br#"{"namespace": "tenant-42:results"}"#.to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(parsed.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            parsed.get("namespace").and_then(|v| v.as_str()),
            Some("tenant-42:results")
        );
        assert!(parsed.get("elapsed_micros").is_some());
        assert!(parsed.get("generation_before").is_some());
        assert!(parsed.get("generation_after").is_some());
    }

    #[test]
    fn admin_blob_cache_flush_namespace_missing_body_returns_400() {
        let server = test_server();
        let resp = server.handle_admin_blob_cache_flush_namespace(Vec::new());
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        assert!(parsed
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.contains("namespace"))
            .unwrap_or(false));
    }

    #[test]
    fn admin_blob_cache_flush_namespace_missing_field_returns_400() {
        let server = test_server();
        let body = br#"{"other": "x"}"#.to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 400);
    }

    #[test]
    fn admin_blob_cache_flush_namespace_empty_string_returns_400() {
        let server = test_server();
        let body = br#"{"namespace": ""}"#.to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 400);
    }

    #[test]
    fn admin_blob_cache_flush_namespace_rejects_crlf_smuggling_attempt() {
        let server = test_server();
        // Classic CRLF smuggling shape — the namespace tries to splice
        // a fake audit line into structured logs.
        let body = br#"{"namespace": "real-ns\r\nfake-audit: spliced"}"#.to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        let msg = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(msg.contains("CR/LF"), "unexpected error: {msg}");
    }

    #[test]
    fn admin_blob_cache_flush_namespace_rejects_nul_byte() {
        let server = test_server();
        // JSON ` ` decodes to a literal NUL byte after parse;
        // the guard must reject it (NUL truncates downstream sinks
        // like proxies and log shippers). Build the body with a
        // string literal so the source file contains no raw NUL.
        let body = b"{\"namespace\": \"with-nul-\\u0000-here\"}".to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        let msg = parsed
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(msg.contains("NUL"), "unexpected error: {msg}");
    }

    #[test]
    fn admin_blob_cache_flush_namespace_response_round_trips_unicode() {
        let server = test_server();
        let body = r#"{"namespace": "日本語-ns-🦀"}"#.as_bytes().to_vec();
        let resp = server.handle_admin_blob_cache_flush_namespace(body);
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(
            parsed.get("namespace").and_then(|v| v.as_str()),
            Some("日本語-ns-🦀")
        );
    }

    // -------------------------------------------------------------------
    // Issue #195 — compare-and-set endpoint tests
    // -------------------------------------------------------------------

    fn cas_body(namespace: &str, key: &str, new_value: &[u8], new_version: u64) -> Vec<u8> {
        let b64 = {
            let mut s = String::new();
            for chunk in new_value.chunks(3) {
                const CHARS: &[u8] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let b0 = chunk[0] as u32;
                let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
                let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
                let n = (b0 << 16) | (b1 << 8) | b2;
                s.push(CHARS[((n >> 18) & 63) as usize] as char);
                s.push(CHARS[((n >> 12) & 63) as usize] as char);
                s.push(if chunk.len() > 1 {
                    CHARS[((n >> 6) & 63) as usize] as char
                } else {
                    '='
                });
                s.push(if chunk.len() > 2 {
                    CHARS[(n & 63) as usize] as char
                } else {
                    '='
                });
            }
            s
        };
        format!(
            r#"{{"namespace":"{namespace}","key":"{key}","expected_version":0,"new_value_b64":"{b64}","new_version":{new_version}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn cas_happy_first_write() {
        let server = test_server();
        let body = cas_body("ns1", "k1", b"hello", 1);
        let resp = server.handle_admin_blob_cache_compare_and_set(body);
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(
            parsed.get("committed").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            parsed.get("current_version").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn cas_happy_update_increments_version() {
        let server = test_server();
        // First write at version 1.
        server.handle_admin_blob_cache_compare_and_set(cas_body("ns2", "k2", b"v1", 1));
        // Update to version 2 — existing (1) < new_version (2) → ok.
        let resp = server.handle_admin_blob_cache_compare_and_set(cas_body("ns2", "k2", b"v2", 2));
        assert_eq!(resp.status, 200);
        let parsed = parse_body(&resp);
        assert_eq!(
            parsed.get("committed").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            parsed.get("current_version").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[test]
    fn cas_conflict_same_version_returns_409() {
        let server = test_server();
        // Write version 5.
        server.handle_admin_blob_cache_compare_and_set(cas_body("ns3", "k3", b"v1", 5));
        // Try to write version 5 again — existing (5) >= new_version (5) → conflict.
        let resp = server.handle_admin_blob_cache_compare_and_set(cas_body("ns3", "k3", b"v2", 5));
        assert_eq!(resp.status, 409);
        let parsed = parse_body(&resp);
        assert_eq!(
            parsed.get("committed").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            parsed.get("reason").and_then(|v| v.as_str()),
            Some("VersionMismatch")
        );
    }

    #[test]
    fn cas_stale_expected_version_returns_409() {
        let server = test_server();
        // Write version 10.
        server.handle_admin_blob_cache_compare_and_set(cas_body("ns4", "k4", b"v1", 10));
        // Try version 9 (going backwards) — existing (10) >= new_version (9) → conflict.
        let resp = server.handle_admin_blob_cache_compare_and_set(cas_body("ns4", "k4", b"v2", 9));
        assert_eq!(resp.status, 409);
        let parsed = parse_body(&resp);
        assert_eq!(
            parsed.get("current_version").and_then(|v| v.as_u64()),
            Some(10)
        );
    }

    #[test]
    fn cas_crlf_in_namespace_returns_400() {
        let server = test_server();
        // Embed CRLF via JSON unicode escapes.
        let body = b"{\"namespace\":\"real\\r\\ninjected\",\"key\":\"k\",\"expected_version\":0,\"new_value_b64\":\"aGk=\",\"new_version\":1}".to_vec();
        let resp = server.handle_admin_blob_cache_compare_and_set(body);
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        let msg = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(msg.contains("CR/LF"), "expected CR/LF error, got: {msg}");
    }

    #[test]
    fn cas_nul_in_key_returns_400() {
        let server = test_server();
        let body = b"{\"namespace\":\"ns\",\"key\":\"k\\u0000nul\",\"expected_version\":0,\"new_value_b64\":\"aGk=\",\"new_version\":1}".to_vec();
        let resp = server.handle_admin_blob_cache_compare_and_set(body);
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        let msg = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(msg.contains("NUL"), "expected NUL error, got: {msg}");
    }

    #[test]
    fn cas_bad_base64_returns_400() {
        let server = test_server();
        let body = br#"{"namespace":"ns","key":"k","expected_version":0,"new_value_b64":"!!!invalid!!!","new_version":1}"#.to_vec();
        let resp = server.handle_admin_blob_cache_compare_and_set(body);
        assert_eq!(resp.status, 400);
        let parsed = parse_body(&resp);
        let msg = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(msg.contains("base64"), "expected base64 error, got: {msg}");
    }

    #[test]
    fn cas_missing_bearer_returns_401_via_route() {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("RED_ADMIN_TOKEN").ok();
        unsafe {
            std::env::set_var("RED_ADMIN_TOKEN", "test-token-195");
        }

        let server = test_server();
        let req = crate::server::transport::HttpRequest {
            method: "POST".to_string(),
            path: "/admin/cache/compare-and-set".to_string(),
            query: std::collections::BTreeMap::new(),
            headers: std::collections::BTreeMap::new(),
            body: cas_body("ns", "k", b"v", 1),
        };
        let resp = server.route(req);
        assert_eq!(resp.status, 401);

        unsafe {
            match prev {
                Some(v) => std::env::set_var("RED_ADMIN_TOKEN", v),
                None => std::env::remove_var("RED_ADMIN_TOKEN"),
            }
        }
    }

    #[test]
    fn cas_wrong_bearer_returns_401_via_route() {
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("RED_ADMIN_TOKEN").ok();
        unsafe {
            std::env::set_var("RED_ADMIN_TOKEN", "correct-token");
        }

        let server = test_server();
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer wrong-token".to_string(),
        );
        let req = crate::server::transport::HttpRequest {
            method: "POST".to_string(),
            path: "/admin/cache/compare-and-set".to_string(),
            query: std::collections::BTreeMap::new(),
            headers,
            body: cas_body("ns", "k", b"v", 1),
        };
        let resp = server.route(req);
        assert_eq!(resp.status, 401);

        unsafe {
            match prev {
                Some(v) => std::env::set_var("RED_ADMIN_TOKEN", v),
                None => std::env::remove_var("RED_ADMIN_TOKEN"),
            }
        }
    }

    #[test]
    fn cas_concurrent_race_exactly_one_commits() {
        use std::sync::{Arc, Mutex};

        // RedDBServer may not be Sync, so we protect it with a Mutex and share
        // across threads. The BlobCache's check_version runs under a shard write
        // lock, so even serialised calls exercise the version-monotonicity guard.
        let server = Arc::new(Mutex::new(test_server()));
        let committed = Arc::new(Mutex::new(0u32));
        let conflicted = Arc::new(Mutex::new(0u32));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let server = Arc::clone(&server);
                let committed = Arc::clone(&committed);
                let conflicted = Arc::clone(&conflicted);
                std::thread::spawn(move || {
                    // All threads try to write version 1 to the same key.
                    let body = cas_body("race-ns", "race-key", b"payload", 1);
                    let resp = {
                        let s = server.lock().unwrap();
                        s.handle_admin_blob_cache_compare_and_set(body)
                    };
                    match resp.status {
                        200 => *committed.lock().unwrap() += 1,
                        409 => *conflicted.lock().unwrap() += 1,
                        s => panic!("unexpected status {s}"),
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        assert_eq!(
            *committed.lock().unwrap(),
            1,
            "exactly one CAS should commit (version 1 can only be written once)"
        );
    }

    // -------------------------------------------------------------------
    // Routing-layer auth gate: when RED_ADMIN_TOKEN is set the routes
    // must reject unauthenticated requests with 401. We exercise the
    // route() entrypoint, not the handler directly, so the gate (which
    // lives in is_authorized()) is on the path.
    // -------------------------------------------------------------------

    #[test]
    fn admin_blob_cache_routes_reject_unauth_when_admin_token_set() {
        // Serialize on a per-process mutex because RED_ADMIN_TOKEN is a
        // process-wide env var; running other admin-auth tests in
        // parallel would race the unset/set sequence.
        use std::sync::Mutex;
        static GUARD: Mutex<()> = Mutex::new(());
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("RED_ADMIN_TOKEN").ok();
        // SAFETY: env mutation is unsafe in 2024; we serialize via
        // GUARD above and restore the previous value at the end.
        unsafe {
            std::env::set_var("RED_ADMIN_TOKEN", "test-token-148");
        }

        let server = test_server();

        // Sweep without auth → 401.
        let req = crate::server::transport::HttpRequest {
            method: "POST".to_string(),
            path: "/admin/blob_cache/sweep".to_string(),
            query: std::collections::BTreeMap::new(),
            headers: std::collections::BTreeMap::new(),
            body: br#"{"limit_entries":1}"#.to_vec(),
        };
        let resp = server.route(req);
        assert_eq!(resp.status, 401, "sweep without admin token must be 401");

        // Flush namespace without auth → 401.
        let req = crate::server::transport::HttpRequest {
            method: "POST".to_string(),
            path: "/admin/blob_cache/flush_namespace".to_string(),
            query: std::collections::BTreeMap::new(),
            headers: std::collections::BTreeMap::new(),
            body: br#"{"namespace":"x"}"#.to_vec(),
        };
        let resp = server.route(req);
        assert_eq!(resp.status, 401, "flush without admin token must be 401");

        // With matching bearer → 200.
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer test-token-148".to_string(),
        );
        let req = crate::server::transport::HttpRequest {
            method: "POST".to_string(),
            path: "/admin/blob_cache/flush_namespace".to_string(),
            query: std::collections::BTreeMap::new(),
            headers,
            body: br#"{"namespace":"ok"}"#.to_vec(),
        };
        let resp = server.route(req);
        assert_eq!(resp.status, 200, "flush with admin token must be 200");

        // Restore previous env state.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("RED_ADMIN_TOKEN", v),
                None => std::env::remove_var("RED_ADMIN_TOKEN"),
            }
        }
    }
}
