# Limitations

RedDB v0.1 (Beta) has the following known limitations:

## Not Yet Supported

| Feature | Status | Notes |
|:--------|:-------|:------|
| Multi-region replication | Not supported | Planned for v2 |
| Automatic sharding | Not supported | Single-node only |
| Full RBAC granularity | Partial | Three roles: admin, write, read |
| Cross-entity transactions | Not supported | Per-collection atomicity |
| Distributed query planner | Not supported | Local cost-based planner |
| Full ACID guarantees | WAL-based | Best-effort durability |
| Automatic failover | Not supported | Manual failover only |
| SQL joins across collections | Limited | Single-table joins supported |
| Persistent binary index formats | In progress | Being hardened |
| Streaming query results | Not supported | Full result sets only |

## Performance Considerations

| Scenario | Recommendation |
|:---------|:---------------|
| `FROM ANY` on large databases | Always use `LIMIT`; filter by `_collection` |
| Vector search > 1M vectors | Use IVF with `n_probes` tuning |
| Graph analytics on large graphs | Use projections to scope the subgraph |
| Bulk insert > 100K entities | Use bulk endpoints, not individual inserts |
| WAL accumulation | Force periodic checkpoints in long-running servers |

## Known Issues

- The SQL planner does not yet support subqueries
- Natural language queries are best-effort and may misinterpret complex intent
- Graph analytics compute over the full graph unless a projection is specified
- Remote backends (S3, Turso, D1) add significant latency

## Planned for Future Releases

- Cross-collection ACID transactions
- Distributed query execution
- Automatic sharding
- Multi-region active-active replication
- Persistent vector index formats (HNSW, IVF saved to disk)
- Advanced SQL features (subqueries, CTEs, window functions)
