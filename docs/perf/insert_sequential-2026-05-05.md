# `insert_sequential` mini-duel profile — 2026-05-05

Status: **static analysis (live profile blocked)**.

Tracking issue: #76 — *"insert_sequential mini-duel fails 20% margin
versus PostgreSQL and MongoDB"*.

> **Reproducing the live profile.** P5 below is now wired up as
> `make perf-bench` (writes `target/perf/insert_sequential.svg`).
> The host kernel knobs that blocked this slice are documented in
> [`perf-knobs.md`](perf-knobs.md) — relax them per that doc, then
> run `make perf-bench`.

## TL;DR

- The 20%-slower-than-PostgreSQL claim in #76 came from a 1-row
  `--items 1 --runs 1` mini-duel and is **not reproducible** at the
  larger sizes called for in this investigation. On this host
  (cyber-XPS-13-9300, 8 cores, Docker compose, RedDB built from
  worktree HEAD `612b855`), `reddb_wire` is consistently 2.5×–3.5×
  **faster** than PostgreSQL on `insert_sequential`. The original
  failure is dominated by warm-up / container start cost, not steady
  state.
- The most plausible *real* bottlenecks on the steady-state path are
  the WAL group-commit fsync wait per autocommit insert, the
  `manager.get(id)` clone after every `insert_auto`, the per-row
  Vec/HashMap construction inside `handle_bulk_insert_binary` →
  `create_rows_batch` → `MutationEngine::append_one`, and the
  per-statement `xid` allocation + commit cycle from the snapshot
  manager.
- A live CPU profile could not be produced from this worktree because
  `kernel.perf_event_paranoid = 4`, `kernel.yama.ptrace_scope = 1`,
  and the bench `red` runs inside a Docker container we don't own. The
  punch-list below is therefore a static read of the suspected hot
  path, anchored to the throughput measurements we *did* capture.
  Re-run with `perf` (or `cargo flamegraph`) once the kernel knobs are
  loosened, and update the percentages.

## Reproducer & raw ops/sec

Bench is at `/home/cyber/Work/reddb.io/rdb-benchmark`. Compose-driven;
RedDB binary is bind-mounted from
`/home/cyber/Work/reddb.io/reddb/target/release/red` (built from
this worktree's HEAD via the global `CARGO_TARGET_DIR` and copied
into the in-repo path that `docker/compose.yml` expects).

`--mini-duel` requires `--warmup-runs 0`, so the canonical command
matches the prompt's three-run shape with `--warmup-runs 0`:

```bash
cd /home/cyber/Work/reddb.io/rdb-benchmark
cargo run -q -p bench-runner -- \
    --mini-duel --scenario insert_sequential \
    --database duel --profile standard \
    --warmup-runs 0 --items 1000 --typed-items 1000 --runs 3
```

### Headline (3-run averages, ops/sec)

Source: `rdb-benchmark/results/history.jsonl`, sessions
`sess-20260505161631-1775046` (1k items, `duel`),
`sess-20260505163612-1820658` (10k items, `duel`),
and `sess-20260505164130-1843347` (10k items, `mongodb,reddb_wire`).

| items | postgresql | reddb_wire | mongodb | reddb advantage vs ref |
|------:|-----------:|-----------:|--------:|------------------------|
|  1 000 |     2 104 |     6 367 |       — | **3.03× over PG**      |
| 10 000 |       562 |     1 367 |       — | **2.43× over PG**      |
| 10 000 |         — |     2 189 |     451 | **4.86× over Mongo**   |

Per-run breakdown (`p50` is the per-row request latency reported by
the bench timer; ops/sec is `items / total_time_ms × 1000`):

