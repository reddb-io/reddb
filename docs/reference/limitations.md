# Limitations

RedDB v0.1 (Beta) has the following known limitations:

## Not Yet Supported

| Feature | Status | Notes |
|:--------|:-------|:------|
| Multi-region replication | Not supported | Planned for v2 |
| Automatic sharding | Not supported | Single-node only |
| Full RBAC granularity | Partial | Three roles: admin, write, read |
| Cross-model transactions | Partial (single-node) | Transaction control can cover multiple model paths that use RedDB's shared visibility resolver, but the new history-store MVCC guarantee is table-row-first. Non-table models retain their documented behavior until each path adopts the history-store resolver. Cross-node distributed transactions are not supported. |
| `SERIALIZABLE` isolation | Rejected at parse time | Parser accepts `READ COMMITTED` / `REPEATABLE READ` / `SNAPSHOT` (all map to snapshot). SSI is future work. |
| Autovacuum | Not supported | Manual `VACUUM` reclaims table-row history and tombstones. There is no background autovacuum daemon. |
| Historical secondary indexes | Not supported | Table-row correctness uses current indexes with MVCC recheck and fallback. Dedicated historical indexes are future performance work. |
| Distributed query planner | Not supported | Local cost-based planner |
| Full ACID guarantees | Partial | Table-row single-node transaction recovery uses `TxCommitBatch`; distributed and full multi-model history-store ACID guarantees are out of scope. |
| Automatic failover | Not supported | Manual failover only |
| SQL joins across collections | Limited | Single-table joins supported |
| Persistent binary index formats | In progress | Being hardened |
| Streaming query results | Not supported | Full result sets only |
| PG wire binary result format | Not supported | `Parse`/`Bind` extended query is supported; result rows are emitted in text format. |
| Subqueries in `FROM`/`WHERE` | Limited | Only `SELECT ... FROM (SELECT ...) AS alias` for the simple nested-select case lands today. |
| Partition pruning in the planner | Library ready, DDL wiring pending | Pruner (`src/storage/query/planner/partition_pruning.rs`) ships RANGE / LIST / HASH rules with AND tightening + OR widening + conservative fallback. Planner call-site lands in the sprint that follows B5 projections. |
| Append-only tables | Shipped | `CREATE TABLE ... APPEND ONLY` (or `WITH (append_only = true)`) rejects UPDATE / DELETE at parse time. |
| Hypertables (time-range partitioning) | Registry shipped, SQL DDL pending | `HypertableRegistry` routes writes by timestamp and tracks chunks; `CREATE HYPERTABLE` / `drop_chunks` SQL wiring lands next sprint. |
| Partition TTL | Library shipped, SQL DDL pending | `HypertableSpec::with_ttl` + `HypertableRegistry::sweep_expired` + per-chunk overrides + `chunks_expiring_within` preview. `CREATE HYPERTABLE ... WITH (ttl = '90d')` DDL lands with the hypertable parser bridge. See [Partition TTL](../data-models/partition-ttl.md). |
| Continuous aggregates | Engine shipped, SQL DDL pending | `ContinuousAggregateEngine` + incremental refresh pins the arithmetic; `CREATE CONTINUOUS AGGREGATE` DDL wiring lands next sprint. |
| ClickHouse-style projections | Matcher shipped, maintenance pending | `pick_projection` matcher ships; CDC-driven maintenance + `ALTER TABLE ADD PROJECTION` DDL land in follow-on. |
| Columnar batch execution + SIMD | Library shipped, planner wiring pending | `ColumnBatch` + operators + AVX2 reducers + rayon parallel reducer pool are unit-tested; planner chooser lands next sprint. |

## Performance Considerations

| Scenario | Recommendation |
|:---------|:---------------|
| `FROM ANY` on large databases | Always use `LIMIT`; filter by `collection` |
| Vector search > 1M vectors | Use IVF with `n_probes` tuning |
| Graph analytics on large graphs | Use projections to scope the subgraph |
| Bulk insert > 100K entities | Use bulk endpoints, not individual inserts |
| WAL accumulation | Force periodic checkpoints in long-running servers |

## Known Issues

- The SQL planner does not yet support arbitrary subqueries (`SELECT ... FROM (SELECT ...) AS alias` works; correlated subqueries in `WHERE` are partial).
- Natural language queries are best-effort and may misinterpret complex intent.
- Graph analytics compute over the full graph unless a projection is specified.
- Remote backends (S3, Turso, D1) add significant latency.

Window functions, CTEs (including recursive), and basic aggregates
are **landed** — earlier revisions of this doc listed them as
pending. See
[`src/storage/query/executors/window.rs`](../../src/storage/query/executors/window.rs),
[`src/storage/query/executors/cte.rs`](../../src/storage/query/executors/cte.rs),
and
[`src/storage/query/executors/aggregation.rs`](../../src/storage/query/executors/aggregation.rs)
for the surface.

## Planned for Future Releases

- Full multi-model history-store MVCC rollout
- Distributed query execution + automatic sharding (see
  [distributed-roadmap.md](../architecture/distributed-roadmap.md))
- Multi-region active-active replication
- Persistent vector index formats (HNSW, IVF saved to disk)
- SQL DDL for hypertables, continuous aggregates, projections
  (engines shipped; parser wiring in the next sprint cycle)
- SIMD / parallel paths wired through the SQL planner (library
  shipped)
