# RedDB Logging Operator Guide

Practical reference for the four telemetry channels that RedDB writes to in
production. Pairs with:

- [`docs/operations/runbook.md`](runbook.md) — deployment playbook.
- [`docs/operations/replication.md`](replication.md) — replica bootstrap and resumability.
- [`docs/operations/blob-cache-dashboards.md`](blob-cache-dashboards.md) — Grafana dashboard reference.
- [`docs/operations/blob-cache-backup-restore.md`](blob-cache-backup-restore.md) — backup and restore procedures.
- [`.red/adr/0006-tiered-blob-cache.md`](../../.red/adr/0006-tiered-blob-cache.md) — tiered cache design rationale.
- [`.red/adr/0008-topology-advertisement-security.md`](../../.red/adr/0008-topology-advertisement-security.md) — auth and identity security model.

---

## 1. Three Telemetry Channels

RedDB splits observability output into three deliberately separate channels,
each with a different audience, volume profile, and durability requirement.

| Channel | Audience | Volume | Sink | Purpose |
|---------|----------|--------|------|---------|
| **Operator-grade events** | On-call, SRE, incident commander | Very low (tens/day) | Tamper-evident audit log + `red.log` | High-severity conditions that require human action. Never silent. |
| **Slow-query log** | DBA, performance engineer | Low-medium (sampled) | `red-slow.log` | Queries that exceeded the configured threshold; drives index and schema tuning. |
| **Admin intent journal** | DBA, incident commander, replication ops | Very low (~50 KB/day) | `red-admin-intent.log` | Control-plane recovery trail for long-running admin operations (e.g. replica bootstrap). |
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

### 2.3 Admin intent journal (`red-admin-intent.log`)

**Linux-first:** atomicity of concurrent writers relies on `O_APPEND` with POSIX-guaranteed atomic writes up to `PIPE_BUF`. On Linux this is 4096 bytes for regular files. On macOS `PIPE_BUF` is 512 bytes; multi-process concurrent writes are safe only if you run a single RedDB process per node (which is the standard deployment). All records are capped at 3 KiB to stay under the Linux limit.

| Attribute | Value |
|-----------|-------|
| **Location** | `<data_dir>/red-admin-intent.log` (same directory as `.audit.log`) |
| **Format** | NDJSON — one JSON object per line, UTF-8, no embedded newlines |
| **Rotation** | None by default. Rotate externally with `logrotate` + `copytruncate`. |
| **Retention** | Keep for at least 30 days; forensic value after incidents. |
| **Durability** | `fsync` on `begin` only. Checkpoint / complete / abort writes are buffered. A crash between `begin` and the terminal phase leaves a "dangling" record which `scan_and_report` surfaces at next startup. |
| **Expected volume** | ~50 KB/day under normal replication activity. |

#### Purpose

The admin intent journal is a control-plane recovery mechanism that complements the audit log (tamper-evident, high-severity events) and the slow-query log (query performance). Its audience is crash-recovery and replication ops: it records every long-running admin operation from `begin` through optional checkpoints to `complete` or `abort`, providing enough state to resume an interrupted operation after a crash.

It does **not** replace the audit log. The audit log is the source of truth for security and compliance events. The intent journal is a resumability aid — it may contain partial or aborted records that were never security-relevant.

#### JSON line schema

Every record is a single JSON object on one line. Fields:

| Field | Type | Always present | Description |
|-------|------|---------------|-------------|
| `id` | string (UUID v7) | Yes | Unique intent identifier. Stable across all records for the same operation. |
| `op` | string | Yes | Operation type. Currently: `replica_bootstrap`. |
| `phase` | string | Yes | Lifecycle phase: `running`, `checkpoint_N` (N ≥ 1), `completed`, `aborted`. |
| `ts` | number | Yes | Unix timestamp in milliseconds (UTC). |
| `actor` | string | Yes | Identity of the initiating process or user. |
| `args` | object | Yes | Operation-specific arguments. Sensitive keys (`password`, `secret`, `token`, `key`, `credential`, `auth`) are redacted to `***REDACTED***`. |
| `progress` | object | No | Attached to `checkpoint_N` records. Operation-specific progress state (e.g. `last_applied_lsn`, `batches_applied`). |
| `summary` | object | No | Attached to `completed` records. Operation-specific outcome (e.g. `total_records`, `duration_ms`). |

