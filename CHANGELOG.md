# Changelog

All notable changes to RedDB are documented here. RedDB is **pre-1.0**;
the public surface (HTTP, gRPC, RedWire, PG wire, embedded API, file
format) may change at any minor version. Major changes are called out
explicitly.

Dates are ISO-8601 (UTC-3).

---

## [Unreleased]

### Documentation

- `docs/security/vault.md` rewritten as a full operator reference for
  the encrypted vault: threat model, key hierarchy, bootstrap (CLI +
  HTTP), restart precedence, `*_FILE` companion variables, Docker /
  Kubernetes / cloud secret-manager patterns, risk ranking, rotation
  procedure, and recovery from a lost or leaked certificate.
- `docs/operations/secrets.md` (new) — full secret inventory, storage
  options, rotation matrix, incident response, DR procedures, audit /
  compliance mapping, and anti-pattern catalog.
- `docs/getting-started/docker.md` (new) — quickstart, production-secure
  Docker pattern using vault + Docker secrets, multi-replica setup,
  image variants, signal handling.
- Driver READMEs (14 drivers) — added a `Production deploy` section
  pointing at the vault and Docker guides.

---

## [0.2.4] — 2026-04-26

### Added

- **Architectural deepening (6 clusters):**
  - `service_router::ProtocolDetector` trait + composable `Router` —
    adding a new protocol becomes a new struct, not edits to a switch.
  - `runtime::lease_lifecycle::LeaseLifecycle` — single owner of the
    writer-lease state machine.
  - `storage::backend::AtomicRemoteBackend` trait — splits CAS contract
    off `RemoteBackend`. `LeaseStore::new` requires
    `Arc<dyn AtomicRemoteBackend>`. Turso and D1 cannot be wired into a
    writer lease; the type system rejects them. `AtomicHttpBackend`
    refuses construction unless `RED_HTTP_CONDITIONAL_WRITES=true`.
  - `Pager` owns page-level encryption directly via
    `PagerConfig::encryption: Option<SecureKey>` with transparent
    `read_page_decrypted` / `write_page_encrypted` and a fail-closed
    marker matrix (encrypted-vs-plain × key-supplied-vs-absent).
    `EncryptedPager` collapses to a `#[deprecated]` thin wrapper.
  - `server::transport::run_use_case` + `map_runtime_error` — single
    source of truth for `RedDBError → HTTP status`.
  - `application::OperationContext` + sealed `WriteConsent` token.
    `WriteGate::check_consent` is the only mint site outside the gate
    module.
- **Backend conditional-write contract for writer leases:**
  - Local FS uses content-hash tokens.
  - S3-compatible stores use ETag + `If-Match`.
  - Generic HTTP backend opts in with
    `RED_HTTP_CONDITIONAL_WRITES=true`.
- **RedWire wire protocol** — full operational surface wired
  end-to-end (per-frame zstd compression, TLS / mTLS dispatch via
  ALPN, prepared statements, streaming bulk inserts, SCRAM-SHA-256
  in the handshake, OAuth / OIDC JWT in the handshake). Spec:
  [`docs/adr/0001-redwire-tcp-protocol.md`](docs/adr/0001-redwire-tcp-protocol.md).
- **SCRAM-SHA-256 end-to-end** — RedWire + PG wire + user vault.
  Stored credential format:
  `SCRAM-SHA-256$<iter>:<salt>:<stored-key>:<server-key>`.
- **OAuth / OIDC JWT** — pluggable `JwtVerifier` validates `iss`,
  `aud`, `exp`, `nbf` and maps `preferred_username` (default) onto a
  RedDB identity. Same code path serves HTTP, gRPC, and RedWire.
- **HMAC-signed requests** — timestamp + nonce + canonical request
  signing with replay protection. Headers: `X-RedDB-Key-Id`,
  `X-RedDB-Timestamp`, `X-RedDB-Nonce`, `X-RedDB-Signature`.