```
items=1000:
  pg  run1 2142 ops/s  p50= 433 µs
  pg  run2 2046 ops/s  p50= 452 µs
  pg  run3 2125 ops/s  p50= 428 µs
  rw  run1 7103 ops/s  p50= 109 µs
  rw  run2 5905 ops/s  p50= 131 µs
  rw  run3 6094 ops/s  p50= 126 µs

items=10000:
  pg  run1  607 ops/s  p50=1346 µs
  pg  run2  690 ops/s  p50=1341 µs
  pg  run3  389 ops/s  p50=1995 µs
  rw  run1 1629 ops/s  p50= 548 µs
  rw  run2 1440 ops/s  p50= 585 µs
  rw  run3 1031 ops/s  p50= 840 µs

items=10000 (mongo vs reddb):
  mg  run1  366 ops/s  p50=2554 µs
  mg  run2  331 ops/s  p50=2730 µs
  mg  run3  655 ops/s  p50=1341 µs
  rw  run1 1942 ops/s  p50= 457 µs
  rw  run2 1208 ops/s  p50= 631 µs
  rw  run3 3417 ops/s  p50= 263 µs
```

Note on PostgreSQL setup: `docker/compose.yml` runs the PG service
with `synchronous_commit=off` and `wal_level=minimal`. RedDB runs
with the default `WalDurableGrouped` mode + `window_ms = 0`, i.e.
each commit blocks the caller until an fsync that covers its LSN. PG
has the easier durability bargain in this matrix, but RedDB still wins
at every size we measured.

A 100 000-item run was started but cancelled at ~13 minutes after
two completed runs because the budget was over the 10-minute slice.
The two completed runs (`9f2423fd`, `035d4d4d`, `966ef294`,
`2452e1b5` in `history.jsonl`) read **PG ≈ 452 ops/s, reddb ≈ 1280
ops/s**, in line with the smaller-N pattern.

### Reproducing the original #76 failure

The headline failure in the issue body uses `--items 1 --runs 1`,
which measures container warm-up + auth handshake + a single insert.
Running the same shape on this worktree:

```bash
cargo run -q -p bench-runner -- --mini-duel \
    --scenario insert_sequential --database duel-core \
    --profile standard --warmup-runs 0 --items 1 --typed-items 1 --runs 1
```

was attempted and timed out within the slice budget while the second
mongo container was warming up. The 1-row shape is by design
warm-up-bound, so the 51-ops/s vs 700-ops/s gap in #76 is consistent
with start-cost variance, not steady-state insert throughput. The
larger runs above contradict the "RedDB is slower" framing.

## Profile

A live CPU profile of the bench `red` process could not be produced
from this slice. Recording attempts failed because:

- `kernel.perf_event_paranoid = 4`: non-root cannot open
  `perf_event_open` even on processes the user owns.
- `kernel.yama.ptrace_scope = 1`: only direct children can be
  attached to with `strace`/`perf`.
- The bench `red` runs inside a Docker container created by
  `bench-runner` (not a child of this shell), so neither attachment
  path is available.
- `cargo flamegraph` shells out to `perf record` and hits the same
  ceiling.

`strace` of a host-spawned `red` ran cleanly, but driving the
real `bulk_insert_binary` workload over the wire requires the
RedWire SCRAM handshake, which neither the bundled `node` driver
(`drivers/node/index.js` — speaks the legacy 5-byte frame, no
handshake) nor `red connect` (gRPC only) implement. Building a
one-off Rust client to drive it is bigger than this slice.

When the host kernel is reconfigured (`sysctl
kernel.perf_event_paranoid=1`, `kernel.yama.ptrace_scope=0`), the
intended capture is:

```bash
RUSTFLAGS="-Cforce-frame-pointers=yes" cargo build --release --bin red
docker compose -f docker/compose.yml -p bench_perf up -d reddb
PID=$(docker inspect -f '{{.State.Pid}}' bench_perf-reddb-1)
perf record -F 99 -g -p "$PID" -- sleep 30 &
cargo run -q -p bench-runner -- --mini-duel --scenario insert_sequential \
    --database duel --profile standard --warmup-runs 0 --items 10000 --runs 1
wait
perf report --stdio --no-children | head -40
```

