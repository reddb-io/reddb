# ADR 0060 - Operational telemetry substrate

Status: accepted
Date: 2026-06-22

Resolves issue #1247 (PRD #1237, Operational Telemetry for red-ui, Phase 0).
Extends [ADR 0017](0017-prometheus-grafana-adapters-for-metrics.md) (Prometheus/
Grafana adapters are boundary adapters, not the source of truth) and
[ADR 0041](0041-operational-collection-layouts.md) (append-only segments +
retention for event-shaped data). Authorized via HITL (2026-06-22) to draft the
Phase-0 contract with reasonable maintainer-review defaults.

This ADR is a **design contract**, not enforcement code. It names the
operational-telemetry **store / read-model boundary** that the downstream metric
slices (#1238–#1245) implement against. No metric implementations land in #1247.

## Context

red-ui renders RedDB's topology, cluster status, and analytics surfaces directly
from the server HTTP contract. Per the #738 honesty rule, any field the engine
cannot measure is returned as an `{ "available": false, "reason": "…" }`
envelope rather than fabricated. That is correct, but several operator panels
today show honest "not exposed" chips where an operator most wants a number.

The fix is to grow the *measurable* set, and to do so against a single durable
substrate rather than letting each new metric invent its own storage, retention,
and label set. Without a substrate contract first, the seven follow-up slices
would each be free to add unbounded labels and unbounded history through the
back door of an HTTP route or a `/metrics` series. This ADR fixes the substrate
so they cannot.

The first-class product contract is: **RedDB records bounded, retention-managed
operational telemetry internally; HTTP routes, `/metrics`, `/cluster/status`,
and the red-ui read model are consumers of that substrate, not the source of
truth.**

Some telemetry already exists in shapes this ADR must subsume:
`telemetry/slow_query_logger.rs` writes `red-slow.log` JSONL (`ts_ms`, `kind`,
`duration_ms`, `sql`, `tenant`, `identity`) — file-only, and `tenant`/`identity`
are currently stored in plaintext; `/cluster/status` already serves measured
fields (replication LSNs, cold-start phases, db size) alongside `unavailable`
envelopes (`throughput`, `latency`, `last_error`, `system.cpu_usage`,
`system.ram_usage`, `wal.bytes`). The substrate this ADR defines is the durable
home those producers write into and those surfaces read from.

## Decision

### 1. Four layers, one direction

The substrate is split into four distinct layers. Data flows in one direction;
no layer may be skipped and no consumer may become a producer's source of truth.

```
measurement  →  durable storage  →  rollups  →  export surfaces
  (hot path)     (substrate)        (substrate)   (consumers)
```

- **Measurement** — cheap, hot-path capture (counter increment, duration sample,
  event record). Measurement code knows the data class and the dimension values;
  it does not know retention, rollup, or export shape.
- **Durable storage** — the substrate proper. Bounded append-only collections
  (ADR 0041 layout) plus current-snapshot cells. Owns retention, cardinality
  enforcement, and redaction-at-write.
- **Rollups** — periodic compaction of raw samples into coarser time buckets,
  governed by the same retention contract. Rollups are derived data, never an
  authority for raw history.
- **Export surfaces** — `/metrics`, `/cluster/status`, and the red-ui read model.
  **Consumers only.** They read the substrate and shape it for a wire format;
  they neither store nor fabricate. An export surface that has nothing to read
  emits an unavailable envelope (see §6), never a default.

### 2. Data classes

Three first-class classes. Every operational metric is exactly one of these.

- **Numeric samples**
  - *Counters* — monotonic, reset-aware (e.g. `http_requests_total`,
    `replication_apply_bytes_total`). Stored as cumulative value + reset epoch.
  - *Gauges* — point-in-time values that rise and fall (e.g. `db_size_bytes`,
    in-flight connections, `cpu_usage`).
  - *Histograms* — classic `le`-bucketed distributions for latency
    (`query_duration_seconds_bucket{kind,le}`); fixed bucket schema per metric,
    declared once, never per-sample.
  - *Node samples* — per-node occupancy gauges (CPU, RAM, active queries,
    connection churn) keyed by `node_id`.
- **Operational events** — discrete, timestamped, individually interesting
  records: slow queries, primary↔replica reconnects, last errors. Stored in
  append-only event segments with TTL retention.
- **Current snapshots** — the latest honest value of each field `/cluster/status`
  serves (phase, uptime, replication position, storage size, and — as phases
  land — throughput/latency/system gauges). A snapshot is a single overwritten
  cell, not history; history lives in the sample/event classes.

### 3. Retention and rollup policy

Every class has a default TTL and a configurable hard cap. **Bounded by
construction**: no future metric may add unbounded retention; it inherits its
class's policy and may only tighten within the cap.

| Class | Raw retention (default) | Rollup | Rolled-up retention | Hard cap (default) |
|---|---|---|---|---|
| Numeric samples | 24h at full resolution | 1m → 5m → 1h buckets | 30 days at 1h | per-series row cap; oldest evicted |
| Operational events | 7 days | none (events are not rolled up) | — | per-class ring (e.g. last 10k slow queries) |
| Current snapshots | latest only | n/a | n/a | one cell per field |

- TTLs and caps are configurable under a `telemetry.*` config block; defaults
  above apply when unset.
- Eviction is **deterministic and bounded** — when a cap is hit, the oldest
  raw samples / events are dropped (and that drop is itself observable via a
  substrate-internal "dropped" counter; silent truncation is forbidden).
- Rollups are produced by a janitor pass (cf. `telemetry/janitor.rs`), are
  idempotent, and never extend raw retention.

### 4. Cardinality budgets and normalization

Each dimension has a **fixed allowed value set or an explicit budget**. A
measurement carrying a value outside the budget is folded to a reserved
`__overflow__` bucket — never admitted as a new series. This is what makes
"future metrics cannot add unbounded labels" enforceable rather than aspirational.

| Dimension | Normalization rule | Budget |
|---|---|---|
| `route` | template, never raw path (`/collections/{name}`, not `/collections/users`) | closed set from the router table |
| `query_kind` | closed enum (select/insert/update/delete/bulk/aggregate/ddl/internal) | 8 (matches `QueryKind`) |
| `node_id` | stable node identity | cluster member count |
| `status` | HTTP status **class** preferred (`2xx`/`4xx`/`5xx`); exact code only where justified | ≤ ~60 |
| `replica_id` | stable replica identity | replica count |
| `tenant` | **hashed** (see §5), not raw | budgeted; overflow → `__overflow__` |
| `identity` | **hashed** (see §5), not raw | budgeted; overflow → `__overflow__` |

- Total series per metric is the product of its dimension budgets and is capped;
  exceeding the cap drops the new series to `__overflow__` and increments the
  drop counter rather than growing storage.
- A new metric must declare its dimension set and inherits these rules; it may
  not introduce a free-form (unbounded) label.

### 5. Privacy and redaction (redaction-at-write)

Redaction happens **at the storage boundary**, before durability — never relying
on an exporter to redact on the way out.

- **Slow-query SQL** is stored as a **normalized fingerprint/shape, not raw
  text**: literals and parameters collapsed to placeholders, whitespace/case
  normalized (e.g. `SELECT * FROM t WHERE id = ?`). The raw statement is never
  durably stored by the substrate. (The existing logger takes a pre-redacted
  `sql_redacted` string; this ADR makes fingerprinting the contract, and the
  follow-up slice tightens the producer to emit a fingerprint, not free text.)
- **Tenant and identity are hashed** (stable keyed hash) before storage, so
  telemetry can group-by and rate-limit per tenant/identity without storing the
  raw principal. *This supersedes the current plaintext `tenant`/`identity`
  fields in `red-slow.log`; that producer is migrated to the hashed form by the
  slow-query slice (#1238-class work).*
- **Future error payloads** are treated as redactable: a stored `last_error`
  carries a bounded, message-shape-only string (code + redacted message), never
  raw row data or unbounded backtraces.
- The keyed hash secret is process/deployment-scoped config; rotating it rotates
  the grouping namespace (acceptable for operational telemetry).

### 6. Honesty rule (#738) is preserved end-to-end

- A field with **no real sample** stays an **unavailable envelope**
  (`{ "available": false, "reason": "…" }`); the substrate never materializes a
  zero, a default, or an interpolated value to fill a gap.
- Counters reading zero because *nothing happened* are distinct from
  *unmeasured*: a registered counter at 0 is honest data; an unregistered /
  not-yet-instrumented field is an unavailable envelope.
- Rollups of empty windows are absent, not zero-filled.
- This rule is the substrate's invariant, inherited by every export surface;
  `/cluster/status` fields flip from envelope to number only when a real sample
  exists and flip back if measurement is lost.

### 7. Exporter read contracts

All three consumers read the **same substrate**; they differ only in shape and
window.

- **`/metrics` (Prometheus/OpenMetrics)** — the process/export view. Reads
  current counter/gauge values and histogram buckets, renders OpenMetrics text.
  Per ADR 0017 this is a boundary adapter; it owns no storage. Unmeasured series
  are simply absent (Prometheus has no envelope concept).
- **`/cluster/status`** — the current honest snapshot view. Reads the
  current-snapshot cells; measured fields render as numbers, unmeasured fields
  render as unavailable envelopes (§6).
- **red-ui read model** — a stable read model over the substrate for: recent
  events (slow queries, reconnects, last errors, with `limit`/`since_ms`/
  filter params), time windows (rolled-up sample series for charts), and current
  snapshots (the same cells `/cluster/status` reads). red-ui never reads raw
  producer files (e.g. `red-slow.log`) directly — it reads the substrate's read
  model so retention, redaction, and cardinality rules apply uniformly.

## Considered options

- **Substrate-first contract (chosen).** Define the store/read-model boundary,
  data classes, retention, cardinality, and redaction once; downstream slices
  implement against it. Costs an upfront design doc; prevents seven independent,
  divergent storage/label decisions.
- **Per-metric ad-hoc storage.** Each slice picks its own storage and labels.
  Rejected: no bounded-history or bounded-cardinality guarantee, redaction
  applied inconsistently (the existing plaintext-tenant slow log is exactly this
  failure mode), and red-ui ends up reading heterogeneous surfaces.
- **`/metrics` as the source of truth.** Treat Prometheus scrape state as the
  store. Rejected: contradicts ADR 0017 (adapters are boundary, not authority),
  loses durability across restarts, and has no envelope concept so it cannot
  preserve #738.
- **Raw retention only, no rollups.** Rejected: bounded raw retention forces a
  choice between short history and large storage; rollups give long, cheap
  trends within the same bounded-by-construction contract.
- **Redact on export.** Rely on each exporter to strip SQL/tenant/identity.
  Rejected: raw sensitive data would be durably stored and one un-redacted
  exporter leaks it. Redaction-at-write makes leakage structurally impossible.

## Consequences

- #1238–#1245 implement (slow-query events, HTTP counters, latency histograms,
  replication throughput, reconnect counter, per-node occupancy) as producers
  into this substrate and consumers out of it — not as bespoke stores.
- The existing slow-query logger is migrated: SQL → fingerprint, tenant/identity
  → hashed, and a substrate-backed read model replaces direct `red-slow.log`
  reads for the `GET /slow-queries` surface.
- A `telemetry.*` config block gains TTL/cap/sample knobs with the defaults in
  §3; unset means defaults.
- New metrics must declare their data class, dimension set (within §4 budgets),
  and inherit class retention; review can reject a metric that adds an unbounded
  label or unbounded history by pointing at this ADR.
- Export surfaces stay thin: shaping only, no storage, no fabrication. The #738
  honesty rule is enforced once at the substrate and inherited by all consumers.
- Cardinality/retention overflow is observable (drop counters), so silent loss
  of telemetry is itself surfaced as telemetry.
