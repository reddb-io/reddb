# Plan: Performance Parity Push

Status: **REVISED after spec approval 2026-04-17**. Incorporates the
7 resolved open questions:

- Sync is the primary KPI (async is opt-in tier, measured separately)
- No feature flags — each optimisation ships default-on, rollback = revert
- Lehman-Yao BTree is mandatory, not conditional
- Namespaces: `concurrency.*` / `storage.*` / `durability.*`
- Single Docker image with env-var + mount overrides
- Config matrix has two tiers (A self-healing, B in-memory default)
- Using contaminated baseline; final rebench at P6

## Dependency graph

```
┌──────────────────────────────────────────────────────────────┐
│ P0 · Operational scaffolding                                  │
│   - Tuned reddb:latest image (env-var + mount overlay)        │
│   - Config matrix boot-time self-healing                      │
│   - Bench reproduction guide                                  │
└────────────────┬─────────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────────────────────────┐
│ P1 · Wire LockManager into write paths                        │
│   - concurrency.locking.* keys                                │
│   - IS/IX reads, IX writes, X DDL                             │
│   - RAII guard, ordered acquisition                           │
│   - Target: concurrent 66× → ≤1.5×                            │
└────────────────┬─────────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────────────────────────┐
│ P2 · WAL group commit + async tier                            │
│   - storage.wal.* keys; durability.mode                       │
│   - GroupCommitFlusher background task                        │
│   - Sync-mode target: insert_bulk 10× → ≤1.5×                 │
└────────────────┬──────────────────┬──────────────────────────┘
                 │                  │
        ┌────────▼─────────┐   ┌────▼────────────────────────┐
        │ P3 · HOT-like    │   │ P4 · Multi-row insert       │
        │    updates       │   │    batching                 │
        │  storage.hot_    │   │  storage.bulk_insert.*      │
        │  update.max_     │   │  keys                       │
        │  chain_hops      │   │  Target: insert_bulk        │
        │  Target: bulk_   │   │  (composed w/ P2) ≤1.5×     │
        │  update ≤1.5×    │   │                             │
        └────────┬─────────┘   └────────┬────────────────────┘
                 │                      │
                 └──────────┬───────────┘
                            ▼
                 ┌───────────────────────────────────┐
                 │ P5 · Lehman-Yao B-tree (REQUIRED) │
                 │   storage.btree.lehman_yao = true │
                 │   STORE_VERSION_V8 + migrator     │
                 │   Target: select_* ≤1.5×          │
                 └──────────┬────────────────────────┘
                            ▼
                 ┌───────────────────────────────────┐
                 │ P6 · Wire bgwriter · rebench ·    │
                 │      CHANGELOG + release notes    │
                 └───────────────────────────────────┘
```

**Parallel-safe:** P3 and P4 don't touch each other's files.
**Must-serial:** P0 → P1 → (P2 → {P3,P4}) → P5 → P6.

## Vertical slicing principle

Every task below is one complete path through the stack:

1. Committable independently (build + regression net + bench delta)
2. Default-on at commit (no feature flags)
3. Touches ≤ ~5 files
4. Has explicit acceptance criteria and a verification command

## Phase 0 — Operational scaffolding

### P0.T1 — Config matrix + self-healing loader

- **Acceptance:** New `src/runtime/config_matrix.rs` declares Tier A
  keys with defaults; on boot the loader writes missing Tier A keys
  into `red_config` (idempotent). Tier B keys read with in-memory
  default without persistence. `SHOW CONFIG` after fresh boot lists all
  Tier A keys with their defaults.
- **Verify:** `cargo test --test e2e_config_matrix` (new) — fresh
  in-memory runtime, boot, assert `SHOW CONFIG durability.mode` =
  `'sync'`.
- **Files:** `src/runtime/config_matrix.rs` (new ~150 LOC),
  `src/runtime/impl_core.rs` (boot hook), `tests/e2e_config_matrix.rs`
  (new).

### P0.T2 — Env-var + config-file overlay

- **Acceptance:** Boot resolution order: env `REDDB_<UP_KEY>` →
  `/etc/reddb/config.toml` → `red_config` persisted → default. Env
  vars re-evaluated every boot and always win. Missing file is
  silent; malformed file logs a warning and is ignored (boot still
  succeeds using other tiers).
- **Verify:** Extend `tests/e2e_config_matrix.rs` — set
  `REDDB_DURABILITY_MODE=async`, confirm boot picks it up even with a
  config file saying `sync`.
- **Files:** `src/runtime/config_overlay.rs` (new), `src/bin/red.rs`.

### P0.T3 — Tuned Docker image

- **Acceptance:** `docker/reddb.Dockerfile` builds `reddb:latest` with
  release binary + opinionated defaults pre-baked via env vars in the
  Dockerfile itself. Users override via `-e REDDB_X=Y` or
  `-v path:/etc/reddb/config.toml`.
