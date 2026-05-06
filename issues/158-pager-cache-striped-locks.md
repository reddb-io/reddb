# perf(storage): pager cache striped locks (Roadmap #3) [AFK]

GitHub: reddb-io/reddb#158
Parent: #152

Shard `PageCache` into 8/16 buckets, each with own RwLock. `insert(page_id)` picks bucket via `page_id % N`. Per-shard SIEVE eviction in first cut.

## Acceptance Criteria

- [ ] PageCache sharded into 8 or 16 buckets, each own RwLock.
- [ ] Bucket selection deterministic (`page_id % N`).
- [ ] Per-shard SIEVE; trade-off vs global documented in code.
- [ ] No public API changes; existing tests pass.
- [ ] Concurrency property tests: 2 writers on disjoint pages don't block.
- [ ] Bench: write-heavy scenarios improve under canonical config (#154).