- **`*_FILE` secrets convention** — every sensitive env var
  (`RED_ADMIN_TOKEN`, `RED_S3_SECRET_KEY`, `RED_BACKEND_HTTP_AUTH`,
  `RED_TURSO_TOKEN`, `RED_D1_TOKEN`, `REDDB_CERTIFICATE`,
  `REDDB_USERNAME`, `REDDB_PASSWORD`, …) honours an `*_FILE`
  companion that wins over the inline value.
- **Live secret rotation via SIGHUP** — sending SIGHUP reloads every
  `*_FILE` companion in place.
- **`CommitPolicy`** (`src/replication/commit_policy.rs`):
  `Local | RemoteWal | AckN | Quorum`, set via
  `RED_PRIMARY_COMMIT_POLICY` (or per-request in bulk RPCs).
- **`CommitWaiter`** — writer surface waits on per-replica durable
  LSN before acking the client.
- **`AckReplicaLsn` gRPC** — replicas durably-ack their applied LSN
  to the primary; per-replica state visible in `/admin/replicas`.
- **`LogicalChangeApplier`** — typed errors `Gap`, `Divergence`,
  `Apply`, `Decode`. Replicas in divergence refuse promotion.
- **HTTP + gRPC commit-policy enforcement** in DML, bulk, and graph
  paths.
- **`RED_LEASE_REQUIRED=true`** — fail-closed boot when the chosen
  backend cannot enforce conditional writes.
- **Auto-restore from remote on cold boot** when `RED_AUTO_RESTORE=true`.
- **Cloud-agnostic backend selection** via `RED_BACKEND` (`s3`, `fs`,
  `http`, `turso`, `d1`, `none`).
- **Hot-path quota enforcement** — `RED_MAX_QPS_PER_CALLER` token
  bucket keyed by `bearer:<sha256-prefix>` / `replica:<id>` / `anon`.
- **`ResourceLimits`** from `RED_MAX_*` env vars, surfaced in
  `/metrics` (`reddb_limit_*`) and `/admin/status`.
- **`/metrics` Prometheus** + **`/admin/status` JSON** snapshots.
- **`ServerSurface` enum** (`Public | AdminOnly | MetricsOnly`) —
  operators can pin admin and metrics to dedicated listeners.
- **`Dockerfile.musl`** — static-binary container image
  (`release-static` profile, `panic = "abort"`).
- **Reference deployment manifests** for AWS ECS Fargate, App Runner,
  Lambda+EFS (read replica), Azure Container Apps, Cloudflare
  Containers, Fly Machines, Google Cloud Run, HashiCorp Nomad, and
  Kubernetes (StatefulSet + PVC).
- **`reddb_slo_lag_budget_remaining_seconds{replica_id}`** metric.
- **`reddb_replica_apply_health{state}`** — per-state gauge for
  `ok|connecting|stalled_gap|divergence|apply_error`.
- **`reddb_primary_commit_policy{policy}`** + `reddb_commit_wait_*`
  counters.
- **`reddb_quota_rejected_total{principal}`** for per-caller throttling.
- **`reddb_cold_start_phase_seconds{phase}`** — `restore`,
  `wal_replay`, `index_warmup`, `total`.
- **OpenTelemetry config scaffold** behind `--features otel`.
- **`red doctor`** — probes `/metrics` + `/admin/status` against
  operator-tunable thresholds, exits `0|1|2`.
- Release gates: cold-start baselines, artifact size measurement,
  feature-matrix compilation, nightly backup/restore drills.
- CI publish dry-run on every PR (`cargo package` for engine + rust
  client).

### Changed

- Serverless writer fencing fails closed when the configured backend
  cannot enforce CAS — no more last-writer-wins on writer leases.
- Production release binaries use `panic = "abort"`; WAL/recovery is
  the consistency boundary after process death.
- `Cargo.toml` `include` patterns anchored with leading `/` so
  `README.md` / `LICENSE` no longer sweep up vendored `node_modules` /
  `dolt` trees. Package shrinks from 1026 → 638 files.
