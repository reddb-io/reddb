# perf(storage): UnifiedRecord schema-shared layout (Roadmap #1) [AFK]

GitHub: reddb-io/reddb#156
Parent: #152

Replace per-record `HashMap<String,Value>` with `Arc<Vec<String>>` schema + parallel `Vec<Value>` + optional overflow HashMap. 746 call sites. Compounds across `select_range`, `select_filtered`, `mixed_workload_indexed`, `select_complex`. Flamegraphs show HashMap insert at ~60% CPU on scan paths.

## Acceptance Criteria

- [ ] `UnifiedRecord` carries `Arc<Vec<String>>` schema + parallel `Vec<Value>` + opt-in overflow.
- [ ] Public API: `set_owned`, `get`, `columns`, schemaless path.
- [ ] All 746 call sites compile; existing test suite passes.
- [ ] Proptest covers get/set round-trip, overflow promotion, schema-mismatch.
- [ ] Bench shows measurable improvement on scan-heavy scenarios under canonical config (#154).
- [ ] No regression on `typed_insert`, `disk_usage`.
