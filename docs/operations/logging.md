# RedDB Logging Operator Guide

Practical reference for the three telemetry channels that RedDB writes to in
production. Pairs with:

- [`docs/operations/runbook.md`](runbook.md) — deployment playbook.
- [`docs/operations/blob-cache-dashboards.md`](blob-cache-dashboards.md) — Grafana dashboard reference.
- [`docs/operations/blob-cache-backup-restore.md`](blob-cache-backup-restore.md) — backup and restore procedures.
- [`docs/adr/0006-tiered-blob-cache.md`](../adr/0006-tiered-blob-cache.md) — tiered cache design rationale.
- [`docs/adr/0008-topology-advertisement-security.md`](../adr/0008-topology-advertisement-security.md) — auth and identity security model.

---

## 1. Three Telemetry Channels

RedDB splits observability output into three deliberately separate channels,
each with a different audience, volume profile, and durability requirement.

| Channel | Audience | Volume | Sink | Purpose |
|---------|----------|--------|------|---------|
| **Operator-grade events** | On-call, SRE, incident commander | Very low (tens/day) | Tamper-evident audit log + `red.log` | High-severity conditions that require human action. Never silent. |
| **Slow-query log** | DBA, performance engineer | Low-medium (sampled) | `red-slow.log` | Queries that exceeded the configured threshold; drives index and schema tuning. |
| **Developer signal** | Developer, debugger | High (can be noisy) | `red.log` / stderr | `DEBUG`/`INFO`/`TRACE` spans from `tracing`; useful in development, filtered down in production. |

### Why three channels, not one

A single log stream forces the operator to filter signal from noise under
pressure. During an incident the operator needs operator-grade events to surface
immediately and unconditionally. Slow-query lines would bury them in a
high-traffic deployment. Conversely, developer `DEBUG` traces are invaluable
during a schema migration but are harmful noise in an on-call log stream.

The separation also lets each channel have its own retention and rotation policy
without affecting the others, and makes it straightforward to ship each stream
to a different sink (Loki for the developer log, PagerDuty for operator events,
a time-series store for slow-query analysis).

PostgreSQL makes the same distinction: `log_min_messages` governs `postgresql.log`
(developer signal), `log_min_duration_statement` governs slow-query output, and
`pg_audit` is a separate extension for tamper-evident event capture.

---

## 2. Sinks

### 2.1 Audit log (operator-grade events)

| Attribute | Value |
|-----------|-------|
| **Location** | `<data_dir>/.audit.log` (configurable via `RED_AUDIT_LOG_PATH`) |
| **Format** | NDJSON — one JSON object per line, UTF-8, no embedded newlines |
| **Rotation** | None by default. Operators should rotate with `logrotate` or equivalent and send `SIGHUP` to reopen. |
| **Retention** | Keep indefinitely or per compliance policy; audit lines are tamper-evident and should not be auto-purged. |
| **Durability** | Synchronous write — each `emit()` call blocks until the line is on disk before returning. The background writer thread uses `O_APPEND` semantics. |

Each audit line contains:

```json
{
  "ts_ms":   1746614400000,
  "action":  "operator/replication_broken",
  "source":  "system",
  "outcome": "error",
  "detail":  { "peer": "replica-1", "reason": "TCP reset" }
}
```

