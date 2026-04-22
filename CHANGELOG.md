# Changelog

All notable changes to RedDB are documented here. Dates are ISO-8601 (UTC-3).

## 2026-04-22 — TimescaleDB + ClickHouse Parity Push

Adds the foundations for competing directly with TimescaleDB (in
time-series / log workloads) and ClickHouse (in columnar OLAP).
Every item below ships as a library + unit tests; SQL parser wiring
for the DDL surfaces lands alongside the corresponding feature in a
follow-on sprint. See `/.claude/plans/eu-recebi-esta-review-serene-glade.md`
for the full plan.

### Time-series / logs

- **Hypertables** (`src/storage/timeseries/hypertable.rs`) — chunk
  auto-routing by timestamp, `show_chunks`, `drop_chunks_before`.
- **Continuous aggregates** (`src/storage/timeseries/continuous_aggregate.rs`) —
  incremental refresh driven by a `last_refreshed_bucket` watermark;
  respects `refresh_lag` + `max_interval_per_job`.
- **Retention daemon** (`src/storage/timeseries/retention.rs`) —
  cooperative sweeper, backend-agnostic trait, observable stats.
- **Partition TTL** (new) — `HypertableSpec::with_ttl("90d")` +
  `HypertableRegistry::sweep_expired` + per-chunk overrides +
  preview via `chunks_expiring_within`. Docs: `docs/data-models/partition-ttl.md`.
- **Log pipeline** (`src/storage/timeseries/log_pipeline.rs`) —
  `LogPipeline` bundles hypertable + retention + tail ring buffer;
  `LogLine` struct with severity + labels + numeric fields + trace
  IDs; `ingest_batch` / `tail_since` / `set_partition_ttl`.
- **Log docs** (new) — `docs/guides/using-reddb-for-logs.md` (full
  guide, ~400 lines) + `docs/guides/logs-quickstart.md`.

### Compression

- **T64 bit-packing** — integer codec for narrow-range columns.
- **zstd per-chunk fallback** with leading-marker passthrough for
  tiny inputs.
- **`select_int_codec` heuristic** — picks DeltaOfDelta / T64 / Raw
  by shape.
- **Per-column codec pipeline** (`src/storage/unified/segment_codec.rs`) —
  ColumnCodec enum (None / Lz4 / Zstd / Delta / DoubleDelta / Dict),
  chain header serialisable. Adds `lz4_flex` (pure-Rust) dep.

### Execution / OLAP

- **ColumnBatch + operators** (`src/storage/query/batch/`) — typed
  `ColumnVector` (Int64/Float64/Bool/Text + validity), filter /
  project / aggregate batch operators, 2048-row vectors.
- **SIMD reducers** (`src/storage/query/batch/simd.rs`) — AVX2
  sum_f64 / sum_i64 / min_f64 / max_f64 / filter_gt_f64 with scalar
  fallbacks, runtime-detected.
- **Parallel reducers** (`src/storage/query/batch/parallel.rs`) —
  rayon-driven `parallel_sum_f64` + `parallel_aggregate` with partial
  group-by merge.

### Planner primitives

- **Partition pruning** (`src/storage/query/planner/partition_pruning.rs`) —
  RANGE / LIST / HASH pruner with `PrunePredicate` AST, AND
  tightening + OR widening + FNV hash + conservative fallback.
- **Projections** (`src/storage/query/planner/projections.rs`) —
  ClickHouse-style pre-aggregation matcher; picks narrowest-fit
  projection; filter-signature compatibility check.

### Aggregates

- **T-Digest primitive** (`src/storage/primitives/tdigest.rs`) with
  merging variant + compact loop.
- **ClickHouse-parity aggregators** (`src/storage/query/engine/aggregates_extra.rs`) —
  Uniq (HLL-backed), QuantileTDigest, Covariance (Welford + parallel
  merge for corr / covar_pop / covar_samp), CountIf, SumAvgIf, Any,
  AnyLast, GroupArray, `arrayJoin` helper.

### Schema

- **APPEND ONLY tables** (`CREATE TABLE ... APPEND ONLY`) — catalog
  flag on `CollectionContract`; UPDATE / DELETE rejected at parse
  time before RLS. Docs: `docs/data-models/append-only-tables.md`.

