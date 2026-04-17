# Spec: Performance Parity Push + Feature Gap Closure

Status: **APPROVED — Phase 1 (SPECIFY) complete**. Awaiting
implementation kick-off on Phase 2 (PLANNING → tasks/plan.md).
Owner: @filipeforattini · Approved: 2026-04-17

---

## Objective

Close the measured performance gap between RedDB and reference row-store
databases in `benches/bench_definitive_dual.py` under **synchronous
durability**. The target is PG-equivalent sync performance, not
async-masked wins — async mode remains an opt-in tier for callers that
explicitly trade durability for throughput, not the default or the
primary measurement.

Preserved invariants:

- Cross-model MVCC (tables + graph + vector + queue + timeseries in one BEGIN)
- Existing on-disk format compatibility (migration allowed, data-loss not)
- Every correctness test currently passing (35 integration tests)

Secondary: unblock 7 remaining categories from the parity survey
(PK/FK enforcement, window functions, JSON PG-compat operators,
COPY TO, streaming replication, autovacuum daemon, multi-platform
builds). Each gets its own spec later.

### Why now

Current bench (contaminated by pre-`b1d22e3` red_stats corruption, but
directionally correct):

| Scenario | PG ops/s | RedDB ops/s | Gap |
|----------|---------:|------------:|----:|
| insert_bulk | 86,782 | 8,474 | 10.2× |
| insert_sequential | 1,443 | 336 | 4.3× |
| bulk_update | 45,596 | 943 | 48× |
| select_range | 114 | 9 | 12.7× |
| select_complex | 1,085 | 55 | 19.7× |
| concurrent | 6,523 | 98 | 66× |

Rebench under clean binary deferred — first-pass optimisations use
these numbers; we'll rebench at P6.

## Success criteria

Post-implementation, on the same bench harness in the same Docker
environment, **sync mode** reaches ≤ 1.5× slower than the reference DB
in the canonical scenarios. Stretch: within 10%.

Acceptance cuts (sync mode, both databases at their defaults for the
equivalence comparison — defaults tuned via our Docker image):

- [ ] insert_bulk gap ≤ 1.5× (from 10.2×)
- [ ] bulk_update gap ≤ 1.5× (from 48×)
- [ ] concurrent gap ≤ 1.5× (from 66×)
- [ ] select_range gap ≤ 1.5× (from 12.7×)
- [ ] select_complex gap ≤ 1.5× (from 19.7×)
- [ ] 0/28 scenarios fail on BulkInsertBinary or visibility (was 7/28)
- [ ] 35/35 existing integration tests stay green
- [ ] One tuned Docker image shipped: `reddb:latest` + override via env vars +
      mountable config file, no companion PG image
- [ ] Each optimisation lands with a bench delta in the commit message

Async mode (opt-in via `durability.mode = 'async'`) treated as
additional win, measured separately, not gating.

## Tech stack + conventions (no change)

- **Language:** Rust (existing crate)
- **Bench harness:** `benches/bench_definitive_dual.py` (Python +
  Docker Compose)
- **Source inspiration:** `node_modules/postgres/src/backend/` (C) and
  `node_modules/mongo/src/mongo/` (C++) — architectural reference only
- **Build:** `cargo build --release --bin red` (~8 min, 28 MB binary)
- **Regression net:** 35 integration tests in `tests/e2e_*`

## Pre-existing infrastructure discovered during planning

- **`src/storage/transaction/lock.rs`** — complete `LockManager` with
  intent-lock modes (IS/IX/S/X/SIX), compatibility matrix, deadlock
  detection, wait queue. **Zero callers outside unit tests.** We wire,
  not design.
- **`src/storage/cache/bgwriter.rs`** — `BgWriter` + `DirtyPageFlusher`
  + `spawn()` entry point, documented as "post-MVP wiring". **Zero
  callers.** We wire, not design.
- **`graphify-out/GRAPH_REPORT.md` Community 45** — "Lehman-Yao
  right-link scheme (post-MVP)" already documented as planned BTree
  extension.

This shrinks the build-from-scratch effort substantially. The spec's
phases reflect wiring existing infrastructure + writing HOT updates +
multi-row insert + Lehman-Yao.

## Operational model — one RedDB image, many configurations

Only one Docker image ships: `reddb:latest`. It is opinionated and
powerful by default. Users shape behaviour via two mechanisms:

1. **Environment variables** — `REDDB_<DOTTED_KEY_UPPERCASED>`. Example:
   `REDDB_DURABILITY_MODE=sync`, `REDDB_STORAGE_BTREE_LEHMAN_YAO=true`.
   Dots map to underscores.