- Engine version bumped 0.1.5 → 0.2.4 to align with crates.io's 0.2.x
  line. `drivers/rust` engine dep follows.

### Fixed

- `drivers/rust/src/embedded.rs` adapted to the engine's `Arc<str>`
  schema-key types — embedded mode compiles again.
- WAL hash chain validation on PITR restore aborts with a typed
  `chain` error on a break.

### Documentation

- New: `docs/release/v1.0-migration.md`,
  `docs/reference/features.md`, `bench/artifact-sizes.md`,
  `docs/release/drill-history.md`.
- Operator runbook updated with lease backend/runtime matrix, panic
  policy, and SLO lag budget alert.

---

## 2026-04-23 — REST surface rewrite: RESTful + collection-centric

Dropped the `/vcs/*` RPC-shaped endpoints and replaced them with a
properly RESTful, collection-centric layout. **Breaking change for
HTTP consumers** — there is no compatibility shim.

### Design

- **Nouns, not verbs**: `/repo/commits`, `/repo/refs/heads/{name}`,
  no `/vcs/checkout` or `/vcs/merge` as top-level paths.
- **HTTP semantics respected**: GET for reads, POST creates, PUT
  moves refs, DELETE deletes. `201 Created`, `204 No Content`,
  `404 Not Found`, `409 Conflict` mapped from `RedDBError`.
- **Session-scoped state transitions**: checkout/merge/reset/
  cherry-pick/revert live under `/repo/sessions/{conn}/*`.
- **Collection-centric opt-in**: `/collections/{name}/vcs` owns the
  versioned toggle.
- **Nested conflicts**: `/repo/merges/{msid}/conflicts/{cid}/resolve`.
- **Consistent JSON envelope**: `{ ok, result }` / `{ ok, error }`.

### New surface (20 endpoints)

```text
GET    /repo                                     repo summary
GET    /repo/refs[?prefix=…]                     unified ref listing
GET    /repo/refs/heads                          branch list
POST   /repo/refs/heads                          create
GET    /repo/refs/heads/{name}                   show
PUT    /repo/refs/heads/{name}                   move ref
DELETE /repo/refs/heads/{name}                   delete
GET    /repo/refs/tags
POST   /repo/refs/tags
GET    /repo/refs/tags/{name}
DELETE /repo/refs/tags/{name}
GET    /repo/commits?branch=&limit=&…            log
POST   /repo/commits                             create (session workset)
GET    /repo/commits/{hash}                      show
GET    /repo/commits/{a}/diff/{b}                diff
GET    /repo/commits/{a}/lca/{b}                 LCA
GET    /repo/sessions/{conn}                     status
POST   /repo/sessions/{conn}/checkout
POST   /repo/sessions/{conn}/merge
POST   /repo/sessions/{conn}/reset
POST   /repo/sessions/{conn}/cherry-pick
POST   /repo/sessions/{conn}/revert
GET    /repo/merges/{msid}                       merge-state summary
GET    /repo/merges/{msid}/conflicts             list
POST   /repo/merges/{msid}/conflicts/{cid}/resolve
GET    /collections/{name}/vcs                   opt-in state
PUT    /collections/{name}/vcs                   toggle { versioned }
```

`POST /repo/sessions/{conn}/{cherry-pick|revert}` exposed cherry-pick
and revert, previously runtime-only.

---

## 2026-04-23 — Git for Data: opt-in per collection (Phase 7)

User collections now stay outside VCS by default; each one explicitly
opts in via `vcs.set_versioned(name, true)` (library), `POST
/vcs/versioned` (REST), `red vcs versioned on <name>` (CLI), or
`ALTER TABLE <name> SET VERSIONED = true|false`.

- Default is non-versioned: a fresh collection does not participate in
  merge / diff / AS OF.
- `vcs_diff` / `vcs_merge` / cherry-pick / revert only scan opted-in
  collections.
- `AS OF` on an unversioned table raises a typed error. Internal
  `red_*` collections always accept `AS OF` because they're
  append-only.
