# PRD: Operational Telemetry for red-ui

GitHub: https://github.com/reddb-io/reddb/issues/1237
Related: docs/spec/metrics.md, crates/reddb-server/src/presentation/cluster_status_json.rs (#738), crates/reddb-server/src/telemetry/slow_query_logger.rs, issues/prd/reddb-metrics-backend-v0.md

## Problem Statement

red-ui renders RedDB's topology, cluster status, and (new) analytics surfaces
directly from the server's HTTP contract. Per the #738 honesty rule, any field
the engine cannot measure is returned as `{ "available": false, "reason": "…" }`
rather than fabricated — which is correct, but it means several operator-facing
panels currently render honest "not exposed" chips where an operator most wants
a number.

Auditing what red-ui can show today against what operators ask for surfaces a
concrete gap list. Some telemetry already exists but is not reachable over HTTP
(the slow-query log is written to a file only); some is partially exposed
(per-collection storage); and some is genuinely net-new (per-request latency,
HTTP status counters, replication throughput, per-node occupancy).

This PRD scopes the telemetry RedDB should expose so red-ui can replace its
"not exposed" envelopes with live data, ordered by leverage versus cost. It is
deliberately scoped to *operator/infrastructure* telemetry and does not touch
the customer-facing metrics backend (see `reddb-metrics-backend-v0.md`).

## What exists today (verified)

- **Slow-query capture** — `telemetry/slow_query_logger.rs` writes `red-slow.log`
  as JSONL (`ts_ms`, `kind`, `duration_ms`, redacted `sql`, `tenant`, `identity`)
  with a configurable threshold and sampling. **File-only — no HTTP read path.**
- **Replication lag** — `/cluster/status` per-replica `last_acked_lsn` /
  `last_sent_lsn` / `lag_records` / `last_seen_at_unix_ms`; `/replication/status`
  per-replica `lag_lsn` + `lag_seconds`; Prometheus `reddb_replica_lag_*`,
  `reddb_replica_apply_errors_total{kind}`, `reddb_replica_apply_health`.
- **Cold-start** — `reddb_cold_start_duration_seconds` /
  `reddb_cold_start_phase_seconds{phase}`, plus `/cluster/status`
  `started_at_unix_ms` / `ready_at_unix_ms` / `uptime_secs` / `phase`.
- **Storage size** — `/cluster/status` `storage.db_size_bytes`,
  `reddb_db_size_bytes`. Per-collection `on_disk_bytes` exists partially in the
  `red.collections` projection.
- **Honest gaps in `/cluster/status`** — `throughput`, `latency`, `last_error`,
  `system.cpu_usage`, `system.ram_usage`, `wal.bytes` are returned as
  `unavailable` envelopes today.

## Product Goal

Let an operator open red-ui and read, live: how slow the slowest queries are,
how much traffic each node is taking, how fast replication is moving (not just
how far behind it is), and how loaded each node is — without attaching an
external Prometheus/Grafana stack. Every number red-ui shows must be one the
engine can honestly measure; this PRD grows that measurable set.

## Phased Delivery

### Phase A — Quick wins (no hot-path redesign)

- **Slow-query HTTP read.** Add `GET /slow-queries` returning the most recent N
  entries the slow-query logger already produces (params: `limit`, `since_ms`,
  `min_duration_ms`, `kind`). The data already exists; this is a read surface
  over the existing JSONL. Unblocks an entire red-ui panel.
- **HTTP request counters.** `reddb_http_requests_total{method, route, status}`
  incremented in the HTTP layer. Replaces the `/cluster/status` `throughput`
  unavailable envelope's "requests" dimension and gives per-route error rates.
- **Per-collection size.** Complete `red.collections.on_disk_bytes` for every
  collection so red-ui can break down `storage.db_size_bytes`.

### Phase B — Hot-path instrumentation

- **Per-request latency histogram.** `reddb_query_duration_seconds_bucket`
  (classic histogram, `le` buckets) on the query hot-path, keyed by `kind`
  (select/insert/update/delete/ddl). Feeds `/cluster/status` `latency` and a
  P50/P95/P99 panel via the existing `histogram_quantile` path.
- **Replication throughput.** Bytes/sec and records/sec applied, instrumented at
  WAL apply: `reddb_replication_apply_bytes_total`,
  `reddb_replication_apply_records_total`. Distinguishes a replica that is
  behind-but-catching-up from one that is stalled (today only lag is visible).
- **Reconnect counter.** A primary↔replica reconnect counter to contextualise
  `last_error` (today reconnect frequency is not tracked).

### Phase C — Per-node occupancy (single deployment)

- **CPU / RAM sampling.** Replace the `system.cpu_usage` / `system.ram_usage`
  unavailable envelopes with a sampled value where the platform supports it,
  keeping the honest envelope where it does not (e.g. non-Linux).
- **Per-node load.** Active query count and connection churn per node so red-ui
  can show "which node is hot".

### Phase D — Cluster GA (deferred until multi-writer is GA)

- Range/hash shard ownership and key distribution per node (the cluster topology
  red-ui has a placeholder for today).
- Intra-cluster RPC/gossip metrics. Note: the current architecture is
  primary-replica with no Raft in the data path, so these are net-new and gated
  on the clustering work in `.red/context/clustering.md`.

## API and Metric Surface

- `GET /slow-queries?limit&since_ms&min_duration_ms&kind` → JSON array of the
  existing slow-query records.
- `/cluster/status` `throughput`, `latency`, `system.cpu_usage`,
  `system.ram_usage` move from `unavailable` envelopes to measured values as the
  phases land; fields stay as honest envelopes until then (no fabrication).
- New Prometheus series: `reddb_http_requests_total{method,route,status}`,
  `reddb_query_duration_seconds_bucket{kind,le}`,
  `reddb_replication_apply_bytes_total`, `reddb_replication_apply_records_total`,
  `reddb_replication_reconnects_total`.

## Testing Strategy

- Unit-test the slow-query read endpoint's filtering (limit/since/min/kind) over
  synthetic log fixtures, no filesystem coupling in the pure layer.
- Contract-test that `/cluster/status` fields flip from `unavailable` to a
  number only when a real sample exists, and remain honest envelopes otherwise
  (mirrors the existing `cluster_status_json` contract tests).
- Histogram correctness: `histogram_quantile` over the new buckets returns
  sane P50/P95/P99 on a known distribution.
- Replication throughput counters are monotonic and reset-aware under WAL apply.

## Non-goals

- The customer-facing metrics backend, Prometheus remote-write, or PromQL
  surface — owned by `reddb-metrics-backend-v0.md`.
- Fabricating any value the engine cannot measure; the honesty envelope stays.
- Multi-writer cluster sharding telemetry beyond a placeholder until that engine
  work is GA.
- A bundled dashboards product; red-ui is the first consumer, Grafana stays
  supported via the metrics backend PRD.

## Acceptance Criteria

- A durable PRD artifact exists describing the operational-telemetry gaps red-ui
  surfaces and the phased plan to close them.
- Each phase maps to independently shippable issues with externally observable
  acceptance (an endpoint or a metric series), not internal refactors.
- Phase A items are validated against red-ui replacing its corresponding
  "not exposed" chips with live data.
