# perf(query): AggregateQueryPlanner — push-down GROUP BY into scan [AFK]

GitHub: reddb-io/reddb#161
Parent: #152

`aggregate_group` 12× behind PG, no roadmap item. Build `AggregateQueryPlanner` deep module: input AST + scan iterator → output per-group accumulator + final row stream of one row per group. First cut: `COUNT(*)`, `COUNT(col)`, `SUM`, `AVG`, `MIN`, `MAX` over single-column GROUP BY. Multi-column + rare aggregates as follow-up.

## Acceptance Criteria

- [ ] `AggregateQueryPlanner` deep module with small input/output surface.
- [ ] Supports COUNT(*), COUNT(col), SUM, AVG, MIN, MAX over single-column GROUP BY.
- [ ] Functional parity with legacy materialize-all path on existing aggregate corpus.
- [ ] Test harness asserts push-down materializes O(group count), not O(row count).
- [ ] Edge cases: empty groups, NULL columns, single-row groups, hashable key collisions.
- [ ] Bench: `aggregate_group` improves under canonical config (#154).
- [ ] Multi-column GROUP BY + rare aggregates filed as follow-up issues.