### Docs

- New data-model pages: `hypertables.md`, `continuous-aggregates.md`,
  `append-only-tables.md`, `partition-ttl.md`.
- New architecture pages: `competitive-positioning.md`,
  `distributed-roadmap.md`.
- New engine page: `columnar-execution.md`.
- New guides: `using-reddb-for-logs.md`, `logs-quickstart.md`.
- Updated: `docs/_sidebar.md`, `docs/data-models/overview.md`,
  `docs/data-models/tables.md`, `docs/data-models/timeseries.md`,
  `docs/reference/limitations.md`, `README.md`.

## 2026-04-17 — Performance Parity Push (Phases 0-4 + P6.T1)

Reduces the measured gap vs. the reference row-store row-store on
`benches/bench_definitive_dual.py`. Spec + plan:
`docs/spec-performance-parity-2026-04-17.md`, `tasks/plan.md`.

### Phase 0 — Operational scaffolding

- `src/runtime/config_matrix.rs` — single source of truth for
  durability / concurrency / storage keys, with a two-tier contract:
  **Tier A** (critical) self-heals on boot (writes the default into
  `red_config` when absent); **Tier B** (optional) uses in-memory
  defaults, only appearing in `SHOW CONFIG` after a user write.
- `src/runtime/config_overlay.rs` — precedence env → file →
  `red_config` → default. `REDDB_<UP_DOTTED_KEY>` env vars are
  re-read every boot and never persist; `/etc/reddb/config.json`
  overlay is write-if-absent so a later `SET CONFIG` always wins.
- `Dockerfile` gains `/etc/reddb` volume + `REDDB_CONFIG_FILE`
  default. Single image, opinionated by default.
- `docs/engine/perf-bench.md` — reproduction guide + tuning surface.
- `SET CONFIG` / `SHOW CONFIG` now lowercase dotted keys so keyword
  segments (`MODE`, `SIZE`) don't mismatch the matrix.

### Phase 1 — Per-collection locking

- `src/runtime/locking.rs` — `Resource::{Global, Collection(name)}`
  + `LockerGuard` RAII wrapper over the pre-existing (but unused)
  `storage::transaction::lock::LockManager`. Ordered acquire +
  reverse-order release; illegal escalations return a typed error.
- Dispatch integrates a single mode-picker
  (`intent_lock_modes_for`):
  - Reads → `(Global, IS) → (Collection, IS)`
  - Writes → `(Global, IX) → (Collection, IX)`
  - DDL → `(Global, IX) → (Collection, X)`
  - Admin / control → no lock
- 4-test concurrent-writes suite proves 20 threads × 200 inserts
  across 5 collections complete without serialisation. DDL suite
  proves `CREATE TABLE`/`ALTER TABLE` take X mode and that a DDL on
  collection `a` doesn't block writers on `b`.

### Phase 2 — Durability default flipped

- `DurabilityMode::default()` swapped from `Strict` (per-commit
  fsync) to `WalDurableGrouped` (batched sync; writers wait for
  durability but fsyncs coalesce across concurrent commits).
  Matches PG's `synchronous_commit=on` throughput under load.
- `DurabilityMode::from_str` accepts the matrix spelling `"sync"` →
  `WalDurableGrouped` and `"strict"` → `Strict`.
- The group-commit coordinator, `GroupCommit` waiter, and
  `JournalFlusher`-style background thread were already implemented
  (`src/storage/wal/group_commit.rs`,
  `src/storage/unified/store/commit.rs`); this release wires the
  default and the matrix key.
- **Deferred:** true fire-and-forget async tier (P2.T4) — same
  group-commit primitives can drive it, spec stays open.

### Phase 3 — HOT decision helper + UPDATE fast path

- `src/storage/engine/hot_update.rs` — pure decision helper mirroring
  PG's `heap_update` (no indexed column modified + fits the page).
- `flush_applied_entity_mutation` consults the helper and skips the
  `index_entity_update` call when HOT fires — saves a registry-lock
  acquisition + damage-vector walk per UPDATE.
- Parser fix: `CREATE INDEX ... USING HASH` now accepts the `HASH`
  keyword token alongside the ident form.