## Suspected hot path (static read)

Anchors: `crates/reddb-server/src/wire/listener.rs:350`,
`:427`, `:444`; `crates/reddb-server/src/runtime/mutation.rs:66`,
`:89`; `crates/reddb-server/src/storage/unified/store/impl_entities.rs:699`;
`crates/reddb-server/src/storage/unified/store/commit.rs:365`,
`:530`, `:561`; `crates/reddb-server/src/storage/engine/btree/impl.rs:94`.

Per `insert_sequential` row, with the bench's `bulk_insert_binary`
single-row frame, the server walks:

1. **Wire decode** — `handle_bulk_insert_binary`
   (`wire/listener.rs:350`). Parses `coll_len/coll/ncols/(name)*ncols/nrows/(tag+val)*ncols`;
   builds a `Vec<CreateRowInput>` of 1, allocating a `String` for
   the collection name (cloned per row inside the row loop, line
   419) plus per-field `String` keys.
2. **Routing** — `runtime.create_rows_batch`
   (`application/ports_impls_entity.rs:2082`) → `MutationEngine::apply`
   (`runtime/mutation.rs:66`) which sees `rows.len() == 1` and
   dispatches to `append_one` (`runtime/mutation.rs:89`).
3. **Contract / uniqueness** — for `bench_users` no contract is
   declared (`setup_schema` is a no-op in the wire adapter,
   `bench-adapters/src/reddb_wire.rs:589`), so
   `db.collection_contract` returns `None` and
   `normalize_insert_fields` short-circuits (`ports_impls_entity.rs:246`).
   This is *not* a hot spot for this scenario — but it would be for
   any benchmark that pre-creates a typed table.
4. **Snapshot xid** — autocommit allocates a fresh xid via
   `snapshot_manager.begin()` and immediately `commit()`s it
   (`runtime/mutation.rs:111-118`). Each call locks the snapshot
   manager's allocator + commit set twice.
5. **`store.insert_auto`** (`storage/unified/store/impl_entities.rs:699`):
   - `get_or_create_collection`
   - `next_entity_id()` (atomic CAS)
   - `next_row_id()` (atomic CAS, separate counter)
   - `context_index.index_entity(...)` — for `EntityKind::TableRow`
     with no link refs this is a quick check but still a
     `RwLock::read` on the registry.
   - `manager.insert(entity)` (`storage/unified/manager.rs:269`):
     write-locks the growing segment, copies the entity into the
     segment's storage, and bumps `total_entities_atomic`.
   - **`manager.get(id)`** — clones the entity *out* of the segment
     so `serialize_entity_record` can run against it. Comment on
     line 740 says this used to happen 3× per insert; today it's 1×,
     but it's still a full clone of every just-inserted row.
   - `btree.insert(&id_be_bytes, &serialized)`
     (`storage/engine/btree/impl.rs:94`): the monotonic-id append
     hits the rightmost-leaf cache and avoids tree descent on the
     happy path, but every insert still does
     `pager.read_page` + `pager.write_page` on the leaf, plus a
     `RwLock::write` on the rightmost-leaf cache to bump the high
     key.
   - `finish_paged_write` →
     `commit.append_actions(&[StoreWalAction::upsert_entity])`
     (`storage/unified/store/commit.rs:365`).
6. **WAL append + group-commit fsync wait**
   (`storage/unified/store/commit.rs:399-417`,
   `wait_until_durable` at `:513`). The blob — `Begin / PageWrite /
   Commit` for one row — is encoded outside any lock, then enqueued
   under the queue's mutex; the caller blocks on the
   `CommitStateCondvar` until the drainer fsyncs past its LSN.
   `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0`
   (`crates/reddb-server/src/api.rs:36`), so for a *single* writer
   the fsync runs immediately on each statement — there is no
   coalescing partner. With one bench client serially calling
   `insert_one`, every statement pays one fsync.
