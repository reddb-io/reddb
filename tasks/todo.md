# Todo: Performance Parity Push

Derived from `tasks/plan.md` (revised 2026-04-17 post-approval). Every
task ships default-on in one commit. Rollback = git revert.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

## Gate 0 — approvals

- [x] Spec approved — `docs/spec-performance-parity-2026-04-17.md`
- [ ] Plan approved — `tasks/plan.md`
- [ ] Human says "start P0.T1"

## Phase 0 — Operational scaffolding

- [x] **P0.T1** Config matrix + self-healing loader
  - Acceptance: Tier A keys self-populate on boot; Tier B defaults in-memory
  - Verify: `cargo test --test e2e_config_matrix`
  - Files: `src/runtime/config_matrix.rs` (new), `src/runtime/impl_core.rs`, `tests/e2e_config_matrix.rs` (new)
- [x] **P0.T2** Env-var + config-file overlay
  - Acceptance: precedence env → file → red_config → default; malformed file logs warn, boot succeeds
  - Verify: `REDDB_DURABILITY_MODE=async` overrides file saying `sync`
  - Files: `src/runtime/config_overlay.rs` (new), `src/bin/red.rs`
- [x] **P0.T3** Tuned Docker image `reddb:latest`
- [x] **P0.T4** Bench reproduction guide
- [ ] **P0 checkpoint** — human review

## Phase 1 — Wire LockManager into write paths

Reuses existing `src/storage/transaction/lock.rs`.

- [x] **P1.T1** `Arc<LockManager>` on `RuntimeInner`
  - Acceptance: dormant, config from matrix, regression net green
  - Files: `src/runtime.rs`, `src/runtime/impl_core.rs`
- [x] **P1.T2** `src/runtime/locking.rs` — `Resource` + `LockerGuard`
  - Acceptance: compatibility-matrix unit tests + 50-thread stress, no deadlocks
  - Files: `src/runtime/locking.rs` (new), `src/runtime.rs`
- [x] **P1.T3** Wire reads to IS (Select/Join/Vector/Hybrid/Graph/Path)
  - Acceptance: regression net green; lock stats bump on SELECT
  - Files: `src/runtime/impl_core.rs`, `tests/e2e_locking_reads.rs` (new)
- [ ] **P1.T4** Wire writes to IX (DML + builders)
  - Acceptance: 20×1000 inserts across 5 collections without serialisation
  - Files: `src/runtime/impl_core.rs`, `src/runtime/impl_dml.rs`, `src/runtime/impl_queue.rs`, `src/runtime/impl_graph.rs`, `tests/e2e_concurrent_writes.rs` (new)
- [ ] **P1.T5** Wire DDL to X
  - Acceptance: ALTER blocks INSERT; INSERT resumes after
  - Files: `src/runtime/impl_ddl.rs`, `tests/e2e_ddl_concurrency.rs` (new)
- [ ] **P1 checkpoint** — `concurrent` bench delta, no >5% regression single-threaded

## Phase 2 — WAL group commit + async tier

- [ ] **P2.T1** Baseline WAL flush latency scaffolding
  - Files: `tests/bench_wal_flush.rs` (scaffolding)
- [ ] **P2.T2** `GroupCommitFlusher` + `Database::open` wiring
  - Acceptance: task spawns; no writer uses it yet; regression net green
  - Files: `src/storage/wal/group_commit.rs` (new), `src/storage/engine/database.rs`
- [ ] **P2.T3** Route sync commit through flusher
  - Acceptance: 100 threads × 1 commit → ≤ 5 fsyncs
  - Files: `src/storage/wal/*.rs`, `tests/e2e_group_commit.rs` (new)
- [ ] **P2.T4** Async tier + startup banner, delete scaffolding
  - Acceptance: kill-test confirms bounded loss in async only
  - Files: `src/service_cli.rs`, `tests/e2e_async_commit_crash.rs` (new), delete `tests/bench_wal_flush.rs`
- [ ] **P2 checkpoint** — sync `insert_bulk` ≤ 1.5×; async measured separately