- **Deferred:** page-local in-place rewrite + `t_ctid` chain walker
  (P3.T4) — requires new on-disk fields on entities; spec stays
  open.

### Phase 4 — Fused bulk-insert index maintenance

- `IndexStore::index_entity_insert_batch(collection, &rows)` — one
  registry-lock acquisition for the whole batch instead of N per
  row. Outer loop walks the index registry once; inner loop walks
  the batch. Mirrors PG's `heap_multi_insert` + `ExecInsertIndexTuples`.
- `MutationEngine::append_batch` routes through it.
- The upstream `bulk_insert` primitive + `create_rows_batch` wire
  path + gRPC `BulkInsertBinary` handler were already in place;
  this commit closes the index-fusion gap the plan identified.

### Phase 6.T1 — Background writer wired

- `PageCache::flush_some_dirty(max)` + `dirty_count()` — bounded
  snapshot of dirty pages plus count introspection.
- `Pager::flush_some_dirty(max)` / `dirty_fraction()` — mirror the
  bgwriter contract.
- `bgwriter::PagerDirtyFlusher` — production `DirtyPageFlusher`
  holding a `Weak<Pager>` so the background thread exits cleanly on
  database drop.
- `Database::open_with_config` spawns the bgwriter (non-read-only
  mode) with default config; `Database::bgwriter_stats()` accessor
  exposes rounds / pages_flushed / dirty fraction for tests + ops.

### Tests

79 new / extended integration tests across 15 suites (all green):

- `unit_locking` (5)  — compat matrix + 50-thread stress
- `unit_hot_update` (6) — pure decision coverage
- `e2e_locking_reads` (3) — SELECT acquires IS, disabled flag
  suppresses, admin doesn't lock
- `e2e_concurrent_writes` (4) — IX modes + 20 threads × 200 inserts
- `e2e_ddl_concurrency` (3) — X locks + DDL doesn't block other
  collections' writers
- `e2e_hot_update` (3) — unindexed / non-indexed-col / indexed-col
- `e2e_config_matrix` (7) — tier A self-heal, Tier B silence, env
  overrides, file overlay, durability mapping, idempotency

Plus every pre-existing integration suite (tenancy, within,
RLS-universal, cross-model MVCC, views, auto-index, multi-model)
stays green — zero regression across 80+ correctness tests.

### Commits

- `P0`: config matrix + overlay + Docker + perf-bench doc
- `P1`: `Arc<LockManager>` + `LockerGuard` + reads / writes / DDL wiring
- `P2`: `WalDurableGrouped` default
- `P3`: HOT helper + UPDATE fast path
- `P4`: fused secondary-index insert batch
- `P6.T1`: bgwriter `Weak<Pager>` wiring

### Deferred (tracked)

- **P2.T4** async commit tier — same group-commit primitives
- **P3.T4** t_ctid chain walker — needs new entity-format field
- **P5** full Lehman-Yao B-tree + STORE_VERSION_V8 migration —
  multi-day storage surgery; `next_leaf` right-link already present,
  `high_key` + lock-free descent + local-split lock pending

---

## 2026-04-17 — Structured Logging with `tracing` + File Rotation

Replaces ~140 ad-hoc `eprintln!` sites with a `tracing` /
`tracing-subscriber` / `tracing-appender` pipeline. Every server
event now carries levels, timestamps, and correlation fields
(`conn_id`, `tenant`, `transport`, `bind`, `peer`, `err`) that ops
tools can filter on.

### Features

- `reddb::telemetry::{init, TelemetryConfig, LogFormat,
  TelemetryGuard}` — façade over tracing-subscriber. Two layers:
  stderr (pretty / JSON) and optional daily-rotating file written
  via `tracing-appender`, both sharing the same `EnvFilter`.
- Background janitor (`telemetry::janitor::spawn`) purges rotated
  files older than `--log-keep-days` every hour. Silent no-op when
  no tokio runtime is active.
- `span::query_span`, `span::connection_span`, `span::listener_span`
  — pull `conn_id` / `tenant` out of the existing thread-locals and
  stamp them on every event inside the span.