- `red_*` collections cannot be opted in; the `red_vcs_settings`
  writer refuses them explicitly.

---

## 2026-04-23 — Git for Data (VCS layer over MVCC)

First-class version control on top of the MVCC engine. Every mutation
runs under MVCC snapshots; the VCS layer pins those snapshots, hashes
them, and exposes git-style semantics (commit / branch / tag / checkout
/ merge / cherry-pick / revert / reset / log / status / diff / LCA /
`AS OF` time-travel) across CLI, REST, and SQL.

### New collections (seven internal `red_*`)

- `red_commits` — commit entities
- `red_refs` — branches, tags, per-connection `HEAD:<conn>` pointers
- `red_worksets` — per-connection working / staged state
- `red_closure` — commit ancestry index for fast LCA
- `red_conflicts` — shadow docs for unresolved merge conflicts
- `red_merge_state` — in-progress merge / cherry-pick / revert
- `red_remotes` — remote repository configuration (placeholder)

### MVCC extension

- `SnapshotManager::pin(xid)` / `unpin(xid)` / `is_pinned(xid)` /
  `pin_count(xid)` — reference-counted.
- `prune_aborted` skips pinned xids so historical row versions survive
  VACUUM as long as a commit references them.

### SQL: `AS OF` time-travel

```sql
SELECT * FROM users AS OF COMMIT '<hash>' WHERE age > 21;
SELECT * FROM users AS OF BRANCH 'staging' LIMIT 5;
SELECT * FROM orders AS OF TAG 'v1.0' WHERE total > 100;
SELECT * FROM events AS OF TIMESTAMP 1710000000000;
SELECT * FROM t AS OF SNAPSHOT 42;
```

Commit hash:
`SHA-256("reddb-commit-v1" || root_xid || sorted_parents || author || message || timestamp_ms)`.

Standalone 3-way JSON merge (`application::merge_json`) is reusable and
covered by 15 unit tests.

---

## 2026-04-22 — AI-first SQL surface + hypertable pipeline

Multi-sprint push to make the AI-first multi-model pitch defensible
from a SQL session. Every item below is callable without touching the
Rust API.

### AI / ML scalars

- `ML_CLASSIFY(model, features)` /
  `ML_PREDICT_PROBA(model, features)` — evaluate a registered
  classifier.
- `MODEL_REGISTER(name, kind, weights_json [, hyperparams, metrics])`
  / `MODEL_DROP(name)` — lifecycle for pre-trained weights.
- `EMBED(text [, provider])` — call the AI provider stack; returns
  `Vector`.
- `SEMANTIC_CACHE_GET(ns, embedding)` /
  `SEMANTIC_CACHE_PUT(...)` — cosine-similarity LLM response cache.
- `LIST_MODELS()` / `SHOW_MODELS()`.

### Hypertables

- `CREATE HYPERTABLE name TIME_COLUMN col CHUNK_INTERVAL 'dur' [TTL 'dur']`.
- `DROP HYPERTABLE name`.
- INSERT-time chunk routing.
- `LIST_HYPERTABLES()` / `SHOW_HYPERTABLES()`.
- `HYPERTABLE_PRUNE_CHUNKS(name, lo_ns, hi_ns)`.

### Continuous aggregates

- `CA_REGISTER(name, source, bucket_dur, alias, agg, field [, lag, max_interval])`.
- `CA_REFRESH(name [, now_ns])`.
- `CA_QUERY(name, bucket_start_ns, alias)`.
- `CA_STATE(name)` / `CA_LIST()` / `CA_DROP(name)`.

### Schema

- `CREATE TABLE t(...) WITH (append_only = true)` parses — the
  parenthesised form `WITH (k = v, k = v)` works everywhere the
  legacy `WITH k = v` shorthand does.

---

## 2026-04-22 — Performance & Stability

### Wire / ingest

- **Streaming bulk wire protocol** — Postgres `COPY`-equivalent
  columnar stream over a persistent connection. ~3× `typed_insert` on
  10k-row batches.
