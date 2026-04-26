# RedDB `/metrics` Specification

Public, versioned metric set exposed at `GET /metrics` in Prometheus / OpenMetrics text format (`text/plain; version=0.0.4`). PLAN.md Phase 5.1 + 5.4. Pairs with [`admin-api.openapi.yaml`](admin-api.openapi.yaml).

External tooling, dashboards, and alert rules can rely on the metric names + labels listed below — additive changes only within v1.x; removed metrics get one release of overlap.

## Lifecycle + health

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_uptime_seconds` | gauge | — | Wall-clock seconds since runtime construction. |
| `reddb_health_status` | gauge | — | `0` = down/starting, `1` = degraded/draining, `2` = ready. |
| `reddb_phase` | gauge (label-only) | `phase` | Always `1`; phase reported in label (`starting|ready|draining|shutting_down|stopped`). |
| `reddb_read_only` | gauge | — | `1` when public mutations are gated; `0` otherwise. |
| `reddb_replication_role` | gauge (label-only) | `role` | `standalone|primary|replica`, value always `1`. |
| `reddb_writer_lease_state` | gauge (label-only) | `state` | `not_required|held|not_held`. PLAN.md Phase 5.2 / W6. |
| `reddb_db_size_bytes` | gauge | — | On-disk size of the primary database file. |
| `reddb_cold_start_duration_seconds` | gauge | — | Seconds from process start to `/health/ready` 200. |
| `reddb_cold_start_phase_seconds` | gauge | `phase` | Per-phase breakdown (`restore|wal_replay|index_warmup|total`). PLAN.md Phase 9.1. |

## Operator-imposed limits (Phase 4.1)

Emitted only when the operator pinned a cap via `RED_MAX_*`.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_limit_db_size_bytes` | gauge | — | Cap on primary DB file size. |
| `reddb_limit_connections` | gauge | — | Cap on concurrent client connections. |
| `reddb_limit_qps` | gauge | — | Sustained per-instance QPS cap. |
| `reddb_limit_batch_size` | gauge | — | Cap on rows per bulk DML batch. |
| `reddb_limit_memory_bytes` | gauge | — | Soft memory budget. |

## Backup + WAL archive (Phase 5.1)

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_backup_age_seconds` | gauge | — | Seconds since last successful backup. **Alert when growing**. |
| `reddb_backup_last_success_timestamp_seconds` | gauge | — | Unix seconds of the most recent successful backup. |
| `reddb_backup_last_duration_seconds` | gauge | — | Wall-clock duration of the most recent backup. |
| `reddb_backup_failures_total` | counter | — | Backup failures since process start. |
| `reddb_backup_total_total` | counter | — | Successful backups since process start. |
| `reddb_wal_current_lsn` | gauge | — | Current local LSN. |
| `reddb_wal_last_archived_lsn` | gauge | — | LSN of the most recently archived WAL segment. |
| `reddb_wal_archive_lag_records` | gauge | — | `current_lsn - last_archived_lsn`. **Alert when growing**. |

## Replication (Phase 11.4)

Per-replica metrics on the primary; absent on replicas / standalone. Replicas registered but not yet acked still appear (so dashboards detect "registered but stuck").

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_replica_count` | gauge | — | Currently registered replicas. |
| `reddb_replica_ack_lsn` | gauge | `replica_id` | Most recent LSN acked. |
| `reddb_replica_lag_records` | gauge | `replica_id` | `current_lsn - last_acked_lsn`. |
| `reddb_replica_lag_seconds` | gauge | `replica_id` | Wall-clock seconds since the replica was last seen. |
| `reddb_slo_lag_budget_remaining_seconds` | gauge | `replica_id` | `RED_SLO_REPLICA_LAG_BUDGET_SECONDS` (default 60) minus `reddb_replica_lag_seconds`; negative means SLO breach. |
| `reddb_replica_apply_errors_total` | counter | `kind` | `gap|divergence|apply|decode`. **`divergence > 0` = page operator immediately**. |
| `reddb_replica_apply_health` | gauge (label-only) | `state` | Current apply state (`ok|healthy|connecting|stalled_gap|divergence|apply_error`). |
| `reddb_primary_commit_policy` | gauge (label-only) | `policy` | `local|remote_wal|ack_n|quorum`. |
| `reddb_commit_wait_total` | counter | `outcome` | `reached|timed_out|not_required`. |
| `reddb_commit_wait_last_seconds` | gauge | — | Wall-clock seconds of the most recent commit wait. |

## Per-caller quotas (Phase 4.4)

Emitted only when at least one caller has been throttled (`RED_MAX_QPS_PER_CALLER` set + bucket exhausted).

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `reddb_quota_rejected_total` | counter | `principal` | Requests denied by the per-caller token bucket. `principal` is `bearer:<sha256-prefix>`, `replica:<id>`, or `anon`. |

## Suggested alert thresholds

Tune to your SLA; these are starting points used by `red doctor` defaults:

| Metric | Warn | Critical | Reason |
|--------|------|----------|--------|
| `reddb_health_status < 2` | 5m | 15m | Engine isn't ready. |
| `reddb_writer_lease_state{state="not_held"} == 1` | immediate | immediate | Split-brain risk if role is primary. |
| `reddb_backup_age_seconds` | 600 | 3600 | DR posture degraded. |
| `reddb_wal_archive_lag_records` | 1000 | 10000 | Archive stuck. |
| `reddb_replica_lag_records` | 1000 | 100000 | Replica too far behind to be promoted. |
| `reddb_slo_lag_budget_remaining_seconds < 0` | immediate | 5m | Replica lag exhausted the operator's SLO budget. |
| `reddb_replica_apply_errors_total{kind="divergence"} > 0` | immediate | immediate | Corruption / split-brain. |
| `reddb_commit_wait_total{outcome="timed_out"} rising` | 10/min | 100/min | `ack_n` policy too tight or replicas can't keep up. |
| `reddb_quota_rejected_total{principal=...} sustained` | 10/min | 100/min | Caller exceeded budget. |
| `reddb_cold_start_duration_seconds > 2.0` | 1 occurrence | 3 occurrences | Cold start past target. |

## Stability promise

All metric names + labels listed above are **stable across patch + minor releases of the engine**. New metrics land additively; renames or removals get one release of overlap with the old name. Major engine version bumps (incompatible spec) re-version this document.

## Out of scope

These are not yet emitted; documented here so the contract is clear:

- `reddb_ops_total{op}` — per-operation counter. Hot-path instrumentation needed.
- `reddb_query_duration_seconds_bucket` — histogram. Needs histogram framework or external aggregation.
- `reddb_active_connections` — needs connection-pool wiring through the surface gates.
- `reddb_bytes_egressed_total` — billing-ready counter; needs response-size hook.
- `reddb_restore_progress_ratio` — gauge while restore is in flight.

PLAN.md Phase 5.1 lists these as v1.x follow-ups.