- **Verify:** `docker build -t reddb:test -f docker/reddb.Dockerfile .`
  + `docker run reddb:test ...` + `SHOW CONFIG` from a client against
  the container confirms tuned defaults.
- **Files:** `docker/reddb.Dockerfile`, `docker/entrypoint.sh`.

### P0.T4 — Bench reproduction guide

- **Acceptance:** `docs/engine/perf-bench.md` covers: prerequisites,
  `docker compose up`, scenario list, baseline numbers, env-var
  override recipes, known failure modes.
- **Verify:** Fresh checkout + follow guide literally → baseline
  reproduces within ±10%.
- **Files:** `docs/engine/perf-bench.md`.

### P0 checkpoint (human review)

- Baseline image builds and boots cleanly
- `SHOW CONFIG` lists all Tier A keys
- Env-var override verified

---

## Phase 1 — Wire LockManager into write paths

Reuses existing `src/storage/transaction/lock.rs` (complete,
currently unused).

### P1.T1 — Add `Arc<LockManager>` to `RuntimeInner`

- **Acceptance:** `RuntimeInner` owns one `Arc<LockManager>`. Boot
  reads `concurrency.locking.enabled` and
  `concurrency.locking.deadlock_timeout_ms` from the matrix. Dormant
  (no path touches it) for this commit.
- **Verify:** Regression net green.
- **Files:** `src/runtime.rs`, `src/runtime/impl_core.rs`.

### P1.T2 — `src/runtime/locking.rs` — Resource + LockerGuard RAII

- **Acceptance:** `Resource::{Global, Collection(String)}` and
  `LockerGuard` that acquires in order, releases in reverse on drop,
  rejects invalid escalations with a typed error.
- **Verify:** Unit tests for the compatibility matrix; 50-thread
  stress test with random acquire/release — zero deadlocks.
- **Files:** `src/runtime/locking.rs` (new ~200 LOC), `src/runtime.rs`.

### P1.T3 — Wire read path to IS

- **Acceptance:** `QueryExpr::{Table, Join, Vector, Hybrid, Graph,
  Path}` dispatches acquire `(Global,IS) → (Collection,IS)` before
  the scan. Guard scoped per statement.
- **Verify:** Regression net green. `tests/e2e_locking_reads.rs`
  (new) asserts lock-stats bump on a SELECT.
- **Files:** `src/runtime/impl_core.rs`, `tests/e2e_locking_reads.rs`.

### P1.T4 — Wire write paths to IX

- **Acceptance:** `Insert`/`Update`/`Delete` + graph/vector/queue/
  timeseries builders acquire `(Global,IX) → (Collection,IX)`.
- **Verify:** `tests/e2e_concurrent_writes.rs` (new) — 20 threads ×
  1000 inserts across 5 collections; no serialisation wall. Regression
  net green.
- **Files:** `src/runtime/impl_core.rs`, `src/runtime/impl_dml.rs`,
  `src/runtime/impl_queue.rs`, `src/runtime/impl_graph.rs`,
  `tests/e2e_concurrent_writes.rs`.

### P1.T5 — Wire DDL to X

- **Acceptance:** CREATE/DROP/ALTER TABLE, CREATE/DROP INDEX, policy
  DDL acquire `(Global,IX) → (Collection,X)`. Other threads on the
  same collection wait.
- **Verify:** `tests/e2e_ddl_concurrency.rs` (new) — ALTER + concurrent
  INSERT; INSERT waits then completes on post-ALTER schema.
- **Files:** `src/runtime/impl_ddl.rs`, `tests/e2e_ddl_concurrency.rs`.

### P1 checkpoint (human review)

- `concurrent` scenario bench delta recorded
- No ≥ 5% regression on single-threaded scenarios
- Deadlock stress test passes

---

## Phase 2 — WAL group commit (sync) + async tier

### P2.T1 — Baseline WAL flush latency measurement

- **Acceptance:** `tests/bench_wal_flush.rs` (scaffolding, removed in
  P2.T4) prints: 1-row commit latency, 100-row sequential commit,
  100-thread concurrent-1-row commit. Numbers recorded in commit
  message for pre/post comparison.
- **Files:** `tests/bench_wal_flush.rs`.

### P2.T2 — `GroupCommitFlusher` + `Database::open` wiring

- **Acceptance:** `src/storage/wal/group_commit.rs` defines the
  flusher with atomics `pending_lsn`/`flushed_lsn` and a background
  task spawned by `Database::open`. Reads
  `storage.wal.max_interval_ms` (Tier A) and
  `storage.wal.min_batch_size` (Tier B).