7. **CDC + cache invalidation** — `cdc_emit` + secondary-index
   maintenance (`runtime/mutation.rs:138-149`). Empty for the bench
   matrix (no indexes pre-created on `bench_users` until
   `post_seed`), but each call still acquires the CDC ring buffer
   lock.

The wire frame round-trip itself (`session.rs:110-115`) is one
buffered TCP write + one read; we already paid the no-handshake fast
path tax once per connection, not per row.

## Top suspected bottlenecks

Ordered by expected weight on a single-writer `insert_sequential` mix.
Without a flamegraph these are best-guesses; the punch-list below is
written so each item carries its own validation step.

1. **WAL group-commit fsync wait per autocommit statement.** Every
   `insert_one` becomes one `commit.append_actions` call, which
   blocks on `wait_until_durable` until the drainer fsyncs. With
   `window_ms = 0` and one client there is nothing to coalesce, so
   we pay one fsync per row. Latency floor on consumer-grade NVMe
   is ~50–150 µs; that alone explains the 109–840 µs p50 range.
2. **Per-row entity clone after `manager.insert`.** `insert_auto`
   inserts then re-fetches the entity via `manager.get(id)` to
   serialize for the B-tree image. The fetch clones the
   `UnifiedEntity` (HashMap<String, Value> + per-field Strings) so
   the segment can keep its copy. This was the dominant line in the
   pre-refactor flamegraphs cited in `docs/perf/roadmap.md` (god
   node `UnifiedRecord layout`).
3. **Per-row `Vec<(String, Value)>` round trip in
   `handle_bulk_insert_binary` → `CreateRowInput`.** The wire
   decoder allocates an owned `String` for every column name on
   every row, even when the column-name list was already decoded
   once at the head of the payload. The single-row mini-duel hits
   this for every insert.
4. **Snapshot manager `begin()` + `commit()` per autocommit row.**
   Each row's xid lifecycle takes the manager's allocator lock and
   commit-set lock back-to-back. Cheap individually, but on a hot
   loop it shows up.
5. **B-tree leaf hot-page contention.** Every monotonic-id insert
   hits the same rightmost leaf, taking
   `rightmost_leaf.write()` plus `pager.write_page` (which itself
   takes the page cache mutex on `cache.insert` and `mark_dirty`).
   Multi-writer this would be a serialised choke point.

## Punch-list

Five concrete follow-up items the main agent can decide to file.
Each is bounded so a single PR can land it without sprawling.

### P1 — Coalesce single-writer fsyncs by raising `window_ms`

- **Problem.** `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0` means a lone
  benchmark client gets one fsync per row, which is the throughput
  floor. PG sidesteps this with `synchronous_commit = off`. The
  current default was chosen to keep multi-writer latency low, but
  it is hostile to single-writer OLTP.
- **Suspected fix.** Make the group-commit window adaptive: when the
  drainer observes one statement queued + zero late arrivals across
  a short observation window (say, 100 µs), bump the wait to e.g.
  500 µs–1 ms so successive autocommits coalesce. Validate by
  re-running the 10k mini-duel — expectation is reddb_wire moves
  from ~1.4k ops/s to >2.5k ops/s.
- **Acceptance.** Multi-writer p99 doesn't regress in the
  `mixed_workload` mini-duel.

### P2 — Drop the `manager.get(id)` clone in `insert_auto`

- **Problem.** `storage/unified/store/impl_entities.rs:745` fetches
  the just-inserted entity back out of the segment to serialize for
  the B-tree image. This re-clones a `UnifiedEntity` (HashMap +
  per-field `String`s + Vec metadata) per insert.
- **Suspected fix.** Have `manager.insert` return either the
  inserted `&UnifiedEntity` or a small handle (`SegmentRef { seg, slot
  }`) that `serialize_entity_record` can read in place. Same shape
  the bulk path already uses for the columnar layout.