2. **Mounted config file** — `/etc/reddb/config.toml` (or `.json`).
   Parsed at boot, keys written into `red_config` only when absent.

**Precedence (highest wins):** env var → mounted file → persisted
`red_config` value → hard-coded default. Env vars are re-evaluated on
every boot; they override everything so operators can hot-fix a
live-config mistake by restarting with `REDDB_X=Y`.

## Configuration matrix

Two tiers. The distinction drives whether the value shows up in
`SHOW CONFIG` out-of-the-box.

### Tier A — critical / self-healing

On boot, if missing, RedDB writes the default into `red_config`. Always
listed in `SHOW CONFIG`. Operators see them.

| Key | Default | Effect |
|-----|---------|--------|
| `durability.mode` | `sync` | `sync` blocks until fsync; `async` returns on WAL buffer insert |
| `concurrency.locking.enabled` | `true` | Intent-lock hierarchy on; `false` falls back to global mutex (emergency rollback) |
| `storage.wal.max_interval_ms` | `10` | Ceiling on group-commit batching delay |
| `storage.bgwriter.delay_ms` | `200` | Background-writer scan cadence |
| `storage.btree.lehman_yao` | `true` | Right-link concurrent B-tree reads |

### Tier B — optional / in-memory default

Consulted with an in-memory default when not set. Never self-populated.
Appear in `SHOW CONFIG` only after a user has written them.

| Key | Default | Effect |
|-----|---------|--------|
| `concurrency.locking.deadlock_timeout_ms` | `5000` | Kill stuck txn after this |
| `storage.wal.min_batch_size` | `4` | Min concurrent waiters to trigger early flush |
| `storage.bgwriter.max_pages_per_round` | `100` | Soft cap on pages flushed per scan |
| `storage.bgwriter.lru_multiplier` | `2.0` | Aggressive adaptive scaling |
| `storage.bulk_insert.max_buffered_rows` | `1000` | `insert_many` buffer capacity |
| `storage.bulk_insert.max_buffered_bytes` | `65536` | Buffer byte cap |
| `storage.hot_update.max_chain_hops` | `32` | Scan walk limit on t_ctid chains |

No `perf.*` namespace. No `*.enabled` flags on individual optimisations
— each ships turned on. If an optimisation breaks something, it gets
reverted, not flag-disabled.

## Architectural reference — what we're borrowing

| Pattern | Source | Why it matters here |
|---------|--------|---------------------|
| Intent lock hierarchy (IS/IX/S/X) — writers on different collections don't block | MongoDB `lock_manager_defs.h`, `locker.cpp::PartitionedInstanceWideLockStats` | Core fix for the 66× concurrent gap. We have the primitive — need to wire it |
| WAL writer daemon + group commit (`CommitDelay`/`CommitSiblings`) | PG `xlog.c::XLogFlush`, `xloginsert.c::XLogInsert` | fsync amortisation under concurrent sync writers |
| HOT (Heap-Only Tuple) updates — same-page, no indexed column changed | PG `heapam.c::heap_update` lines 3976-4031 | 48× bulk_update gap |
| `heap_multi_insert` — 1000 tuples + single WAL record | PG `heapam.c::heap_multi_insert`, `copyfrom.c::CopyMultiInsertBufferFlush` | insert_bulk throughput |
| Lehman-Yao B-tree (right-link, readers pin-only) | PG `nbtree/README`, `nbtinsert.c::_bt_split` | Index path on concurrent mutate; now mandatory, not conditional |
| `JournalFlusher` — `SharedPromise` coalesces waiters on one fsync | MongoDB `journal_flusher.cpp` line 131 | Rust async mapping for group commit |
| `SnapshotManager` — separate `_committedSnapshot` / `_lastApplied` | MongoDB `wiredtiger_snapshot_manager.h` | Clean read-your-writes semantics |

## Phases

### Phase 0 — Operational scaffolding

**Goal:** ship the tuned RedDB image + bench reproduction path. No
rebench now; we use contaminated baseline and rebench at P6.

**Scope:**
- Docker image `reddb:latest` with env-var + config-file overlay
- Config matrix self-healing on boot
- `docs/engine/perf-bench.md` reproduction guide

### Phase 1 — Wire LockManager

**Goal:** break implicit global write serialisation. Primary target:
concurrent 66× → ≤ 1.5×.

**Scope:**
- Add `Arc<LockManager>` to `RuntimeInner`
- New `src/runtime/locking.rs` with `Resource` + `LockerGuard` RAII
- Wire reads to `(Global,IS) → (Collection,IS)`
- Wire writes to `(Global,IX) → (Collection,IX)`
- Wire DDL to `(Global,IX) → (Collection,X)`
- Deadlock stress test