- **Columnar pre-validated insert path** — skips the N×ncols `String`
  clones the legacy bulk path paid.
- **Wire encode** — column indices are resolved once per result set
  and the output buffer is reused across rows.

### CDC

- **Split CDC lock** — concurrent CDC observers no longer serialise on
  a single mutex.

### WAL / durability

- **Lock-free append queue** for `WalDurableGrouped` mode.
- **Batched bulk inserts** — one WAL action per bulk op.
- **Phase C busy-spin deadlock fix** under tokio preemption.
- **Truncate invariant** — the append queue's LSN cursor resets
  alongside the WAL on checkpoint truncate.

### B-tree

- **Right-sibling hop** on sorted bulk insert.

### Dependencies

- Rust 1.95.0 toolchain.
- `tonic` 0.14, `prost` 0.14.
- `ureq` 3.3 with `rustls`.
- `hmac` 0.13, `sha2` 0.11, `lz4_flex` 0.13, `roaring` 0.11,
  `rayon` 1.12, `rcgen` 0.14, `criterion` 0.8, `pprof` 0.15.

---

## 2026-04-22 — TimescaleDB + ClickHouse Parity Push

Foundations for competing directly with TimescaleDB (time-series /
log workloads) and ClickHouse (columnar OLAP).

### Time-series / logs

- **Hypertables**, **continuous aggregates**, **retention daemon**.
- **Partition TTL** via `HypertableSpec::with_ttl("90d")`.
- **Log pipeline** — `LogPipeline` bundles hypertable + retention +
  tail ring buffer; `LogLine` with severity + labels + numeric fields.

### Compression

- **T64 bit-packing** for narrow-range integer columns.
- **zstd per-chunk fallback** with leading-marker passthrough.
- **`select_int_codec`** heuristic (DeltaOfDelta / T64 / Raw).
- **Per-column codec pipeline** — `ColumnCodec` enum (None, Lz4, Zstd,
  Delta, DoubleDelta, Dict).

### Execution / OLAP

- **ColumnBatch + operators** — typed `ColumnVector`
  (Int64/Float64/Bool/Text + validity), 2048-row vectors.
- **SIMD reducers** — AVX2 sum / min / max / filter for f64 and i64
  with scalar fallbacks.
- **Parallel reducers** — rayon-driven `parallel_sum_f64` +
  `parallel_aggregate` with partial group-by merge.

### Planner primitives

- **Partition pruning** — RANGE / LIST / HASH pruner with
  `PrunePredicate` AST.
- **Projections** — ClickHouse-style pre-aggregation matcher.

### Aggregates

- **T-Digest primitive**.
- **ClickHouse-parity aggregators** — Uniq (HLL-backed),
  QuantileTDigest, Covariance, CountIf, SumAvgIf, Any, AnyLast,
  GroupArray, `arrayJoin` helper.

### Schema

- **APPEND ONLY tables** (`CREATE TABLE ... APPEND ONLY`) —
  catalog flag; UPDATE / DELETE rejected at parse time before RLS.

---

## 2026-04-17 — Performance Parity Push (Phases 0-4 + P6.T1)

### Phase 0 — Operational scaffolding

- `src/runtime/config_matrix.rs` — single source of truth for
  durability / concurrency / storage keys, with **Tier A** (critical,
  self-heals on boot) and **Tier B** (optional, in-memory defaults).
- `src/runtime/config_overlay.rs` — precedence env → file →
  `red_config` → default.
- `Dockerfile` gains `/etc/reddb` volume + `REDDB_CONFIG_FILE`.

### Phase 1 — Per-collection locking

- `src/runtime/locking.rs` — `Resource::{Global, Collection(name)}`
  with intent-lock dispatch:
  - Reads → `(Global, IS) → (Collection, IS)`
  - Writes → `(Global, IX) → (Collection, IX)`
  - DDL → `(Global, IX) → (Collection, X)`

### Phase 2 — Durability default flipped

