# perf(btree): batch upsert by leaf for sorted keys (Roadmap #4) [AFK]

GitHub: reddb-io/reddb#159
Parent: #152

Add `BTree::upsert_batch_sorted`: sort keys, walk to each leaf once, apply all updates per leaf in one page write. Additive helper. Caller `persist_entities_to_pager` sorts before invoking.

## Acceptance Criteria

- [ ] `BTree::upsert_batch_sorted(keys_and_values)` walks each leaf once.
- [ ] `persist_entities_to_pager` sorts before invoking.
- [ ] Existing single-key `upsert` unchanged.
- [ ] Property test: random batches produce identical state to loop-of-upsert baseline.
- [ ] Bench: `bulk_update` improves under canonical config (#154).
- [ ] No regression on `insert_sequential`, `insert_bulk`.
