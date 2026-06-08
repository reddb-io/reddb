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

/// Sanitize replica IDs for use as Prometheus label values.
/// Replaces double quotes, backslashes, and newlines so the resulting
/// metric line stays parseable. Operators picking aggressive replica
/// IDs is rare but malicious input must not break /metrics.
pub(crate) fn sanitize_label(value: &str) -> String {
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

    /// `POST /admin/drain` — flip to Draining phase. Subsequent
    /// `WriteGate`-checked writes will be rejected until shutdown
    /// completes or another phase override re-enables Ready.
    /// Idempotent.
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
            "transport_listeners".to_string(),
            self.transport_readiness_json(),
        );
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

    #[test]
    fn metrics_expose_replication_resync_alert_counters() {
        // Issue #839 — the full-resync counter must be scrapeable as an
        // alerting metric; the partial counter rides alongside for context.
        let runtime = crate::runtime::RedDBRuntime::with_options(
            crate::api::RedDBOptions::in_memory()
                .with_replication(crate::replication::ReplicationConfig::primary()),
        )
        .expect("runtime");

        let server = RedDBServer::new(runtime);
        let response = server.handle_metrics();
        let body = String::from_utf8(response.body).expect("utf8 metrics");

        for needle in [
            "# TYPE reddb_replication_full_resync_total counter",
            "reddb_replication_full_resync_total 0",
            "# TYPE reddb_replication_partial_resync_total counter",
            "reddb_replication_partial_resync_total 0",
        ] {
            assert!(
                body.contains(needle),
                "missing metric line for {needle}\n{body}"
            );
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

    // -------------------------------------------------------------------
    // Issue #752 — Red UI feature & capability discovery
    // -------------------------------------------------------------------

    fn cap_state<'a>(section: &'a JsonValue, key: &str) -> &'a str {
        section
            .get(key)
            .and_then(|c| c.get("state"))
            .and_then(JsonValue::as_str)
            .unwrap_or_else(|| panic!("missing capability state for `{key}`"))
    }

    #[test]
    fn capabilities_baseline_standalone_reports_supported_contract() {
        let server = test_server();
        let resp = server.handle_capabilities();
        assert_eq!(resp.status, 200);
        let body = parse_body(&resp);

        // Documented discovery contract is versioned.
        assert!(body
            .get("discovery_version")
            .and_then(JsonValue::as_u64)
            .is_some());
        assert!(body.get("version").and_then(JsonValue::as_str).is_some());

        // Replication baseline for an in-memory standalone runtime.
        let repl = body.get("replication").expect("replication");
        assert_eq!(
            repl.get("role").and_then(JsonValue::as_str),
            Some("standalone")
        );
        assert!(repl
            .get("commit_policy")
            .and_then(JsonValue::as_str)
            .is_some());

        // Vector/SIMD is always reported with a concrete level on a
        // supported platform.
        let simd = body
            .get("vector")
            .and_then(|v| v.get("simd"))
            .expect("simd");
        assert_eq!(
            simd.get("state").and_then(JsonValue::as_str),
            Some("supported")
        );
        assert!(simd.get("level").and_then(JsonValue::as_str).is_some());

        // Auth is disabled by default; methods reflect that.
        let auth = body.get("auth").expect("auth");
        assert_eq!(
            auth.get("enabled").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert_eq!(
            cap_state(auth.get("methods").expect("methods"), "bearer"),
            "disabled"
        );

        // Every advertised section the UI depends on is present.
        for section in [
            "build",
            "vector",
            "ai",
            "auth",
            "replication",
            "api_contracts",
            "preview_features",
        ] {
            assert!(body.get(section).is_some(), "missing section `{section}`");
        }

        // AI providers are enumerated.
        let providers = body
            .get("ai")
            .and_then(|a| a.get("providers"))
            .expect("providers");
        assert_eq!(cap_state(providers, "anthropic"), "supported");
    }

    #[test]
    fn capabilities_auth_enabled_marks_password_and_bearer_supported() {
        let runtime =
            crate::runtime::RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory())
                .expect("runtime");
        let auth = std::sync::Arc::new(crate::auth::store::AuthStore::new(
            crate::auth::AuthConfig {
                enabled: true,
                ..Default::default()
            },
        ));
        let server = RedDBServer::new(runtime).with_auth(auth);

        let resp = server.handle_capabilities();
        assert_eq!(resp.status, 200);
        let body = parse_body(&resp);

        let auth = body.get("auth").expect("auth");
        assert_eq!(auth.get("enabled").and_then(JsonValue::as_bool), Some(true));
        let methods = auth.get("methods").expect("methods");
        assert_eq!(cap_state(methods, "password"), "supported");
        assert_eq!(cap_state(methods, "bearer"), "supported");
        // mTLS / OAuth stay disabled — not configured in this fixture.
        assert_eq!(cap_state(methods, "mtls"), "disabled");
    }

    #[test]
    fn capabilities_failed_transport_is_unavailable_with_reason() {
        let runtime =
            crate::runtime::RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory())
                .expect("runtime");
        let mut options = crate::server::ServerOptions::default();
        options.transport_readiness = crate::service_cli::TransportReadiness {
            active: vec![crate::service_cli::TransportListenerState {
                transport: "http".to_string(),
                bind_addr: "127.0.0.1:5055".to_string(),
                explicit: false,
            }],
            failed: vec![crate::service_cli::TransportListenerFailure {
                transport: "grpc".to_string(),
                bind_addr: "127.0.0.1:50051".to_string(),
                explicit: true,
                reason: "address in use".to_string(),
            }],
        };
        let server = RedDBServer::with_options(runtime, options);

        let resp = server.handle_capabilities();
        assert_eq!(resp.status, 200);
        let body = parse_body(&resp);

        let contracts = body.get("api_contracts").expect("api_contracts");
        assert_eq!(cap_state(contracts, "http"), "supported");
        let grpc = contracts.get("grpc").expect("grpc contract");
        assert_eq!(
            grpc.get("state").and_then(JsonValue::as_str),
            Some("unavailable")
        );
        assert_eq!(
            grpc.get("reason").and_then(JsonValue::as_str),
            Some("address in use")
        );
    }

    #[test]
    fn auth_capabilities_anonymous_no_auth_grants_open_access() {
        let server = test_server();
        let headers = std::collections::BTreeMap::new();
        let resp = server.handle_auth_capabilities(&headers);
        assert_eq!(resp.status, 200);
        let body = parse_body(&resp);

        assert_eq!(
            body.get("auth_enabled").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert_eq!(
            body.get("authenticated").and_then(JsonValue::as_bool),
            Some(false)
        );
        // With auth disabled the server bypasses authorization entirely.
        let eff = body.get("effective").expect("effective");
        assert_eq!(cap_state(eff, "read"), "supported");
        assert_eq!(cap_state(eff, "write"), "supported");
    }

    #[test]
    fn auth_capabilities_enabled_anonymous_is_unauthenticated() {
        let runtime =
            crate::runtime::RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory())
                .expect("runtime");
        // require_auth defaults false → anon reads allowed, writes not.
        let auth = std::sync::Arc::new(crate::auth::store::AuthStore::new(
            crate::auth::AuthConfig {
                enabled: true,
                ..Default::default()
            },
        ));
        let server = RedDBServer::new(runtime).with_auth(auth);

        let headers = std::collections::BTreeMap::new();
        let resp = server.handle_auth_capabilities(&headers);
        assert_eq!(resp.status, 200);
        let body = parse_body(&resp);

        assert_eq!(
            body.get("auth_enabled").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            body.get("authenticated").and_then(JsonValue::as_bool),
            Some(false)
        );
        let eff = body.get("effective").expect("effective");
        assert_eq!(cap_state(eff, "read"), "supported");
        assert_eq!(cap_state(eff, "write"), "disabled");
        assert_eq!(cap_state(eff, "admin"), "disabled");
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