- **Verify:** `cargo check`, regression net green. Task runs but
  bypass is still intact (writers don't call through it yet).
- **Files:** `src/storage/wal/group_commit.rs` (new),
  `src/storage/engine/database.rs`.

### P2.T3 — Route sync commit through flusher

- **Acceptance:** Sync commits call `wait_for_flush(my_end_lsn)`
  instead of inline fsync. Flusher batches per matrix keys.
- **Verify:** `tests/e2e_group_commit.rs` (new) — 100 threads × 1
  commit → ≤ 5 fsyncs via flusher stats. `insert_bulk` +
  `insert_sequential` deltas recorded.
- **Files:** `src/storage/wal/*.rs`, `tests/e2e_group_commit.rs`.

### P2.T4 — Async commit tier + startup banner

- **Acceptance:** `durability.mode = 'async'` makes writers return
  without waiting for fsync. `red` binary logs a prominent startup
  banner: `⚠ RedDB running with durability.mode=async — up to X ms of
  writes may be lost on crash`. Remove the scaffolding bench test.
- **Verify:** `tests/e2e_async_commit_crash.rs` (new) — SIGKILL
  child process mid-stream; surviving data matches expectations.
- **Files:** `src/service_cli.rs`, `tests/e2e_async_commit_crash.rs`,
  delete `tests/bench_wal_flush.rs`.

### P2 checkpoint

- Sync `insert_bulk` gap ≤ 1.5× (the acceptance target)
- Async mode measured separately, recorded
- Kill-test matches sync vs async durability guarantees

---

## Phase 3 — HOT-like in-place updates

### P3.T1 — `HotUpdateDecision` pure helper

- **Acceptance:** `decide(rel, old, new_size, modified_cols) →
  HotUpdateDecision { can_hot, indexed_blocker, page_free_space }`.
  No side effects.
- **Verify:** Unit tests per branch.
- **Files:** `src/storage/engine/hot_update.rs` (new).

### P3.T2 — `apply_hot_update` storage primitive

- **Acceptance:** Writes the new row in the same page, sets the old
  row's `xmax` + `HEAP_ONLY_TUPLE` flag + `t_ctid` chain pointer.
  Secondary indexes untouched.
- **Verify:** Index snapshot before/after — zero secondary-index
  changes when the HOT path fires.
- **Files:** `src/storage/unified/store/impl_entities.rs`,
  `src/storage/engine/hot_update.rs`.

### P3.T3 — Route `execute_update` through `decide()`

- **Acceptance:** Per-row decision. Can-HOT rows take the fast path;
  others fall back to existing DELETE+INSERT.
- **Verify:** `tests/e2e_hot_update.rs` (new) — table without indexes,
  UPDATE → no index maintenance observed. Existing UPDATE regression
  tests green.
- **Files:** `src/runtime/impl_dml.rs`, `tests/e2e_hot_update.rs`.

### P3.T4 — Chain-walking reader with bounded hops

- **Acceptance:** Scan path follows `t_ctid` chains, picks visible
  version via MVCC, enforces `storage.hot_update.max_chain_hops`.
- **Verify:** `tests/e2e_hot_update_concurrent.rs` (new) — 10 threads
  hammer UPDATE + SELECT; no torn reads, no infinite loops.
- **Files:** `src/runtime/query_exec/table.rs`,
  `tests/e2e_hot_update_concurrent.rs`.

### P3 checkpoint

- `bulk_update` gap ≤ 1.5×
- Zero regression on UPDATE-with-indexed-col scenarios

---

## Phase 4 — Multi-row insert batching

### P4.T1 — `UnifiedStore::insert_many` primitive

- **Acceptance:** `insert_many(collection, Vec<Entity>) →
  Vec<Result<EntityId>>`. Single ID-bump covering all rows, greedy
  page-fill, one multi-insert WAL record. Reads
  `storage.bulk_insert.max_buffered_{rows,bytes}` for soft caps.
- **Verify:** Unit test — 10 000 rows → 1 WAL record, 1 ID bump.
- **Files:** `src/storage/unified/store/impl_entities.rs`.

### P4.T2 — Fused index update path

- **Acceptance:** After `insert_many` returns IDs, a single batched
  pass updates all registered secondary indexes (sorted B-tree
  bulk_insert per index, pre-allocated hash buckets).
- **Verify:** 10 000 rows into 3-index table → ≤ 3 index-insert
  calls total, not 30 000.
- **Files:** `src/runtime/index_store.rs`.

### P4.T3 — Route gRPC `BulkInsertBinary`

- **Acceptance:** Wire handler calls `insert_many` in one batch,
  acquires `(Global,IX) → (Collection,IX)` once.
- **Verify:** Existing `BulkInsertBinary` tests green + bench delta.
- **Files:** `src/wire/listener.rs`.

### P4.T4 — Route `COPY FROM`

- **Acceptance:** Parser-level `COPY FROM` executor buffers to matrix
  caps then flushes via `insert_many`.
- **Verify:** New `tests/e2e_copy_from.rs`. Bench delta.
- **Files:** `src/runtime/impl_ddl.rs`, `tests/e2e_copy_from.rs`.

### P4 checkpoint

- `insert_bulk` gap ≤ 1.5× (composed with P2)
- Other insert scenarios consistent

---

## Phase 5 — Lehman-Yao B-tree (REQUIRED)

### P5.T1 — Leaf page `high_key` + `right_link` + V8 migration

- **Acceptance:** Leaf header adds both fields. New format is
  `STORE_VERSION_V8`. Boot-time migrator converts V7 → V8 in-place
  with checksummed backup. Old-version open still works
  (up-converts on first write, read-only stays V7-compatible).
- **Verify:** `tests/e2e_btree_v8_migration.rs` (new) — fixture
  V7 dataset, boot, confirm V8 on-disk post-boot, confirm data
  integrity. Regression net green.
- **Files:** `src/storage/engine/btree.rs`,
  `src/storage/engine/btree/impl.rs`, `src/storage/engine/page.rs`.

### P5.T2 — Lock-free read descent

- **Acceptance:** Search pins leaf page; on `high_key` exceeded,
  follows `right_link` rather than restarting from root. No locks
  on read descent.
- **Verify:** `tests/e2e_btree_concurrent.rs` (new) — 10 reader +
  10 writer threads hammer the BTree; no torn reads, no deadlocks.
- **Files:** `src/storage/engine/btree/impl.rs`.

### P5.T3 — Split holds page-exclusive locally only

- **Acceptance:** Split acquires page-exclusive on the page being
  split + its parent; releases immediately after the split completes
  and `right_link` is set. Concurrent readers follow the right-link.
- **Verify:** Extend `e2e_btree_concurrent.rs` with a split-heavy
  workload (ascending keys forcing frequent splits).
- **Files:** `src/storage/engine/btree/impl.rs`.

### P5 checkpoint

- `select_range` / `select_complex` gap ≤ 1.5×
- BTree stress test green
- V8 migration green on fixture datasets

---

## Phase 6 — Seal

### P6.T1 — Wire `bgwriter::spawn` into `Database::open`

- **Acceptance:** Existing `src/storage/cache/bgwriter.rs::spawn`
  called from `Database::open` with matrix-sourced config. Handle
  stored on Database; joined on shutdown.
- **Verify:** `BgWriterStats::snapshot()` shows non-zero rounds +
  pages flushed after a bench run.
- **Files:** `src/storage/engine/database.rs`.

### P6.T2 — Full rebench on clean binary

- **Acceptance:** Two bench runs within ±5% of each other. JSON
  committed at `benches/final-2026-MM-DD.json`.
- **Verify:** Compare vs. contaminated baseline; deltas per scenario
  ≥ target.
- **Files:** `benches/final-*.json`.

### P6.T3 — CHANGELOG + release notes + perf doc

- **Acceptance:** `CHANGELOG.md`, `docs/release-notes-*.md`, and
  `docs/engine/perf-2026-MM-DD.md` publish before/after tables per
  phase commits + residual gaps.
- **Files:** docs.

### P6.T4 — Housekeeping

- **Acceptance:** Every Tier A/B config key documented in
  `docs/engine/perf-bench.md` with effect description and override
  recipe (env var + mount example).
- **Files:** docs.

---

## Rollback strategy

No feature flags. If a commit breaks something:

- Hot rollback = `git revert <sha>` + rebuild + redeploy.
- For STORE_VERSION bumps (P5 only): V8 image can always read V7
  files; V7 image cannot read V8. Document as one-way migration.

## Risk log

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| P1 deadlock detector has undiscovered bugs | Med | High — stalls | P1.T2 50-thread stress; deadlock_timeout_ms kills stuck txn |
| P2 async mode loses data users believed durable | Med | High — trust | Startup banner, docs, `SHOW CONFIG durability.mode` |
| P3 t_ctid chain walker loops under contention | Low | Med | `storage.hot_update.max_chain_hops = 32` safety |
| P4 `insert_many` breaks wire clients counting per-row errors | Med | Med | Return `Vec<Result<EntityId>>`, not all-or-nothing |
| P5 V8 migration corrupts V7 datasets | Med | Critical | Pre-migration checksummed backup; test against V7 fixture |
| Unflagged deploys — can't SHOW CONFIG disable | High | Med | Revert-to-ship pattern; CI builds are reproducible |

## Open questions

None. All 7 resolved by spec.

## Verification checklist before implementation

- [x] Human reviewed `tasks/plan.md`
- [x] Spec questions 1–7 resolved (see spec)
- [x] Success criteria numbers confirmed
- [x] Phase order approved
- [ ] Human says "start P0.T1"
