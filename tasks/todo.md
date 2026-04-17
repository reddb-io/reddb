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
- [x] **P1.T4** Wire writes to IX (DML + builders)
- [x] **P1.T5** Wire DDL to X
- [ ] **P1 checkpoint** — `concurrent` bench delta, no >5% regression single-threaded

## Phase 2 — WAL group commit + async tier

- [x] **P2.T1** Baseline WAL flush latency scaffolding (SKIPPED — infra pre-existed)
- [x] **P2.T2** `GroupCommitFlusher` + `Database::open` wiring (PRE-EXISTING `storage/wal/group_commit.rs` + `StoreCommitCoordinator`, wired)
- [x] **P2.T3** Route sync commit through flusher — default flipped from `Strict` to `WalDurableGrouped`
- [ ] **P2.T4** Async tier + startup banner (deferred, grouped-sync first)
- [ ] **P2 checkpoint** — sync `insert_bulk` ≤ 1.5×

## Phase 3 — HOT-like in-place updates

- [x] **P3.T1** `HotUpdateDecision` pure helper
  - Files: `src/storage/engine/hot_update.rs` (new)
- [x] **P3.T2/T3** decide() wired into `flush_applied_entity_mutation` — skips `index_entity_update` when HOT fires
- [~] **P3.T4** Chain-walking reader — DEFERRED: requires page-local in-place rewrite + t_ctid chain (storage engine redesign, out of session scope)
- [ ] **P3 checkpoint** — `bulk_update` bench delta (measure after P4)

## Phase 4 — Multi-row insert batching

- [x] **P4.T1** `UnifiedStore::bulk_insert` primitive (PRE-EXISTING, already wired via MutationEngine)
- [x] **P4.T2** Fused index update path — `index_entity_insert_batch`, one registry lock per batch
- [x] **P4.T3** gRPC `BulkInsertBinary` → `create_rows_batch` → MutationEngine (PRE-EXISTING)
- [~] **P4.T4** `COPY FROM` wiring — deferred, executor already routes through MutationEngine; parser already batches
- [ ] **P4 checkpoint** — `insert_bulk` ≤ 1.5× (composed with P2)

## Phase 5 — Lehman-Yao B-tree (DEFERRED)

Status: the existing BTree already has `next_leaf` (right-sibling
pointer) on leaf pages, which covers half the Lehman-Yao contract.
Missing: `high_key` separator + reader descent without page locks +
split-local exclusive locks. Ships with a bumped on-disk format
(STORE_VERSION_V8) + migration, which is multi-day work. Plan calls
it out as "required"; session delivers: decision logged, skeleton
not built. Re-open when select_range / select_complex measured gaps
remain > 2× post-P1/P2/P4.

- [~] **P5.T1** Leaf `high_key` + lock-free right-link descent — DEFERRED
- [~] **P5.T2** Lock-free read descent — DEFERRED
- [~] **P5.T3** Split holds page-exclusive locally only — DEFERRED
- [ ] **P5 checkpoint** — `select_range` / `select_complex` ≤ 1.5× (blocked on P5)

## Phase 6 — Seal

- [x] **P6.T1** Wire existing `bgwriter::spawn` from `Database::open`
  - Files: `src/storage/engine/database.rs`
- [ ] **P6.T2** Full rebench on clean binary — PENDING (separate session)
- [x] **P6.T3** CHANGELOG entry for Phases 0-4 + P6.T1
- [x] **P6.T4** Matrix keys documented in `docs/engine/perf-bench.md` (P0.T4)

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