- `DurabilityMode::default()` swapped from `Strict` to
  `WalDurableGrouped`.
- `DurabilityMode::from_str` accepts `"sync"` → `WalDurableGrouped`,
  `"strict"` → `Strict`.

### Phase 3 — HOT decision helper + UPDATE fast path

- `src/storage/engine/hot_update.rs` — pure decision helper mirroring
  PG's `heap_update`.
- HOT skips the `index_entity_update` call.

### Phase 4 — Fused bulk-insert index maintenance

- `IndexStore::index_entity_insert_batch` — one registry-lock
  acquisition for the whole batch.

### Phase 6.T1 — Background writer

- `PageCache::flush_some_dirty` + `dirty_count()`.
- `Pager::flush_some_dirty` / `dirty_fraction()`.
- `bgwriter::PagerDirtyFlusher` — `Weak<Pager>` so the bg thread
  exits cleanly on database drop.

79 new / extended integration tests across 15 suites — all green.

---

## 2026-04-17 — Structured Logging with `tracing` + File Rotation

- `reddb::telemetry::{init, TelemetryConfig, LogFormat,
  TelemetryGuard}` — façade over `tracing-subscriber`.
- Background janitor purges rotated files older than `--log-keep-days`.
- Spans: `query_span`, `connection_span`, `listener_span` —
  carry `conn_id`, `tenant`, `transport`, `bind`, `peer`, `err`.

CLI flags on `red server`:

| Flag | Default | Purpose |
|------|---------|---------|
| `--log-dir <path>` | `<--path parent>/logs` | Directory for rotating log files |
| `--log-level <level>` | `info` | trace/debug/info/warn/error or `RUST_LOG` |
| `--log-format <fmt>` | `pretty` on TTY, `json` otherwise | `pretty` \| `json` |
| `--log-keep-days <N>` | `14` | Retention count for rotated files |
| `--no-log-file` | off | stderr-only mode |

Adds `tracing = "0.1"`, `tracing-subscriber = "0.3"`,
`tracing-appender = "0.2"`. ~80 KB stripped.

---

## 2026-04-17 — RedDB-Native Cross-Model Extensions

Extends the morning's PG-parity bundle across graph, vector, queue,
timeseries so the RedDB idiom feels unified rather than table-centric.

- **MVCC universal** — one `BEGIN` spans tables + graphs + vectors +
  queues + timeseries atomically.
- **ASK tenant-scoped** — RLS in the RAG pipeline. Restrictive
  default: RLS enabled + zero matching policy ⇒ deny.
- **Tenancy dotted paths** — `TENANT BY (root.sub.path)` on
  `CREATE TABLE` and `ALTER TABLE … ENABLE TENANCY ON (root.sub)`.
- **RLS universal per entity kind** — `CREATE POLICY ... ON
  NODES|EDGES|VECTORS|MESSAGES|POINTS|DOCUMENTS OF <collection>`.
- **Views end-to-end + stacked filter merge** — `CREATE OR REPLACE
  VIEW`, MATERIALIZED VIEW + REFRESH, plan + result cache invalidated
  on view DDL.

---

## 2026-04-17 — PostgreSQL Feature Parity (19 categories)

One bundled release closing every category on the PG parity matrix.

- **Transactions & MVCC** — `BEGIN` / `COMMIT` / `ROLLBACK`,
  `SAVEPOINT`, sub-xid stack, tuple-level `xmin` / `xmax`.
- **Security** — `CREATE POLICY` with USING predicate per
  `(table, role, action)`; `ALTER TABLE ENABLE / DISABLE ROW LEVEL
  SECURITY`; mTLS (CN / SAN / OID roles); OAuth / OIDC validator.
- **Multi-tenancy** — `SET TENANT`, `CURRENT_TENANT()`,
  `TENANT BY (col)` declarative, retrofit via `ALTER TABLE`.
- **Views** — `CREATE VIEW` / `OR REPLACE` / `DROP`; materialized
  views + `REFRESH POLICY`.
