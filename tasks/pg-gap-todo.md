# Todo — PG Gap Phase 1+2

Tracks `tasks/pg-gap-plan.md`. Legend: `[ ]` not started · `[~]` in progress · `[x]` done · `[!]` blocked

## Gate 0 — approvals

- [x] Plan approved by human — `tasks/pg-gap-plan.md` (implicit via build greenlight)
- [ ] Answers to open questions (session model, proto break, HNSW sync vs async, baseline lock)
- [ ] `make lock-baselines` captured on current tip

## Progress snapshot (2026-04-19)

Lane paralela completa: **T4.1, T4.2, T5, T6** merged on main.

| Commit | Task | |
|---|---|---|
| `0251d23` | T4.1 | RETURNING parser + AST |
| `ffb077e` | T4.2 | RETURNING executor (INSERT Row) + 6 e2e |
| `5eb89d2` | T5   | Savepoint RELEASE bug fix + 6 e2e |
| `4bb63df` | T6   | Advisory locks (pg_advisory_*) + 7 tests |

Zero regressions vs baseline (12 pre-existing failures unchanged).
+23 tests added across the four commits.

Critical path (T1 → T2 → T3 → T7/T8/T9) still pending.

## Phase 1 — unblock bench + observability

- [ ] **T1** Secondary index maintenance on write path (#15)
  - Acceptance: inline idx updated synchronously; WAL replay consistent; +1k insert visible via idx query
  - Verify: `cargo test storage::engine::btree::tests::secondary_index_maint` · `make mini-duel` green on `mixed_workload_indexed`
  - Files: `src/storage/engine/btree/impl.rs`, `src/storage/schema/{registry,table}.rs`, `src/index.rs`, `src/storage/wal/{writer,record}.rs`
  - Scope: L (split by index kind if >8 files)
- [ ] **T2** EXPLAIN (plan tree) (#13)
  - Acceptance: `EXPLAIN SELECT ...` returns `{op, table?, index?, est_rows, est_cost}` tree; IndexScan named
  - Verify: `red sql "EXPLAIN ..."` · HTTP `POST /query {"explain":"plan"}` · unit test asserts IndexScan
  - Files: `src/storage/query/planner/mod.rs`, `src/storage/query/sql.rs` (or new `parser/explain.rs`), `src/storage/query/ast.rs`, `src/server/http/*`, `src/grpc.rs` + proto
  - Scope: M
- [ ] **T3** EXPLAIN ANALYZE (#13)
  - Acceptance: `actual_rows`, `actual_ms`, loop counts per node; overhead <10%
  - Verify: `bench-scenarios/src/explain_check.rs` · overhead ratio test
  - Files: `src/storage/query/executor.rs` (+executors/*), `src/storage/query/planner/mod.rs`
  - Deps: T2
  - Scope: M

### Checkpoint A

- [ ] `make mini-duel` 28/28 on 5 consecutive runs
- [ ] `cargo test -p reddb` clean
- [ ] `mixed_workload_indexed` green
- [ ] EXPLAIN ANALYZE populated via HTTP + CLI
- [ ] Human review → proceed to Phase 2

## Phase 2 — quick wins + richer indexes

- [x] **T4.1** RETURNING parser + AST (#1a)
  - Delivered: `0251d23` — ReturningItem enum, parser on INSERT/UPDATE/DELETE
- [x] **T4.2** RETURNING executor for INSERT Row (#1a)
  - Delivered: `ffb077e` — UPDATE/DELETE still error explicit (T4.3)
- [ ] **T4.3** RETURNING executor for UPDATE + DELETE (#1a, follow-up)
  - Acceptance: UPDATE/DELETE with RETURNING emit affected rows pre-image (DELETE) / post-image (UPDATE)
  - Approach: pre-image snapshot via WHERE-matching scan before mutation; post-image re-read by id for UPDATE
  - Files: `src/runtime/impl_dml.rs` (execute_update/execute_delete)
  - Scope: M
- [x] **T5** Savepoints runtime wiring (#8a)
  - Delivered: `5eb89d2` — discovered and fixed release-drops-writes bug
- [x] **T6** Advisory locks (#8b)
  - Delivered: `4bb63df` — all four pg_advisory_* variants + volatile-query cache bypass
- [ ] **T7** Partial indexes (#12)
  - Acceptance: parser `WHERE pred`; write path evaluates; planner uses only when query predicate implies idx predicate
  - Verify: unit test size = non-deleted count · `index_advanced::partial`
  - Files: `src/storage/query/parser/index_ddl.rs`, `src/storage/schema/table.rs`, `src/index.rs`, `src/storage/engine/btree/impl.rs`, `src/storage/query/planner/index_only.rs`
  - Deps: T1, T2
  - Scope: M
- [ ] **T8** Expression indexes (#12)
  - Acceptance: `CREATE INDEX ON t (lower(email))`; planner matches exact-match expr
  - Verify: `EXPLAIN SELECT ... WHERE lower(email)=?` uses idx · `index_advanced::expression`
  - Files: parser, schema, btree, planner
  - Deps: T1, T2
  - Scope: M
- [ ] **T9** Covering / index-only scan (#12)
  - Acceptance: `INCLUDE (col)` stored in leaf; planner emits `IndexOnlyScan`; heap skipped
  - Verify: EXPLAIN shows IndexOnlyScan · `index_advanced::covering` 1M-row bench
  - Files: parser, schema, btree (leaf payload), planner, `indexed_scan.rs`
  - Deps: T1, T2
  - Scope: M-L

### Checkpoint B

- [ ] All new scenarios green: `returning_throughput`, `savepoint_rollback`, `advisory_lock_contention`, `index_advanced` (3)
- [ ] `make check-baseline` — no regression >10%
- [ ] Human review → draft Phase 3 plan (#1b/c/d/e/f, #6, #7, #5, #10, #11, #2)
