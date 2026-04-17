# Changelog

All notable changes to RedDB are documented here. Dates are ISO-8601 (UTC-3).

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