- **Partitioning** — `PARTITION BY RANGE|LIST|HASH (col)`,
  `ATTACH` / `DETACH`.
- **Replication** — `QuorumCoordinator` (`Async`, `Sync`,
  `Regions(N)`), multi-region replica binding, LSN-watermark
  consistent reads.
- **Backup & Recovery** — `red dump`, `red restore`, `red pitr-list`,
  `red pitr-restore --target-time`.
- **Foreign Data Wrappers** — `ForeignDataWrapper` trait, built-in
  CSV wrapper, `CREATE SERVER` / `CREATE FOREIGN TABLE`.
- **Network** — PostgreSQL v3 wire protocol (startup, simple query,
  SSL `N` rejection), Unix socket transport.
- **DDL & Maintenance** — `VACUUM`, `ANALYZE`, schemas, sequences
  with `nextval()` / `currval()`.
- **JSON & Imports** — `json_extract`, `json_set`,
  `json_array_length`, `json_path_query`, `COPY ... FROM file.csv`.
- **Platforms** — Windows + macOS CI matrix, `#[cfg(windows)]` gates.

---

## 2026-04-16 — WAL Durability & BRIN Timeseries

- WAL-backed writes — INSERT / UPDATE / DELETE hit the log before the
  pager.
- Configurable fsync policy (`every_commit`, `every_n`, `interval`).
- Boot-time replay of unflushed WAL frames.
- BRIN-equivalent block-range zone maps for time-series segments.
- Race-free reads under concurrent writers.

### Storage correctness

- Metadata persistence in binary file format (`STORE_VERSION_V7`).
- Race-free sealed-segment deletes + table-scoped result cache.
- Arc-wrapped prepared statements — no per-call re-plan.
- Centralised sealed→growing rewrite path.
- CDC + context index gated on confirmed deletion (eliminated
  double-emit).

### Query performance

- Unified mutation engine: single kernel for single-row / micro-batch
  / bulk INSERT.
- Slot-indexed aggregates.
- Covered queries: skip heap fetch when projection ⊆ indexed column.

---

## Earlier — Foundational Data Structures & Query Extensions

### New data structures (10 total)

- **Bloom Filter** — per-segment probabilistic key testing,
  auto-populated on insert.
- **Hash Index** — O(1) exact-match via `CREATE INDEX ... USING HASH`.
- **Bitmap Index** — Roaring bitmap via `CREATE INDEX ... USING BITMAP`.
- **R-Tree Spatial Index** — radius / bbox / nearest-K geo queries.
- **Skip List + Memtable** — write buffer in `GrowingSegment`, sorted
  drain on seal.
- **HyperLogLog** — approximate distinct counting.
- **Count-Min Sketch** — frequency estimation.
- **Cuckoo Filter** — membership testing with deletion.
- **Time-Series** — chunked storage, delta-of-delta timestamps,
  Gorilla XOR compression, retention policies.
- **Queue / Deque** — FIFO/LIFO/Priority message queue with consumer
  groups.

### Query language extensions

- `CREATE INDEX [UNIQUE] name ON table (cols) USING HASH|BTREE|BITMAP|RTREE`
- `SEARCH SPATIAL RADIUS|BBOX|NEAREST ...`
- `CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]`
- `CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]`
- `QUEUE PUSH|POP|PEEK|LEN|PURGE|GROUP CREATE|READ|ACK|NACK`
- `CREATE/DROP HLL|SKETCH|FILTER` + `HLL ADD/COUNT/MERGE` +
  `SKETCH ADD/COUNT` + `FILTER ADD/CHECK/DELETE`
- JSON inline literals: `{key: value}` without quotes in VALUES and
  QUEUE PUSH

### Dependencies

- Added `roaring = "0.10"` (Bitmap Index)
- Added `rstar = "0.12"` (R-Tree Spatial Index)

### Initial release prep

- Multi-model embedded / server / serverless documentation and
  packaging pipeline.
- Unified crate publishing workflow for crates.io.