Adversarial bytes (CRLF, NUL, `"`, `\`) in caller-controlled fields are
JSON-escaped at the serialization boundary (ADR 0010). A log-injection attack
cannot splice a fake audit line.

### 2.2 Slow-query log (`red-slow.log`)

| Attribute | Value |
|-----------|-------|
| **Location** | `<RED_LOG_DIR>/red-slow.log` |
| **Format** | NDJSON — one JSON object per line |
| **Rotation** | File is opened in append mode; rotate externally with `logrotate` + `copytruncate`. |
| **Retention** | Typically 30–90 days; can be shorter in high-traffic deployments. |
| **Durability** | Non-blocking — lines are queued to a background writer thread (65 536-entry buffer, lossy on overflow). A handful of slow-query lines may be dropped during extreme log bursts. |

Each slow-query line contains:

```json
{
  "ts_ms":       1746614401234,
  "kind":        "select",
  "duration_ms": 312,
  "sql":         "SELECT * FROM orders WHERE tenant_id = ?",
  "tenant":      "acme",
  "identity":    "api-service"
}
```

`sql` contains the statement text passed by the caller. Ensure secrets are
redacted by your application layer before passing SQL to RedDB.

### 2.3 Developer log (`red.log` / stderr)

| Attribute | Value |
|-----------|-------|
| **Location** | stderr (always) + `<RED_LOG_DIR>/red.log` (optional, when `RED_LOG_DIR` is set) |
| **Format** | `pretty` (TTY, coloured) or `json` (NDJSON) — controlled by `RED_LOG_FORMAT` |
| **Rotation** | Daily rotation via `tracing-appender`; old files are named `red.log.YYYY-MM-DD`. |
| **Retention** | Configurable via `RED_LOG_ROTATION_KEEP_DAYS` (default: 14). A background janitor thread removes files older than the threshold every hour. |
| **Durability** | Non-blocking — 1 000 000-entry buffer, lossy on overflow. Prefer the audit log for anything that must survive. |

---

## 3. Configuration Knobs

| Variable / Config Key | Default | Description |
|-----------------------|---------|-------------|
| `RUST_LOG` | `info` | `tracing`-style filter for the developer log. Example: `info,reddb::wire=debug`. Overrides `red.logging.level`. |
| `RED_LOG_FORMAT` | `pretty` | `pretty` (human-readable) or `json` (NDJSON for aggregators). |
| `RED_LOG_DIR` | *(none)* | Directory for rotating file sinks. Omit for stderr-only (CLI / embedded). |
| `RED_LOG_FILE_PREFIX` | `reddb.log` | Prefix for rotating log files. |
| `RED_LOG_ROTATION_KEEP_DAYS` | `14` | Days of rotated files to retain. Set `0` to disable auto-purge. |
| `RED_AUDIT_LOG_PATH` | `<data_dir>/.audit.log` | Override audit log path. |
| `slow_query_threshold_ms` | *(disabled)* | Emit a slow-query line for any query at or above this duration (ms). Set to `0` to log every query (not recommended in production). |
| `slow_query_sample_pct` | `100` | Percentage of above-threshold queries to emit, 0–100. Deterministic counter-based sampler; `100` = emit all. |

The `pretty` / `json` selector also applies to the rotating file sink — both
sinks use the same format so log-shipping agents do not need to handle mixed
formats from the same process.

---

## 4. OperatorEvent Variants

`OperatorEvent::emit()` is the only path that writes to the audit log from
inside RedDB. All 12 variants are listed below with their audit `action` string
and a description of when they fire.

| Variant | Audit action | When it fires |
|---------|-------------|---------------|
| `ReplicationBroken` | `operator/replication_broken` | A replication stream to a follower/replica dropped unexpectedly (TCP reset, auth failure, etc.). |
| `Divergence` | `operator/divergence` | The follower's committed LSN or data checksum disagrees with the leader's. Data safety may be compromised. |
| `WalFsyncFailed` | `operator/wal_fsync_failed` | An `fsync(2)` call on the WAL file returned an error. In-flight writes may not be durable. |
| `DiskSpaceCritical` | `operator/disk_space_critical` | Available disk space fell below the configured critical threshold. Writes will fail if not addressed. |
| `AuthBypass` | `operator/auth_bypass` | The auth gate returned `allow` for a request that should have been rejected — a security invariant violation. |
| `AdminCapabilityGranted` | `operator/admin_capability_granted` | An admin capability was granted to a principal at runtime. Audit trail for privilege escalation. |
| `SecretRotationFailed` | `operator/secret_rotation_failed` | A secret rotation attempt failed; the running instance may be holding a stale credential. |
| `ConfigChanged` | `operator/config_changed` | A runtime configuration key was changed on a live instance (e.g. slow-query threshold, readonly flag). |
| `StartupFailed` | `operator/startup_failed` | The server process failed to complete a startup phase (WAL recovery, schema load, etc.). |
| `ShutdownForced` | `operator/shutdown_forced` | The process was forcibly shut down (OOM killer, SIGKILL, unrecoverable internal error). |
| `SchemaCorruption` | `operator/schema_corruption` | On-disk schema metadata for a collection is corrupt or internally inconsistent. |
| `CheckpointFailed` | `operator/checkpoint_failed` | A scheduled or manually triggered checkpoint failed to complete. |

Every variant sets `"source": "system"` and `"outcome": "error"` in the audit
line. These cannot be overridden by caller input.

---

## 5. Slow-Query Sampling

The sampler uses a deterministic round-robin counter, not a random number
generator. This has two useful properties:

1. **Reproducibility** — given the same sequence of above-threshold queries the
   same subset is always emitted. Useful for regression testing.
2. **Exact rate** — over any window of ≥ 100 above-threshold queries the
   emitted fraction equals `sample_pct / 100` within ±1 query. No random
   outliers.

`sample_pct = 0` disables slow-query output entirely without shutting down the
logger. `sample_pct = 100` (default) emits every above-threshold query.

Below-threshold queries pay only one relaxed atomic load (`threshold_ms`) and
return immediately. No allocation, no mutex, no counter increment.

---

## 6. PostgreSQL Comparison

Operators familiar with PostgreSQL will find direct analogues for each RedDB
telemetry mechanism.

| PostgreSQL | RedDB equivalent | Notes |
|------------|-----------------|-------|
| `logging_collector = on` + `log_directory` | `RED_LOG_DIR` + `tracing-appender` daily rotation | Both use a background writer thread to avoid blocking query execution. |
| `log_filename` | `RED_LOG_FILE_PREFIX` | Prefix only; RedDB appends the date automatically. |
| `log_rotation_age` / `log_rotation_size` | Daily rotation (age only; no size trigger today) | PG supports both age and size; RedDB rotates daily. |
| `log_min_messages` | `RUST_LOG` | Same semantics: controls the minimum severity emitted to the developer log. |
| `log_min_duration_statement` | `slow_query_threshold_ms` | PG emits at or above; RedDB uses the same `>=` semantics. |
| `log_min_duration_sample` + `log_statement_sample_rate` | `slow_query_sample_pct` | PG uses a float rate (0.0–1.0); RedDB uses an integer percentage. |
| `pg_audit` extension | `OperatorEvent::emit` → audit log | Both produce tamper-evident structured records of high-severity events. |
| `log_autovacuum_min_duration` | `OperatorEvent::CheckpointFailed` | Both surface background-maintenance failures to the operator. |

Key difference: PG concentrates everything in one `postgresql.log` stream and
relies on `log_min_messages` to filter. RedDB uses three separate sinks so that
operator-grade events are never co-mingled with developer `DEBUG` output,
regardless of the level setting.

---

## 7. Cross-References

- **Backup and restore** — the audit log at `<data_dir>/.audit.log` is *not*
  included in the default backup archive (it is an append-only side-channel,
  not part of the database state). If compliance requires audit-log archival,
  copy it separately before taking a backup snapshot. See
  [`blob-cache-backup-restore.md`](blob-cache-backup-restore.md) for backup
  procedure details.
- **Dashboards** — the Grafana dashboards documented in
  [`blob-cache-dashboards.md`](blob-cache-dashboards.md) expose slow-query
  rate and p99 duration as panel metrics derived from the structured NDJSON in
  `red-slow.log`. The `slow_query_sample_pct` knob affects those panel values —
  lower sampling rates reduce dashboard fidelity.
- **Security model** — `OperatorEvent::AuthBypass` and
  `OperatorEvent::AdminCapabilityGranted` are the audit hooks for the
  threat model described in
  [ADR 0008](../adr/0008-topology-advertisement-security.md). Any auth
  invariant violation surfaces here before it surfaces anywhere else.
- **Cache ADR** — [ADR 0006](../adr/0006-tiered-blob-cache.md) documents why
  the blob cache is derived state: it is not included in backups by default,
  and operator-grade cache events (e.g. namespace corruption) map to
  `SchemaCorruption` in the audit log.