Example — full lifecycle for `replica_bootstrap`:

```json
{"id":"018f1a2b-3c4d-7e8f-9a0b-1c2d3e4f5a6b","op":"replica_bootstrap","phase":"running","ts":1746614400000,"actor":"replica-2","args":{"replica_id":"replica-2","source_lsn":4096,"target_lsn_hint":8192}}
{"id":"018f1a2b-3c4d-7e8f-9a0b-1c2d3e4f5a6b","op":"replica_bootstrap","phase":"checkpoint_1","ts":1746614410000,"actor":"replica-2","args":{"replica_id":"replica-2","source_lsn":4096,"target_lsn_hint":8192},"progress":{"last_applied_lsn":6144,"batches_applied":8}}
{"id":"018f1a2b-3c4d-7e8f-9a0b-1c2d3e4f5a6b","op":"replica_bootstrap","phase":"completed","ts":1746614420000,"actor":"replica-2","args":{"replica_id":"replica-2","source_lsn":4096,"target_lsn_hint":8192},"summary":{"total_records":1024,"duration_ms":20000}}
```

Example — intent that was never completed (crash between `begin` and `complete`):

```json
{"id":"018f1a2b-dead-7e8f-0000-000000000001","op":"replica_bootstrap","phase":"running","ts":1746614400000,"actor":"replica-3","args":{"replica_id":"replica-3","source_lsn":0,"target_lsn_hint":8192}}
```

#### Inspecting with `jq`

```sh
# List all begin records (phase=running)
jq 'select(.phase == "running") | {id, op, ts, actor}' red-admin-intent.log

# List all intents that never reached a terminal phase (dangling)
jq -s '
  group_by(.id) |
  map({
    id: .[0].id,
    op: .[0].op,
    last_phase: (map(.phase) | last)
  }) |
  map(select(.last_phase | test("running|checkpoint_")))[]
' red-admin-intent.log

# Count completed intents by operation type
jq 'select(.phase == "completed") | .op' red-admin-intent.log | sort | uniq -c | sort -rn

# Show all checkpoints for a specific intent id
jq --arg id "018f1a2b-3c4d-7e8f-9a0b-1c2d3e4f5a6b" \
  'select(.id == $id)' red-admin-intent.log
```

### 2.4 Developer log (`red.log` / stderr)

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
| `DanglingAdminIntent` | `operator/dangling_admin_intent` | An admin operation was started but never reached a terminal phase (completed / aborted). Emitted at startup by `AdminIntentLog::scan_and_report`. Severity: **forensic only** — no immediate action required unless the op was a destructive migration or schema change. Expected after a crash mid-bootstrap; verify the replica caught up cleanly before dismissing. |
| `ConfigChangeRequiresRestart` | `operator/config_change_requires_restart` | A config-file change was detected but one or more changed fields require a full server restart to take effect. The change was NOT applied. |

Every variant sets `"source": "system"` and `"outcome": "error"` in the audit
line. These cannot be overridden by caller input.

### 4.1 Config-Driven Routing (`OperatorEventRouter`)

By default every variant is dispatched to two handlers: `audit_log` (durable,
tamper-evident) and `tracing` (breadcrumb in `red.log`). An empty config is
equivalent to the historical hard-coded behaviour — zero upgrade burden.

Operators can opt in to webhook delivery for any subset of variants:

```toml
# config.toml (telemetry block)

# Define a PagerDuty handler — bearer token read from env at boot.
[telemetry.operator_event.routes.pagerduty]
url      = "https://events.pagerduty.com/v2/enqueue"
auth_env = "PAGERDUTY_INTEGRATION_KEY"
rate_limit = { requests = 60, window_sec = 60 }   # 1/sec sustained

# Route security-critical variants to audit + tracing + PagerDuty.
[telemetry.operator_event.routes.AuthBypass]
handlers = ["audit_log", "tracing", "pagerduty"]

[telemetry.operator_event.routes.WalFsyncFailed]
handlers = ["audit_log", "tracing", "pagerduty"]

# All other variants use the code default (audit_log + tracing).
```

#### Routing resolution order

1. Per-variant block (`routes.<VariantName>`) — most specific.
2. `routes.default` block — user-supplied default for all other variants.
3. Code default `["audit_log", "tracing"]` — applied when neither 1 nor 2 match.

#### Handler reference

| Handler name (config) | Description |
|-----------------------|-------------|
| `audit_log` | Writes to `<data_dir>/.audit.log`. Foundational — always available. |
| `tracing` | Emits a `tracing::warn!` breadcrumb to `red.log` / stderr. Foundational. |
| `stderr` | `eprintln!` fallback — useful as an additional sink in containerised environments. |
| `pagerduty` | HTTP POST to a PagerDuty Events API v2 endpoint. Requires `url` + `auth_env`. |
| `generic_webhook` | HTTP POST to any endpoint. Requires `url` + `auth_env`. |

#### Auth: bearer token via env var

```toml
[telemetry.operator_event.routes.pagerduty]
url      = "https://events.pagerduty.com/v2/enqueue"
auth_env = "PAGERDUTY_INTEGRATION_KEY"   # env var name, not the token itself
```

RedDB reads `auth_env` at boot and fails fast if the variable is not set.
The token is never written to logs or config files (12-factor compliance).

#### Rate limiting (per-handler token bucket)

```toml
rate_limit = { requests = 60, window_sec = 60 }
```

The token bucket refills at `requests / window_sec` tokens per second with burst
capacity equal to the sustained rate. When the bucket is empty the event is
dropped for that handler; other handlers on the same route continue to fire.
Metrics: `operator_event_dropped{handler="pagerduty",reason="rate_limit"}`.

#### Webhook failure mode

Each webhook handler runs a dedicated background thread with a bounded queue
(1 000 slots). Overflow drops the **oldest** entry
(`operator_event_dropped{reason="queue_full"}`). Each delivery attempt retries
up to 3 times with exponential backoff (200 ms → 400 ms). After 3 failures the
event is dropped (`operator_event_dropped{reason="max_retries"}`).
`emit()` never blocks — the push onto the in-process queue is O(1) and
synchronous.

#### Config validation at boot

The router validates every key in `routes` against the closed set of
`OperatorEvent` variant names. An unknown key causes a boot-time error with a
Levenshtein-based suggestion:

```
ERROR: unknown OperatorEvent variant 'AuthBypas'; did you mean 'AuthBypass'?
```

Handler names are also validated against the closed set:
`audit_log`, `tracing`, `stderr`, `pagerduty`, `generic_webhook`.

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
  [ADR 0008](../../.red/adr/0008-topology-advertisement-security.md). Any auth
  invariant violation surfaces here before it surfaces anywhere else.
- **Cache ADR** — [ADR 0006](../../.red/adr/0006-tiered-blob-cache.md) documents why
  the blob cache is derived state: it is not included in backups by default,
  and operator-grade cache events (e.g. namespace corruption) map to
  `SchemaCorruption` in the audit log.
- **Replication** — [`docs/operations/replication.md`](replication.md) covers
  replica bootstrap resumability and how the admin intent journal is used to
  detect and recover from interrupted bootstrap operations. A
  `DanglingAdminIntent` event with `op=replica_bootstrap` is the signal to
  consult that guide.
