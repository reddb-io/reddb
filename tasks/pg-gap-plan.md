# Plan — PG Gap Phase 1+2 (items #15, #13, #1a, #8a/b, #12)

Derived from `PLAN-NEW.md` (2026-04-19). Covers first 4-6 weeks of execution. Phases 3-6 deferred to follow-up plan after gate passes.

Existing `tasks/plan.md` (Performance Parity Push) is untouched — this is a parallel workstream.

## Scope

| # | Feature | Phase | Est |
|---|---------|-------|-----|
| 15 | Secondary index maintenance | 1 | 1-2 wk |
| 13 | EXPLAIN / EXPLAIN ANALYZE | 1 | 4-5 d |
| 1a | RETURNING clause | 2 | 1-2 d |
| 8a | Savepoints (runtime wiring) | 2 | 3-5 d |
| 8b | Advisory locks | 2 | 2-3 d |
| 12 | Partial / expression / covering indexes | 2 | 1 wk |

Out of scope here: window/CTE/MERGE/LATERAL, isolation SI/SSI, 2PC, partitioning, PITR, FDW, views.

## Codebase reality check

Confirmed paths (differ from PLAN-NEW in places):

- Planner dir **exists** at `src/storage/query/planner/` (cost, histogram, index_only, join_dp, cache). EXPLAIN extends this — no new `src/query/planner.rs`.
- `IndexDef` lives at `src/storage/schema/table.rs:566`. `IndexKind` in `src/index.rs:10` and `src/storage/index/stats.rs:9`.
- SAVEPOINT tokens parsed in `src/storage/query/sql.rs:424,1181-1202` (`TxnControl::Savepoint/RollbackToSavepoint/ReleaseSavepoint`). Runtime wiring is what's missing.
- RETURNING **not** parsed (only test fixtures mention). Need parser + executor path.
- WAL modules: `src/storage/wal/{writer,record,recovery,transaction,rmgr}.rs`.
- DML parser: `src/storage/query/parser/dml.rs`.
- Schema registry: `src/storage/schema/registry.rs`.

## Dependency graph

```
#15 secondary-index-maint  ──┐
                             ├──► #12 partial/expr/covering
#13 EXPLAIN  ────────────────┘     (planner must know new IndexDef fields)

#1a RETURNING      — independent
#8a Savepoints     — independent (parser done, runtime only)
#8b Advisory locks — independent
```

`#15` unblocks `mixed_workload_indexed` bench. `#13` unblocks perf attribution. `#12` needs both. The three independents (`#1a`, `#8a`, `#8b`) can parallelize against the critical path.

## Vertical slicing

Each task = one user-visible outcome, parser → engine → WAL → bench scenario. No horizontal "build all parser changes first".

## Phases and checkpoints

### Phase 1 — Unblock bench + observability (2-3 wk)