## Phase 3 — HOT-like in-place updates

- [ ] **P3.T1** `HotUpdateDecision` pure helper
  - Files: `src/storage/engine/hot_update.rs` (new)
- [ ] **P3.T2** `apply_hot_update` storage primitive with HEAP_ONLY_TUPLE + t_ctid
  - Files: `src/storage/unified/store/impl_entities.rs`, `src/storage/engine/hot_update.rs`
- [ ] **P3.T3** Route `execute_update` through decide()
  - Files: `src/runtime/impl_dml.rs`, `tests/e2e_hot_update.rs` (new)
- [ ] **P3.T4** Chain-walking reader, bounded by `storage.hot_update.max_chain_hops`
  - Files: `src/runtime/query_exec/table.rs`, `tests/e2e_hot_update_concurrent.rs` (new)
- [ ] **P3 checkpoint** — `bulk_update` ≤ 1.5×

## Phase 4 — Multi-row insert batching

- [ ] **P4.T1** `UnifiedStore::insert_many` primitive
  - Files: `src/storage/unified/store/impl_entities.rs`
- [ ] **P4.T2** Fused index update path
  - Files: `src/runtime/index_store.rs`
- [ ] **P4.T3** gRPC `BulkInsertBinary` → `insert_many`
  - Files: `src/wire/listener.rs`
- [ ] **P4.T4** `COPY FROM` → `insert_many`
  - Files: `src/runtime/impl_ddl.rs`, `tests/e2e_copy_from.rs` (new)
- [ ] **P4 checkpoint** — `insert_bulk` ≤ 1.5× (composed with P2)

## Phase 5 — Lehman-Yao B-tree (REQUIRED)

- [ ] **P5.T1** Leaf `high_key` + `right_link` + V8 migration
  - Files: `src/storage/engine/btree.rs`, `src/storage/engine/btree/impl.rs`, `src/storage/engine/page.rs`, `tests/e2e_btree_v8_migration.rs` (new)
- [ ] **P5.T2** Lock-free read descent
  - Files: `src/storage/engine/btree/impl.rs`, `tests/e2e_btree_concurrent.rs` (new)
- [ ] **P5.T3** Split holds page-exclusive locally only
  - Files: `src/storage/engine/btree/impl.rs`, extend `e2e_btree_concurrent.rs`
- [ ] **P5 checkpoint** — `select_range` / `select_complex` ≤ 1.5×; V8 migration green

## Phase 6 — Seal

- [ ] **P6.T1** Wire existing `bgwriter::spawn` from `Database::open`
  - Files: `src/storage/engine/database.rs`
- [ ] **P6.T2** Full rebench on clean binary
  - Files: `benches/final-2026-MM-DD.json`
- [ ] **P6.T3** CHANGELOG + release notes + perf doc
  - Files: `CHANGELOG.md`, `docs/release-notes-*.md`, `docs/engine/perf-*.md`
- [ ] **P6.T4** Document every matrix key with override recipe
  - Files: `docs/engine/perf-bench.md`

## Global exit criteria (sync mode)

- [ ] `concurrent` gap ≤ 1.5× (was 66×)
- [ ] `bulk_update` gap ≤ 1.5× (was 48×)
- [ ] `insert_bulk` gap ≤ 1.5× (was 10.2×)
- [ ] `select_range` gap ≤ 1.5× (was 12.7×)
- [ ] `select_complex` gap ≤ 1.5× (was 19.7×)
- [ ] 0/28 scenarios fail on seed (was 7/28)
- [ ] 35/35 existing integration tests green
- [ ] `reddb:latest` image ships with env-var + mount override
- [ ] Every matrix key documented with override recipe
- [ ] CHANGELOG + release notes written

## Out of scope for this spec

- PK/FK enforcement (separate spec)
- Window functions / CTE recursive (separate spec)
- JSON PG-compat operators (separate spec)
- COPY TO (separate spec)
- Streaming replication (separate spec)
- Autovacuum daemon (separate spec)
- Multi-platform builds (separate spec)