- Server / gRPC / HTTP / wire / PG-wire / Unix-socket / MCP startup
  messages migrated to `tracing::info!` with structured `transport`
  + `bind` fields. Connection errors now emit `tracing::warn!` with
  `peer` and `err` fields.
- `auth::store` bootstrap and vault persist errors emit structured
  warnings so operators can grep for `target=reddb::auth` in JSON
  output.
- `execute_query` wrapped in a `query` span — every downstream log
  inherits `conn_id`, `tenant`, `query_len` automatically.

### CLI flags

Added to `red server`:

| Flag | Default | Purpose |
|------|---------|---------|
| `--log-dir <path>` | `<--path parent>/logs` | Directory for rotating log files |
| `--log-level <level>` | `info` | trace/debug/info/warn/error or `RUST_LOG` expression |
| `--log-format <fmt>` | `pretty` on TTY, `json` otherwise | `pretty` \| `json` |
| `--log-keep-days <N>` | `14` | Retention count for rotated files |
| `--no-log-file` | off | stderr-only mode |

`RUST_LOG` overrides `--log-level` when set.

### Embedded mode

The library never installs a subscriber itself. Embedders either
call `reddb::telemetry::init` to reuse the same config surface as
the server, or keep their own `tracing-subscriber` pipeline. Docs
at `docs/api/embedded.md#logging`.

### Dependencies

Adds `tracing = "0.1"`, `tracing-subscriber = "0.3"` (with
`env-filter`, `fmt`, `json`, `ansi`, `registry` features),
`tracing-appender = "0.2"`. ~80 KB stripped.

### Scope explicitly deferred

- Test-only `println!` / `eprintln!` inside `#[cfg(test)]` blocks
  (`storage/engine/algorithms/mod.rs`, `storage/primitives/*.rs`)
  left alone — they're expected benchmark output.
- CLI user-facing `println!` in `bin/red.rs` and `client/repl.rs`
  unchanged — that's command output on stdout, not operational
  logging.
- Metrics (Prometheus) and distributed tracing (OTEL) are separate
  plans, though `tracing` is the foundation for both.

Commits: [Phase 1 logging infra + Phase 2 server migration + Phase 3
storage noise + Phase 4 correlation spans + Phase 5 docs — bundled].

---

## 2026-04-17 — RedDB-Native Extensions (Cross-Model Feature Lift)

Takes every PG parity feature from the morning's bundle and extends
it across the other entity kinds (graph, vector, queue, timeseries)
so the RedDB idiom feels unified rather than table-centric.

### MVCC Universal — cross-model atomic transactions