- **T1** Secondary index maintenance on write path (#15)
- **T2** EXPLAIN (plan tree only) (#13)
- **T3** EXPLAIN ANALYZE (adds runtime stats) (#13)

### Checkpoint A (after T1-T3)

- [ ] `make mini-duel` passes 28/28 on 5 consecutive runs
- [ ] `bench-scenarios/mixed_workload_indexed` green
- [ ] `cargo test -p reddb` clean
- [ ] `EXPLAIN ANALYZE SELECT ...` returns populated tree via HTTP + CLI
- [ ] Human review → proceed to Phase 2

### Phase 2 — Quick wins + richer indexes (2-3 wk)

- **T4** RETURNING on INSERT/UPDATE/DELETE (#1a)
- **T5** Savepoints runtime wiring (#8a)
- **T6** Advisory locks (#8b)
- **T7** Partial indexes (#12)
- **T8** Expression indexes (#12)
- **T9** Covering indexes (index-only scan) (#12)

### Checkpoint B (after T4-T9)

- [ ] All new scenarios pass: `returning_throughput`, `savepoint_rollback`, `advisory_lock_contention`, `index_advanced` (3 sub-tests)
- [ ] `make check-baseline` — no regression >10% on existing perf metrics
- [ ] Human review → plan Phase 3 (#1b/c/d/e/f, #6, #7 etc.)

---

## Tasks

### T1: Secondary index maintenance on write path

**Description:** Every INSERT/UPDATE/DELETE on a row with indexed columns must propagate to every secondary index in the same WAL record (atomicity). Today, index created via `CREATE INDEX` does not receive post-creation writes → stale reads.

**Acceptance:**
- [ ] Inline indexes (BTree, Hash, DocumentPathValue) updated synchronously on write
- [ ] Vector / fulltext indexes (HNSW, Inverted) flagged for background rebuild (lazy OK)
- [ ] Insert → query via index returns the new row within same txn
- [ ] WAL replay reconstructs index state after crash

**Verification:**
- [ ] New unit test `src/storage/engine/btree/tests.rs::secondary_index_maint` — insert 1k, create idx, insert +1k, filtered query returns 2k
- [ ] `cargo test -p reddb storage::engine`
- [ ] `make mini-duel` → `mixed_workload_indexed` reddb_hybrid passes
- [ ] Crash recovery: kill mid-insert, restart, index consistent with heap

**Dependencies:** None

**Files:**
- `src/storage/engine/btree/impl.rs` (hot-path hook)
- `src/storage/schema/registry.rs` (expose `IndexDef` list per table)
- `src/storage/schema/table.rs` (IndexDef)
- `src/index.rs` (`SecondaryIndex::insert_entry`/`remove_entry`)
- `src/storage/wal/writer.rs`, `src/storage/wal/record.rs` (batch layout)

**Scope:** L → split if hits >8 files. Likely sub-slice by index kind.

---

### T2: EXPLAIN (plan tree)

**Description:** `EXPLAIN <query>` returns operator tree with type, estimated rows, estimated cost per node. No runtime stats yet.

**Acceptance:**
- [ ] `EXPLAIN SELECT ...` parses
- [ ] Response is JSON (HTTP) + text (CLI) with nodes `{op, table?, index?, est_rows, est_cost}`
- [ ] Index-backed query shows `IndexScan` node with index name

**Verification:**
- [ ] `red sql "EXPLAIN SELECT ..." ` prints tree
- [ ] `curl -X POST /query -d '{"sql":"...","explain":"plan"}'` returns JSON tree
- [ ] Unit test parses tree, asserts `IndexScan` present for `WHERE indexed_col = ?`

**Dependencies:** None

**Files:**
- `src/storage/query/planner/mod.rs` (expose plan-AST)
- `src/storage/query/sql.rs` or `parser/explain.rs` (new) — EXPLAIN token
- `src/storage/query/ast.rs` (EXPLAIN variant)
- `src/server/http/*` (endpoint flag)
- `src/grpc.rs` + proto (new `ExplainMode` enum)

**Scope:** M

---

### T3: EXPLAIN ANALYZE

**Description:** Adds `actual_rows`, `actual_ms`, loop counts to each plan node. Runs the query, instruments executor.

**Acceptance:**
- [ ] `EXPLAIN ANALYZE` fields populated for every node
- [ ] Overhead <10% vs normal run on a 100k-row scan

**Verification:**
- [ ] New scenario `bench-scenarios/src/explain_check.rs`: run ANALYZE, parse, assert `actual_rows > 0` and `actual_ms > 0` for scan node
- [ ] Overhead benchmark: same query with/without ANALYZE, ratio <1.10

**Dependencies:** T2

**Files:**
- `src/storage/query/executor.rs` or `executors/*` (timer + row counter hooks)
- `src/storage/query/planner/mod.rs` (attach runtime stats to nodes)

**Scope:** M

---

### T4: RETURNING clause

**Description:** `INSERT/UPDATE/DELETE ... RETURNING col, col, *` returns affected rows. Engine already materializes row copies; this surfaces them.

**Acceptance:**
- [ ] Parser accepts `RETURNING` list on all three DML statements
- [ ] Response shape matches SELECT (same row format)
- [ ] `RETURNING *` returns all columns

**Verification:**
- [ ] Parser unit tests in `src/storage/query/parser/tests.rs`
- [ ] End-to-end: `INSERT INTO t VALUES (1,2) RETURNING id` returns `{id:1}`
- [ ] New scenario `returning_throughput.rs` — measure ops/sec vs plain INSERT

**Dependencies:** None

**Files:**
- `src/storage/query/parser/dml.rs`
- `src/storage/query/ast.rs` (DML variants + `returning: Vec<Expr>`)
- `src/storage/query/executors/*` (collect + emit affected rows)
- `src/storage/query/lexer.rs` (RETURNING token — if missing)

**Scope:** S

---

### T5: Savepoints runtime wiring

**Description:** Parser already emits `TxnControl::Savepoint/RollbackToSavepoint/ReleaseSavepoint` (sql.rs:1181-1202). Runtime must implement stack of intra-txn rollback points.

**Acceptance:**
- [ ] `SAVEPOINT sp1; ...; ROLLBACK TO sp1` leaves txn alive with pre-sp1 state
- [ ] `RELEASE sp1` drops the mark, work kept
- [ ] Nested savepoints work (stack)
- [ ] Commit drains all marks

**Verification:**
- [ ] New scenario `savepoint_rollback.rs`: seeds, savepoint, dirty writes, rollback, commit; asserts dirty writes gone
- [ ] Unit test in `src/runtime/impl_core.rs` or `src/storage/wal/transaction.rs::tests`

**Dependencies:** None

**Files:**
- `src/storage/wal/transaction.rs` (savepoint stack — WAL LSN marks)
- `src/runtime/impl_core.rs` (dispatch TxnControl variants)

**Scope:** M

---

### T6: Advisory locks

**Description:** `pg_advisory_lock(bigint)` / `pg_advisory_unlock(bigint)`. Session-scoped hash map of held locks; block or return bool on contention.

**Acceptance:**
- [ ] `SELECT pg_advisory_lock(42)` blocks a second session until release
- [ ] `SELECT pg_try_advisory_lock(42)` returns bool, never blocks
- [ ] Session close releases all held locks

**Verification:**
- [ ] New scenario `advisory_lock_contention.rs`: N sessions fight for same key, assert mutual exclusion
- [ ] Unit test `src/auth/locks.rs::tests`

**Dependencies:** None

**Files:**
- `src/auth/locks.rs` (new) — global `DashMap<i64, Mutex<()>>` keyed by lock id + session id set
- `src/storage/query/function_catalog` or equivalent — register `pg_advisory_*` as built-ins
- `src/runtime/impl_core.rs` — on session drop, release

**Scope:** S

---

### T7: Partial indexes

**Description:** `CREATE INDEX idx ON t (col) WHERE pred` — only rows satisfying `pred` go into the index.

**Acceptance:**
- [ ] Parser accepts `WHERE <pred>` after column list
- [ ] Write path: evaluate `pred` on insert/update; skip if false
- [ ] Planner uses index only when query predicate implies index predicate

**Verification:**
- [ ] Unit test: create partial idx `WHERE deleted = false`; insert mixed; asserted index size = non-deleted count
- [ ] Scenario `index_advanced::partial` — measure ops/sec vs full index

**Dependencies:** T1 (write-path hook), T2 (planner matching)

**Files:**
- `src/storage/query/parser/index_ddl.rs`
- `src/storage/schema/table.rs` (`IndexDef.predicate: Option<Expr>`)
- `src/index.rs`
- `src/storage/engine/btree/impl.rs` (respect predicate)
- `src/storage/query/planner/index_only.rs` (predicate implication check)

**Scope:** M

---

### T8: Expression indexes

**Description:** `CREATE INDEX idx ON t (lower(email))` — key = function result.

**Acceptance:**
- [ ] Parser accepts expression in column list
- [ ] Write path computes expr and uses it as key
- [ ] Planner matches queries that contain same expression verbatim

**Verification:**
- [ ] Unit test: create `idx ON t (lower(email))`; `SELECT ... WHERE lower(email)='x'` uses index (verify via EXPLAIN from T2)
- [ ] Scenario `index_advanced::expression`

**Dependencies:** T1, T2

**Files:**
- `src/storage/query/parser/index_ddl.rs`
- `src/storage/schema/table.rs` (`IndexDef.key_expr: Option<Expr>`)
- `src/storage/engine/btree/impl.rs` (evaluate expr on write)
- `src/storage/query/planner/index_only.rs` (expr match)

**Scope:** M

---

### T9: Covering indexes (index-only scan)

**Description:** `CREATE INDEX idx ON t (a) INCLUDE (b, c)` — index stores extra payload; query selecting only `a`, `b`, `c` skips heap.

**Acceptance:**
- [ ] Parser accepts `INCLUDE (col, ...)`
- [ ] Write path stores payload in leaf
- [ ] Planner picks index-only scan when projection ⊆ (key cols ∪ included)
- [ ] EXPLAIN shows `IndexOnlyScan`

**Verification:**
- [ ] Scenario `index_advanced::covering` — compare ops/sec: covered vs non-covered on 1M rows
- [ ] Unit test: `EXPLAIN SELECT a,b FROM t WHERE a=?` emits `IndexOnlyScan` when `INCLUDE (b)` present

**Dependencies:** T1, T2

**Files:**
- `src/storage/query/parser/index_ddl.rs`
- `src/storage/schema/table.rs` (`IndexDef.included: Vec<String>`)
- `src/storage/engine/btree/impl.rs` (extended leaf payload)
- `src/storage/query/planner/index_only.rs` (IndexOnlyScan op)
- `src/storage/query/executors/indexed_scan.rs`

**Scope:** M-L

---

## Parallelization

- Critical path: **T1 → T2 → T3 → T7/T8/T9** (single dev or single agent lane)
- Independent lane: **T4, T5, T6** can run in parallel with T1-T3
- Two-dev split suggested: dev A takes critical path, dev B takes T4+T5+T6

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| T1 touches BTree hot path → regressions | H | Lock baseline before (`make lock-baselines`); gate on <10% regression |
| EXPLAIN ANALYZE overhead >10% | M | Behind flag; only instrument when requested |
| Partial/expression indexes need predicate-implication logic (undecidable in general) | M | MVP: exact-match of expression string; reject non-trivial implications |
| Advisory locks session scope needs session tracking the runtime may not have | M | If missing, scope to txn for MVP, document |
| Covering scan diverges WAL layout | H | Version leaf format byte; reject mixed-format reads |

## Open questions (need human)

1. Session model: does runtime already have per-session state for advisory locks, or do we scope to txn?
2. Proto breaking change OK for `ExplainMode` in gRPC, or new RPC?
3. For T1, is background rebuild of HNSW/Inverted indexes acceptable (eventual consistency on vector search) or strict inline?
4. Baseline lock: should we run `make lock-baselines` on the current tip or wait for a clean `main`?

## Gate

No code on any task until human approves this plan.