**Commits land the optimisation on by default** — no `enabled` flag.
If broken, revert the commit.

### Phase 2 — WAL group commit (sync) + async opt-in tier

**Goal:** amortise fsync across concurrent sync writers. Primary
target: insert_bulk 10× → ≤ 1.5× sync.

**Scope:**
- `src/storage/wal/group_commit.rs` with `GroupCommitFlusher`
- `Database::open` spawns the flusher
- Sync commit routes through flusher; flushes every
  `storage.wal.max_interval_ms` or at `storage.wal.min_batch_size`
  pending writers
- `durability.mode = 'async'` as opt-in tier; startup banner + docs
  clarify trade-off

Sync tier must match PG sync throughput ±1.5×. Async tier measured
separately as additional capability, not gating.

### Phase 3 — HOT-like in-place updates

**Goal:** stop doing DELETE+INSERT when no indexed column changed and
the new row fits on the same page. Primary target: bulk_update 48× →
≤ 1.5×.

**Scope:**
- `src/storage/engine/hot_update.rs` pure decision helper
- Page-local UPDATE apply with `HEAP_ONLY_TUPLE` flag + t_ctid chain
- `execute_update` consults decide() per row
- Scan path follows t_ctid chains with `storage.hot_update.max_chain_hops`
  safety limit

### Phase 4 — Multi-row insert batching

**Goal:** `insert_many` primitive with one WAL record + fused index
updates. Primary target: insert_bulk 10× → ≤ 1.5× (composed with P2).

**Scope:**
- `UnifiedStore::insert_many(collection, Vec<Entity>)`
- Batched secondary-index updates (sorted B-tree bulk_insert, pre-
  allocated hash buckets)
- gRPC `BulkInsertBinary` + `COPY FROM` routed through `insert_many`

### Phase 5 — Lehman-Yao B-tree (REQUIRED)

**Goal:** eliminate reader-blocking on index splits. Primary target:
select_range / select_complex / indexed-lookup scenarios → ≤ 1.5×.

Not conditional. The technique is net positive on BTree concurrent
workloads; if measurement after P1–P4 already shows small gaps,
Lehman-Yao still ships — gain shows up under future load.

**Scope:**
- Leaf pages gain `high_key` + `right_link`
- STORE_VERSION_V8 with boot-time migration from V7
- Read descent pins pages, no locks; follows `right_link` on
  concurrent split
- Split holds page-exclusive locally only

### Phase 6 — Wire bgwriter, rebench, document

**Goal:** final wiring + real measurement + docs.

**Scope:**
- Existing `bgwriter::spawn` called from `Database::open`
- Two clean bench runs post all phases; before/after table
- `CHANGELOG.md` + `docs/release-notes-*.md` + `docs/engine/perf-*.md`

## Boundaries

**Always:**
- Bench before + after every phase; commit message carries the delta
- Run full regression net (35 tests) before commit
- Every new lock has a comment stating its position in the lock order
- Env var + config file override paths documented for every Tier A key

**Ask first:**
- Anything that bumps `STORE_VERSION` (on-disk format change). P5 does
  this; spec pre-approves V8. Other phases must not.
- Adding a new crate dependency
- Touching `src/wire/` (wire protocol)
- Changing public API in `src/runtime.rs` re-exports

**Never:**
- Merge a perf commit without a recorded bench delta
- Skip the regression net because "the optimisation is obviously correct"
- Break existing ASK / RLS / tenancy / cross-model-tx tests
- Default `durability.mode` to `async` — sync is safe, async is
  opt-in with a startup banner

## Resolved open questions

1. **Rebench timing** → use contaminated baseline, rebench at P6.
2. **Tuned image strategy** → single `reddb:latest` image, env-var +
   mount overrides. No companion PG image.
3. **Durability default** → `sync` (matches PG default). Async is
   opt-in tier.
4. **Feature flags** → none per optimisation. Each ships on by
   default. Rollback = git revert.
5. **Lehman-Yao** → mandatory, not conditional.
6. **Namespace** → `concurrency.*` / `storage.*` / `durability.*`.
   No `perf.*`.
7. **Config matrix** → two tiers (A self-healing, B in-memory
   default). See table above.

## Verification before implementation

- [x] Human reviews spec
- [x] Open questions 1–7 answered
- [x] Success criteria confirmed
- [x] Phase order approved
- [x] Spec saved to repo

---

*Phase 2 (tasks/plan.md) is next. Implementation kicks off on P0.T1
once the plan is approved.*