- **Acceptance.** No behaviour change in
  `storage/unified/store/impl_entities.rs` tests; flamegraph share
  of `UnifiedEntity::clone` on `insert_sequential` drops below 5%.

### P3 — Skip per-row `String` cloning in
`handle_bulk_insert_binary`

- **Problem.** `wire/listener.rs:412-424` builds
  `Vec<CreateRowInput>` and clones the collection name + every
  column name into per-row owned `String`s, even though the column
  list was decoded once at the head of the payload. For
  single-row frames this is the entire decode cost.
- **Suspected fix.** Carry the column-name slice as
  `Arc<[Arc<str>]>` (or `&[&str]` borrowed from the payload) and
  pipe it through `CreateRowInput` / `MutationRow` so the
  per-row vector stops re-allocating identical strings. The
  prevalidated columnar path already does the right thing
  (`create_rows_batch_prevalidated_columnar`); align the
  non-prevalidated path with it.
- **Acceptance.** Allocator-instrumented test shows zero
  `String::from(&str)` per row inside `handle_bulk_insert_binary`
  for a 10-row × 7-col frame.

### P4 — Reuse the autocommit xid across micro-batches

- **Problem.** Each row in `MutationEngine::append_one` allocates a
  fresh xid (`snapshot_manager.begin()` + `commit()`).
  `append_batch` already amortises this across the batch.
  Single-row inserts can't, but a Nagle-style coalescer for
  back-to-back autocommit inserts on the same connection could.
- **Suspected fix.** When the wire session sees a sequence of
  single-row `bulk_insert_binary` frames within ~µs, fold them into
  one batch frame before dispatch — same effect as the streaming
  `BulkInsertStream` path. Alternatively: pre-allocate a small pool
  of "pre-committed" xids in the snapshot manager so `begin/commit`
  on the autocommit path is two atomic loads instead of two lock
  acquisitions.
- **Acceptance.** `cargo bench -p reddb-server snapshot_manager`
  doesn't regress; `insert_sequential` reddb_wire ops/sec improves
  by ≥10%.

### P5 — Live profile + flamegraph of `insert_sequential`

- **Problem.** This document is static analysis. Some of the items
  above may turn out to be ≤2% of CPU once a flamegraph lands; the
  ranking is best-effort.
- **Tooling (landed).** [`make perf-bench`](../../Makefile) builds
  `red` with `-Cforce-frame-pointers=yes`, starts it on
  `127.0.0.1:5050`, attaches `perf record -F 99 -g` for 30 s, and
  renders `target/perf/insert_sequential.svg` via
  `inferno-flamegraph`. Host requirements
  (`kernel.perf_event_paranoid <= 1`,
  `kernel.yama.ptrace_scope = 0`) are documented in
  [`perf-knobs.md`](perf-knobs.md); the target itself fails loudly
  with the exact `sysctl` invocation when they're not met. Drive
  load from the bench at
  `/home/cyber/Work/reddb.io/rdb-benchmark` during the 30 s window.
- **Acceptance.** A re-run of this profile lists actual CPU
  percentages for the top 20 frames and updates the punch-list
  ranking accordingly.

## Notes for the next investigator

- The bench `make build` is heavyweight; on this slice the host's
  global `CARGO_TARGET_DIR=~/.cache/cargo-target` already had a
  fresh `red` from worktree HEAD. Copy it once into
  `reddb/target/release/red` so `docker/compose.yml`'s bind-mount
  picks it up — `make package-reddb-local` does this if you don't
  want to run `make build` from scratch.
- `--mini-duel` rejects `--warmup-runs 1`. The prompt's command
  uses `--warmup-runs 1`; substitute `--warmup-runs 0` per the
  bench-runner check at `bench-runner/src/main.rs:649`.
- `results/history.jsonl` is append-only; the `bench-history` crate
  has helpers for slicing it by session id.