Commit: [`7e80a60`](https://github.com/forattini-dev/reddb/commit/7e80a60)

- `stamp_xmin_if_in_txn` post-save hook on `RedDBRuntime` — stamps
  `xmin` on graph nodes / edges, vectors, documents, queue messages,
  and timeseries points as they leave the DevX builder API, without
  crossing the storage ↔ runtime layer boundary
- Visibility filter wired into every non-table scan:
  `graph_dsl::materialize_graph_with_projection`, queue
  `load_queue_message_views`, vector ANN (`db.similar()`)
  post-filter — all use the same `capture_current_snapshot()` /
  `entity_visible_with_context()` pair that tables use
- `delete_message_with_state` turns queue ACK into an MVCC
  tombstone when running inside a transaction (revived by
  `ROLLBACK`, flushed by `COMMIT`)
- `runtime::mvcc` pub re-export module so transports (PG wire,
  gRPC, HTTP) and integration tests can emulate per-connection
  isolation
- `detect_mode` now recognises `BEGIN` / `COMMIT` / `ROLLBACK` /
  `SAVEPOINT` / `RELEASE` / `VACUUM` / `ANALYZE` / `RESET` as
  bare-token SQL commands

**Impact:** one `BEGIN` spans tables + graphs + vectors + queues +
timeseries atomically. PG has no graphs/vectors. Neo4j has no
queues. Kafka has no MVCC. RedDB does all of it natively.

### ASK tenant-scoped — RLS in the RAG pipeline

Commit: [`c08cb3f`](https://github.com/forattini-dev/reddb/commit/c08cb3f)

- `search_entity_allowed(collection, entity, snap_ctx, rls_cache)` —
  per-entity gate wired into all three tiers of `search_context`
  (field-index, token-index, global scan)
- Restrictive default: RLS enabled + zero matching policy ⇒ deny.
  The LLM never reasons over rows the caller cannot see
- Acme session asking over a global corpus transparently filters to
  acme-tagged rows; globex gets its own view

### Tenancy dotted paths — JSON-native tenant discriminators

Commit: [`5a8ce89`](https://github.com/forattini-dev/reddb/commit/5a8ce89)

- `TENANT BY (root.sub.path)` on `CREATE TABLE` and
  `ALTER TABLE t ENABLE TENANCY ON (root.sub)` — arbitrary depth
- Root segment accepts keyword idents (`meta`, `data`, `tags`) that
  would otherwise tokenise as reserved words
- Evaluator parses `Value::Text` starting with `{` or `[` as JSON
  too — users who stash JSON in TEXT columns get the same
  filtering as declared JSON
- `merge_dotted_tenant` and `dotted_tail_already_set` helpers keep
  INSERT auto-fill idempotent: existing root JSON is merged
  in-place, missing root is synthesised as `{tail: tenant_id}`,
  explicit user values are trusted
- Result cache key now mixes tenant + auth identity — previously a
  `SELECT` filtered for acme would be served back to globex when
  the query string matched

### RLS universal per entity kind

Commit: [`55e928d`](https://github.com/forattini-dev/reddb/commit/55e928d)

- Grammar: `CREATE POLICY ... ON NODES|EDGES|VECTORS|MESSAGES|POINTS|DOCUMENTS OF <collection>`.
  The bare `ON <collection>` form still defaults to `TABLE` for
  backwards compatibility
- `PolicyTargetKind` stamped on every stored policy;
  `matching_rls_policies_for_kind` + `rls_policy_filter_for_kind`
  filter by kind when the scan path supplies one
- `runtime_any_record_from_entity` now materialises `QueueMessage`
  entities so policies like
  `USING (payload.tenant = CURRENT_TENANT())` can reach the JSON
  payload via the dotted-path resolver
- `evaluate_entity_filter_with_db` falls back to the any-record
  builder when the TableRow-only path returns `None` — non-tabular
  collections now actually evaluate policies instead of denying
  by default
- Queue scan calls `rls_policy_filter_for_kind(queue, Select, Messages)`
  — `ON MESSAGES OF` policies gate POP / LEN / PEEK; deny-default
  when RLS is on and no MESSAGES policy matches the role

### Views end-to-end + stacked filter merge

Commit: [`0353c6a`](https://github.com/forattini-dev/reddb/commit/0353c6a)

- Parser: accept both `Token::As` / `Token::Or` and their ident
  forms — `CREATE OR REPLACE VIEW … AS …` was erroring at the
  first token because the lexer promotes both to keyword tokens
- `execute_query` (raw SQL entry) now calls `rewrite_view_refs`
  before dispatch, mirroring `execute_query_expr`. Views
  previously only resolved through the prepared-statement path
- Stacked view rewrite: when a view body is itself a `TableQuery`,
  the outer query's `WHERE` (AND-combined), `LIMIT` (min), and
  `OFFSET` (added) are merged into the body instead of being
  silently dropped. `CREATE VIEW b AS SELECT * FROM a WHERE y;
  SELECT * FROM b WHERE z` now applies both predicates
- CREATE / DROP VIEW invalidate plan + result caches. `OR REPLACE`
  no longer serves stale rows from the obsolete body
- MATERIALIZED VIEW + REFRESH execute the body end-to-end

### Test coverage

Eleven end-to-end integration tests added, all green:

- `tests/e2e_cross_model_tx.rs` — 3 tests (graph / queue /
  multi-model atomic rollback)
- `tests/e2e_ask_tenant_scoped.rs` — 1 test (ASK corpus filtering)
- `tests/e2e_tenancy_dotted.rs` — 3 tests (reads, auto-fill
  missing root, merge existing JSON root)
- `tests/e2e_rls_universal.rs` — 1 test (MESSAGES-of-queue policy)
- `tests/e2e_views.rs` — 3 tests (filtered body, stacked, REFRESH)

### Housekeeping

- `DataType::Unknown` variant added — function catalog already
  referenced it for the polymorphic `JSON_SET(json, path, value)`
  third argument, blocking `cargo check`
- `match_if_exists` / `match_if_not_exists` widened to `pub(crate)`
  so the `sql.rs` parser router can reach them
- `src/storage/import/csv.rs` return-type mismatch (`.map(|_| ())`)

---

## 2026-04-17 — PostgreSQL Feature Parity (19 categories)

One bundled release closing every category on the PG parity matrix we
committed to. Each subsystem is shippable independently; grouped here
so the exhaustive-match refactors stay atomic.

Commit: [`81a6155`](https://github.com/forattini-dev/reddb/commit/81a6155)

### Transactions & Visibility (MVCC)

- `BEGIN` / `COMMIT` / `ROLLBACK` with per-connection `TxnContext`
- `SAVEPOINT` / `RELEASE SAVEPOINT` / `ROLLBACK TO SAVEPOINT` with
  sub-xid stack (nested rollback without aborting the parent txn)
- Tuple-level `xmin` / `xmax` stamping on INSERT + DELETE
- Read-path visibility filter wired across ~15 scan sites (sequential,
  parallel, index-assisted, bloom, bitmap-AND, fast-path entity lookup,
  aggregates, vector similarity, universal scan)
- Own-xid visibility: writer always sees own writes from sub-transactions
- DELETE inside a transaction becomes an MVCC tombstone (`xmax` stamped,
  physical removal deferred to COMMIT); ROLLBACK revives the tuple
- Autocommit snapshot uses `peek_next_xid()` so committed rows stay
  visible (previously 0 broke MVCC reads)
- Docs: [Transactions](query/transactions.md)

### Security

- `CREATE POLICY` / `DROP POLICY` — row-level security with a USING
  predicate per `(table, role, action)`
- `ALTER TABLE ENABLE / DISABLE ROW LEVEL SECURITY`
- RLS gates in SELECT, UPDATE, DELETE (OR-combined across policies,
  AND-folded into the user's WHERE)
- mTLS certificate authentication — `CommonName` + `SAN rfc822Name`
  modes with OID-to-role mapping
- OAuth / OIDC bearer-token validator — pluggable JWT verifier closure,
  standard `iss` / `aud` / `exp` / `nbf` claims
- Docs: [RLS](security/rls.md), [mTLS & OAuth](security/overview.md)

### Multi-Tenancy

- Session handle: `SET TENANT 'id'` / `RESET TENANT` / `SHOW TENANT`
- Scalar functions: `CURRENT_TENANT()`, `CURRENT_USER()`,
  `SESSION_USER()`, `CURRENT_ROLE()`
- Declarative: `CREATE TABLE t (...) TENANT BY (col)` — auto-RLS policy
  + INSERT auto-fill of the tenant column
- Retrofit: `ALTER TABLE t ENABLE TENANCY ON (col)` /
  `ALTER TABLE t DISABLE TENANCY`
- Persistence: tenant-table markers replayed from `red_config` on boot
- Docs: [Multi-Tenancy](security/multi-tenancy.md)

### Views & Materialized Views

- `CREATE VIEW` / `CREATE OR REPLACE VIEW` / `DROP VIEW`
- `CREATE MATERIALIZED VIEW` / `REFRESH MATERIALIZED VIEW` with
  `REFRESH POLICY` (manual / on-write / interval)
- Query rewriter descends `TableSource::Subquery` recursively so
  `SELECT ... FROM view_of_view` works
- Docs: [Views](query/views.md)

### Partitioning

- `CREATE TABLE ... PARTITION BY RANGE|LIST|HASH (col)`
- `ALTER TABLE parent ATTACH PARTITION child FOR VALUES ...`
- `ALTER TABLE parent DETACH PARTITION child`
- Registry-only in Phase 2.2; planner-level partition pruning in
  Phase 4
- Docs: [Partitioning](query/partitioning.md)

### Replication

- `QuorumCoordinator` with `Async` / `Sync` / `Regions(N)` modes
- Multi-region replica binding + LSN-watermark consistent reads
- Quorum-ack write path over existing replication infra

### Backup & Recovery

- `red dump -c <collection> -o file.rdbdump`
- `red restore -i file.rdbdump`
- Point-in-time recovery: `red pitr-list`, `red pitr-restore
  --target-time '2026-04-17 12:30:00 UTC'`

### Foreign Data Wrappers

- `ForeignDataWrapper` trait + `ForeignTableRegistry`
- Built-in CSV wrapper (RFC 4180 inline parser, per-table options)
- DDL: `CREATE SERVER` / `CREATE FOREIGN TABLE` / `DROP SERVER` /
  `DROP FOREIGN TABLE`
- Read-path intercept: foreign-table scans bypass native collection
  lookup
- Docs: [Foreign Data Wrappers](guides/foreign-data-wrappers.md)

### Network

- PostgreSQL v3 wire protocol — startup negotiation, simple query
  (`Q` / `T` / `D` / `C` / `Z` frames), SSL rejection with `N`
- PG type OID mapping (~20 variants)
- Unix socket transport via `tokio::net::UnixListener`
  (`--wire-bind unix:/tmp/reddb.sock`)
- Docs: [PostgreSQL Wire Protocol](api/postgres-wire.md)

### DDL & Maintenance

- `VACUUM [FULL] [table]` — triggers GC + page flush + stats refresh
- `ANALYZE [table]` — refreshes planner statistics
- `CREATE SCHEMA [IF NOT EXISTS] name` / `DROP SCHEMA`
- `CREATE SEQUENCE name START ... INCREMENT ...` + `nextval()` /
  `currval()` scalars
- Docs: [Maintenance & DDL Extras](query/maintenance.md)

### JSON & Imports

- `json_extract(json, '$.path')` / `json_set(json, '$.path', value)` /
  `json_array_length(json)` / `json_path_query(json, path)`
- `COPY table FROM 'file.csv' [WITH (DELIMITER ',', HEADER true)]`
- CSV import path paralleling JSONL / Parquet (bulk insert)

### Platforms

- Windows + macOS CI matrix (`cargo check --locked --all` +
  `cargo test --locked --lib`)
- `#[cfg(windows)]` gates for systemd, mmap, `/proc`, Unix socket
  (gracefully skipped)
- `null_device()` helper returning `NUL` on Windows and `/dev/null`
  elsewhere

---

## 2026-04-16 — WAL Durability & BRIN Timeseries

### WAL Durability

Commit: [`c63ed06`](https://github.com/forattini-dev/reddb/commit/c63ed06)

- WAL-backed writes: INSERT / UPDATE / DELETE hit the log before the
  pager, making every committed mutation crash-safe
- Configurable fsync policy (`every_commit` / `every_n` / `interval`)
- Boot-time replay of unflushed WAL frames
- Integration tests covering crash-and-recover flows

### BRIN Timeseries Indexing

Commit: [`eeb5dd5`](https://github.com/forattini-dev/reddb/commit/eeb5dd5)
+ correctness fix [`d6f6d9c`](https://github.com/forattini-dev/reddb/commit/d6f6d9c)

- Block-range zone maps for time-series segments (BRIN-equivalent)
- Min/max per block + binary search pruning
- Race-free reads under concurrent writers (lock contention fixed)
- O(log n) range scans on timestamp-ordered metric streams

### Storage Correctness

Commits: [`f98cc61`](https://github.com/forattini-dev/reddb/commit/f98cc61),
[`4eb4f77`](https://github.com/forattini-dev/reddb/commit/4eb4f77),
[`6a5ce02`](https://github.com/forattini-dev/reddb/commit/6a5ce02),
[`cb08fe3`](https://github.com/forattini-dev/reddb/commit/cb08fe3),
[`820274f`](https://github.com/forattini-dev/reddb/commit/820274f),
[`8e24f31`](https://github.com/forattini-dev/reddb/commit/8e24f31)

- Metadata persistence in binary file format (bumped to
  `STORE_VERSION_V7`)
- Race-free sealed-segment deletes + table-scoped result cache
- Arc-wrapped prepared statements — no more per-call re-plan
- Centralised sealed→growing rewrite path; UPDATE persisted via pager
- CDC + context index gated on confirmed deletion (eliminated
  double-emit)
- Canonical sealed-segment mutations — durable bulk persist path
- 4 critical bugs fixed: sealed mutations, delete ordering, unsafe flag
  rename, spill codec type mismatches

### Query Performance

Commit: [`9818697`](https://github.com/forattini-dev/reddb/commit/9818697)

- Unified mutation engine: single kernel for single-row / micro-batch /
  bulk INSERT
- Slot-indexed aggregates
- Covered queries: skip heap fetch when projection ⊆ indexed column
- Prepared statement Arc reuse across calls

---

## Earlier — New Data Structures & Query Extensions

### New Data Structures (10 total)

- **Bloom Filter**: Per-segment probabilistic key testing,
  auto-populated on insert, bloom pruning in query executor
- **Hash Index**: O(1) exact-match lookups via
  `CREATE INDEX ... USING HASH`
- **Bitmap Index**: Roaring bitmap for low-cardinality columns via
  `CREATE INDEX ... USING BITMAP`
- **R-Tree Spatial Index**: Radius/bbox/nearest-K geo queries via
  `SEARCH SPATIAL` and `CREATE INDEX ... USING RTREE`
- **Skip List + Memtable**: Write buffer in GrowingSegment, sorted
  drain on seal
- **HyperLogLog**: Approximate distinct counting (`CREATE HLL`,
  `HLL ADD/COUNT/MERGE`)
- **Count-Min Sketch**: Frequency estimation (`CREATE SKETCH`,
  `SKETCH ADD/COUNT`)
- **Cuckoo Filter**: Membership testing with deletion (`CREATE FILTER`,
  `FILTER ADD/CHECK/DELETE`)
- **Time-Series**: Chunked storage with delta-of-delta timestamps,
  Gorilla XOR compression, retention policies, time-bucket aggregation
- **Queue / Deque**: FIFO/LIFO/Priority message queue with consumer
  groups (`CREATE QUEUE`, `QUEUE PUSH/POP/PEEK/LEN/PURGE/ACK/NACK`)

### Query Language Extensions

- `CREATE INDEX [UNIQUE] name ON table (cols) USING HASH|BTREE|BITMAP|RTREE`
- `DROP INDEX [IF EXISTS] name ON table`
- `SEARCH SPATIAL RADIUS lat lon km COLLECTION col COLUMN col [LIMIT n]`
- `SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col`
- `SEARCH SPATIAL NEAREST lat lon K n COLLECTION col COLUMN col`
- `CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]`
- `DROP TIMESERIES [IF EXISTS] name`
- `CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]`
- `DROP QUEUE [IF EXISTS] name`
- `QUEUE PUSH|POP|PEEK|LEN|PURGE|GROUP CREATE|READ|ACK|NACK`
- `CREATE/DROP HLL|SKETCH|FILTER` + `HLL ADD/COUNT/MERGE` +
  `SKETCH ADD/COUNT` + `FILTER ADD/CHECK/DELETE`
- JSON inline literals: `{key: value}` without quotes in VALUES and
  QUEUE PUSH

### Deep Integration

- **IndexStore**: Unified manager for Hash/Bitmap/Spatial indices in
  RuntimeInner
- **IndexSelectionPass**: Query optimizer analyzes WHERE and recommends
  Hash/BTree/Bitmap automatically
- **Bloom filter pruning**: Executor skips segments when bloom says key
  is absent
- **Spatial search**: Functional radius/bbox/nearest with Haversine
  distance on GeoPoint and lat/lon fields
- **Memtable**: Write buffer integrated in GrowingSegment lifecycle
- **red_config**: All new features configurable via
  `SET CONFIG red.indexes.*`, `red.memtable.*`, `red.probabilistic.*`,
  `red.timeseries.*`, `red.queue.*`
- **ProbabilisticCommand dispatch**: HLL/CMS/Cuckoo fully functional
  end-to-end via SQL

### Dependencies

- Added `roaring = "0.10"` (Bitmap Index)
- Added `rstar = "0.12"` (R-Tree Spatial Index)

### Earlier

- Initial public release preparation.
- Added multi-model embedded/server/serverless documentation and
  packaging pipeline.
- Added unified crate publishing workflow for crates.io.
