# perf(index): IncrementalIndexMaintainer — close Finding #4 [AFK]

GitHub: reddb-io/reddb#160
Parent: #152

Finding #4: secondary indexes snapshot at CREATE INDEX, never update on writes. Build `IncrementalIndexMaintainer` deep module: input pre/post images of mutated row → output index delta ops. Hook into all write paths (insert/update/delete/bulk variants). Apply to hash, bitmap, bloom, context indexes.

## Acceptance Criteria

- [ ] `IncrementalIndexMaintainer` deep module with small input/output surface.
- [ ] Every write path feeds pre/post images into maintainer.
- [ ] Hash, bitmap, bloom, context indexes accept delta op stream.
- [ ] Proptest: random workload preserves index consistency at every step (full re-scan diff).
- [ ] EXPLAIN shows secondary index selected by WHERE-bound queries after write churn.
- [ ] Bench: `select_filtered` improves under canonical config (#154) when combined with #156.
- [ ] Existing index tests pass unchanged.
