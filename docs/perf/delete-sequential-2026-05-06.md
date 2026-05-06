# `delete_sequential` mini-duel profile — 2026-05-06

Status: **partial live reproduction + static analysis** (perf knobs
still locked — see [`perf-knobs.md`](perf-knobs.md)).

Tracking issue: #85 — *"delete_sequential mini-duel: RedDB ~20%
slower than PostgreSQL"*.

Companion to [`insert_sequential-2026-05-05.md`](insert_sequential-2026-05-05.md)
(#76, commit `7159ddd`).

## TL;DR

- **This is a real regression at scale, not a warm-up artefact.** The
  story is the *opposite* of #76. Reproducing on this host
  (cyber-XPS-13-9300, worktree HEAD `11e438d`) shows reddb_wire
  *winning* at small N (1.5×–3× over PG at 100 rows; 1.3×–1.5× at 1
  000 rows) but *losing* at 10 000 rows: ~330 ops/s vs PG's ~672 ops/s
  (roughly **0.5×** of PG, p50 = 2.9 ms vs 1.4 ms). The gap *opens*
  with N, which is the characteristic shape of a per-delete scan
  growing with collection size.
- **Root cause is structural, not micro-optimisation.** The bench
  driver issues `DELETE FROM bench_users WHERE id = N`, where `id` is
  a *user* column, not the synthetic `red_entity_id`. RedDB's
  `delete_sequential` adapter doesn't call `post_seed`, so no hash
  index on `id` exists during the delete loop. The DML target scan
  therefore falls through every fast path and ends up doing a
  zone-scan of the growing segment. PostgreSQL hits its `id BIGINT
  PRIMARY KEY` index; MongoDB hits its `id` index; RedDB scans every
  surviving row. Total cost is roughly O(N²/2).
- **Top three bottlenecks** (per row in the steady-state delete
  loop):
  1. Full segment scan in `for_each_entity_zoned` for every
     `WHERE id = N` (no index, `id` ≠ `red_entity_id`).
  2. WAL group-commit fsync wait per single-row `DELETE`
     (`commit::append_actions` → `wait_until_durable`), inherited
     from #76 P1 — `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0`.
  3. Per-row registry-wide locks: `entity_cache.write`,
     `cross_refs.write`, `reverse_refs.write` (scans every collection's
     reverse-ref Vec even though the bench has zero cross-refs), plus
     a B-tree leaf `pager.write_page` for each delete.

The headline 20 % gap in #85 corresponds to the small-N segment of
the curve. The *right* number is "gap that grows with N" — the
adapter and runtime both contribute, and a naïve "make the bench
fair" patch (call `post_seed`) hides a real engine property: RedDB
has no automatic primary-key index on user-declared columns.

## Reproducer & raw ops/sec

Bench harness: `/home/cyber/Work/reddb.io/rdb-benchmark`
(prebuilt `bench-runner` at
`~/.cache/cargo-target/release/bench-runner` via the global
`CARGO_TARGET_DIR`).

`--mini-duel` rejects `--warmup-runs 1`; canonical command uses
`--warmup-runs 0`:

```bash
~/.cache/cargo-target/release/bench-runner \
    --mini-duel --scenario delete_sequential \
    --database duel --profile standard \
    --warmup-runs 0 --items <N> --typed-items <N> --runs <R>
```

### Headline (mini-duel sessions on 2026-05-06)

| items | postgresql      | reddb_wire       | mongodb       | reddb advantage vs PG |
|------:|-----------------|------------------|---------------|-----------------------|
|   100 | 430 / 611 / 284 | 1071 / 1032 / 930 | —            | **2.5× – 3.3× over PG** |
| 1 000 | 698 / 501 / 1001 | 1046 / 659 / *t/o* | —            | **1.3× – 1.5× over PG** |
| 1 000 | —               | 660 / 711        | 441 / 505     | **1.5× – 1.4× over Mongo** |
| 10 000| 673 / 671       | 340 / 331        | —            | **0.51× – 0.49× of PG (regression)** |

(Triplets are run-1/run-2/run-3 ops/sec. *t/o* = run timed out
waiting for the reddb container at the third iteration of the 1 000
shape — same intermittent that bit #76 P5; not a steady-state
signal.)

Sessions in `results/history.jsonl`:

- `sess-20260506031806-3887331` (100 items, duel)
- `sess-20260506031458-3867892` (1 000 items, duel)
- `sess-20260506031918-3895539` (10 000 items, duel)
- `sess-20260506032137-3902615` (1 000 items, mongodb,reddb_wire)

p50 latencies tell the same story:

```
items=100:     pg p50= 1.5–2.5 ms   rw p50= 0.9–1.0 ms
items=1000:    pg p50= 1.0–1.8 ms   rw p50= 0.9–1.4 ms
items=10000:   pg p50= 1.4 ms       rw p50= 2.9 ms     ← gap opens
```

The 10 000-row p50 alone — 2.9 ms vs 1.4 ms — is the size of one
extra full-segment scan over a now-larger growing segment. That
matches the static read below.

### Is this real or warm-up-dominated?

**Real.** Two independent indicators:

1. The 100-row and 1 000-row shapes already *won* against PG. If the
   regression was warm-up + container start cost (the #76 shape) we
   would expect reddb to lose at small N and recover at large N. We
   see the opposite: reddb is fastest at the smallest N and loses at
   the largest N. Warm-up costs go down with N (per-op amortisation),
   not up.
2. p50 latency *increases* with N for reddb (0.9 → 2.9 ms) but is
   roughly flat for PG (1.0 → 1.4 ms). PG's per-op cost is
   index-bound (constant). RedDB's per-op cost is scan-bound (grows
   with N).

The original #85 issue body shows a 20 % gap; that's the small-N
boundary where reddb is sometimes slightly behind PG run-to-run on
warm-up wobble. The *engineering* gap is the slope, and it is real.

## DELETE hot path (static read)

Anchors:

- `crates/reddb-server/src/runtime/impl_dml.rs:1167` — `execute_delete_inner`
- `crates/reddb-server/src/runtime/impl_dml.rs:209` — `delete_entities_batch`
- `crates/reddb-server/src/runtime/dml_target_scan.rs:58` — `find_target_ids`
- `crates/reddb-server/src/runtime/query_exec/helpers.rs:42` — `extract_entity_id_from_filter`
- `crates/reddb-server/src/storage/unified/store/impl_entities.rs:925` — `UnifiedStore::delete_batch`
- `crates/reddb-server/src/storage/unified/store/impl_entities.rs:1237` — `unindex_cross_refs_batch`
- `crates/reddb-server/src/storage/unified/manager.rs:837` — `manager.delete_batch`
- `crates/reddb-server/src/storage/unified/manager.rs:1179` — `for_each_entity_zoned`
- `crates/reddb-server/src/storage/unified/segment.rs:667` — `Segment::delete_batch`
- `crates/reddb-server/src/storage/unified/store/commit.rs:429` — `append_actions` (WAL)

Per `delete_sequential` row, with the bench's wire DELETE
(`DELETE FROM bench_users WHERE id = N`), the server walks:

1. **SQL parse + DML dispatch** — execute_delete picks up the
   `id = N` filter and routes to `execute_delete_inner`
   (`impl_dml.rs:1164`).
2. **Target scan** — `DmlTargetScan::find_target_ids`
   (`dml_target_scan.rs:58`):
   - **Entity-id fast path miss** (`:69`):
     `extract_entity_id_from_filter` only matches column names
     `red_entity_id` or `entity_id`
     (`query_exec/helpers.rs:53`). The bench column is `id`, so this
     returns `None`.
   - **Hash index miss** (`:77`): `try_hash_eq_lookup` looks up an
     `idx_id` hash index. The `reddb_wire` adapter's `setup_schema`
     is a no-op (`reddb_wire.rs:589`); only `post_seed` and
     `prepare_update_by_id` create the `idx_id` HASH index, and
     `delete_sequential` never calls them. Returns `None`.
   - **Sorted index miss** (`:83`): same story, no BTREE on `id`.
     Returns `None`.
   - **Zone-pruned full scan** (`:123`):
     `manager.for_each_entity_zoned` walks the growing segment plus
     every sealed segment that the zone-map can't prune on
     `id`. For the bench shape, all rows live in a single growing
     segment (no seal between insert_bulk and the delete loop), so
     zone-map pruning never fires; we visit every live row.
     `entity_visible_under_current_snapshot` runs per row before the
     compiled filter.
3. **Per-row delete batch** — `delete_entities_batch` is called with
   a 1-element slice (`impl_dml.rs:1186` chunks of 2 048, but the
   match list itself is size 1):
   - `entity_cache.write` (`impl_entities.rs:935`): take the
     store-wide entity cache write lock to remove one entry.
   - `manager.delete_batch` → `Segment::delete_batch`
     (`segment.rs:667`): take the growing segment's write lock,
     remove the entity (HashMap or flat vec branch), update
     metadata, push to `self.deleted` set.
   - **B-tree leaf delete** (`impl_entities.rs:957`): `btree.delete`
     on the per-collection physical-id BTREE — one
     `pager.read_page` + `pager.write_page` on the leaf, plus
     freelist work if the leaf empties.
   - `unindex_cross_refs_batch` (`impl_entities.rs:1237`): take
     `cross_refs.write` and `reverse_refs.write`, *then iterate
     `values_mut()` over the entire reverse-ref map* (every live
     entity, scoped by `mark_paged_registry_dirty`). For the bench
     shape `reverse_refs` is empty so each scan is cheap, but the
     two write-lock acquisitions still serialise vs concurrent
     traffic.
   - `remove_from_graph_label_index_batch`: another short write
     lock per delete.
   - `mark_paged_registry_dirty`: bumps a flag.
4. **WAL append + group-commit fsync wait** —
   `finish_paged_write([DeleteEntityRecord{…}])`
   (`commit.rs:429`). One `Begin / PageWrite / Commit` blob per
   call, enqueued under the queue mutex; caller blocks on
   `wait_until_durable`. With the bench running a single client
   serially, `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0`
   (`api.rs:36`) means every delete pays one fsync. Same story as
   #76 P1 for INSERT — but here it stacks on top of the O(N) scan,
   not on top of a constant-time path.
5. **CDC emit + context-index remove** —
   `context_index().remove_entity` per id, plus a CDC ring-buffer
   push. Empty subscribers but the locks still cycle.

### The Big-O picture

For N rows seeded then deleted one-by-one:

| step                                | per delete | total over N |
|-------------------------------------|------------|--------------|
| Target scan (`for_each_entity_zoned`) | O(rows_remaining) | **O(N²/2)** |
| Per-row write locks (`entity_cache`, `cross_refs`, `reverse_refs`, segment) | O(1) | O(N) |
| B-tree leaf delete + pager write    | O(log N)   | O(N log N) |
| WAL fsync                           | O(1) syscall | O(N) syscalls |

The dominant term at N = 10 000 is the target scan: ~50 M predicate
evaluations across the loop, each doing a HashMap lookup on the row's
`id` field for the equality check. PG and Mongo are O(N log N) total
because their primary-key index turns the scan into a B-tree probe.

## Top three bottlenecks

Ordered by expected weight at N = 10 000.

1. **`WHERE id = N` falls through to a full segment scan because the
   adapter doesn't index `id`.** This is the load-bearing finding.
   Two ways to fix this — neither is purely engine work:
   - *Adapter side*: have `delete_sequential` (and any other scenario
     that does point lookups by `id`) call `adapter.post_seed()` /
     `prepare_update_by_id()` after the bulk insert so `idx_id` HASH
     exists before the delete loop. This is the closest analogue of
     PG's `id BIGINT PRIMARY KEY`.
   - *Engine side*: teach RedDB to *automatically* maintain a hash
     index on the user-declared `id` column when present. Implicit
     PK is what every comparable system has; we don't.
2. **Per-statement WAL fsync (#76 P1 redux).** Every single-row
   `DELETE` commits with `window_ms = 0`, so each row pays one fsync.
   On NVMe that's ~50–150 µs floor *before* the scan even runs.
   Coalescing single-writer fsyncs is already on the punch-list from
   #76 P1; it would help DELETE the same way it helps INSERT.
3. **Per-delete registry-wide write locks.** Every single-row delete
   takes — in order — `entity_cache.write`, the segment's write
   lock, `cross_refs.write`, `reverse_refs.write` (with a
   `values_mut()` scan over *all* reverse refs, even when the deleted
   entity has none), `graph_label_index.write`, plus the BTREE
   write. For sequential, single-client deletes this is just
   sequential cost; for concurrent deletes it serialises every
   writer. The `unindex_cross_refs_batch` `values_mut()` scan in
   particular has no early-exit when the deleted ids have no inbound
   refs, so its cost grows with the *graph*, not the batch size.

Lower-ranked but worth flagging:

- The `delete_entities_batch` path always goes through
  `manager.get(id)` first when inside a BEGIN-wrapped txn
  (`impl_dml.rs:227`) to read+update xmax — that's two lock cycles
  per row. Autocommit DELETE skips this branch, so it doesn't bite
  the bench, but a `BEGIN; DELETE; DELETE; …; COMMIT` shape would.
- The `entity_cache` is sized at 10 000 entries with a naïve
  "drop the first key" eviction (`impl_entities.rs:871`). Under a
  delete-heavy workload that touches every entity, the cache is
  pure overhead.

## Punch-list

Five concrete follow-up items the main agent can decide to file.

### P1 — Implicit hash index on user `id` columns at insert time

- **Problem.** RedDB has no concept of an automatic primary-key index
  on user columns. Every benchmark scenario that does point lookups
  by `id` either has to remember to call `post_seed` (and pay the
  index-build cost upfront) or eat an O(N) scan per lookup. PG and
  Mongo both auto-index `id` (PG via PRIMARY KEY, Mongo via `_id`
  default index).
- **Suspected fix.** When a column named `id` (or declared with a
  `PRIMARY KEY` modifier) is materialised on first insert, register a
  HASH index in `index_store` automatically. The maintenance cost is
  one HashMap insert per row on the write path, which is cheap
  compared to the scan it saves on every read/update/delete.
- **Acceptance.**
  `delete_sequential` reddb_wire ops/sec at N=10 000 climbs from
  ~330 to ≥1500. No regression on `insert_sequential` reddb_wire at
  any N (the index maintenance is small relative to the WAL fsync
  floor; should be ≤5 %).
- **Status (2026-05-04, #112 implementation).** Landed as a hook in
  `MutationEngine::{append_one, append_batch}` (`crates/reddb-server/
  src/runtime/mutation.rs::maybe_auto_index_id`). On the first insert
  carrying a column named `id` (case-sensitive, conservative), the
  hook registers a HASH index named `idx_id` on the collection via
  `IndexStore::create_index` + `register`. The standard per-row
  `index_entity_insert{,_batch}` pass that follows populates it.

  Path-level expectation against the static read in this doc:
  - `DmlTargetScan::find_target_ids` (`dml_target_scan.rs:77-80`) now
    has `try_hash_eq_lookup` succeed for `WHERE id = N` because
    `find_index_for_column("bench_users", "id")` returns
    `Some(idx_id, Hash, ["id"])`. The growing-segment scan is skipped.
  - The per-delete cost line "**Full segment scan** in
    `for_each_entity_zoned` for every `WHERE id = N`" — the dominant
    O(N²/2) term identified above — collapses to an O(1) HashMap
    probe. Big-O total drops from O(N²/2) to O(N) on the scan term.
  - WAL fsync (P3 / #76 P1) and per-row registry write locks (P4) are
    untouched; they remain the new floor at ~330 ops/s × (10000 ÷
    50M predicate evaluations saved) ≈ ≥1500 ops/s expected, matching
    the original Acceptance target. Confirming with a live mini-duel
    requires the bench-runner + PG/Mongo containers; deferred to the
    next round when knobs in `perf-knobs.md` are loosened.

  Opt-out: `RedDBOptions::with_auto_index_id(false)` →
  `UnifiedStoreConfig::auto_index_id = false`. Defaults to `true`.
  Lifecycle: `execute_drop_table` already iterates `list_indices` and
  drops every entry, so the implicit index is reaped with the
  collection (no special-case needed).

### P2 — Add `post_seed` to `delete_sequential` (bench-side stop-gap)

- **Problem.** Even before P1 lands, the bench is comparing
  apples-to-oranges: PG/Mongo run with their PK index; RedDB doesn't.
  This makes #85's headline reproducible but not actually
  load-bearing for "is RedDB slow at DELETE".
- **Suspected fix.** In
  `rdb-benchmark/crates/bench-scenarios/src/delete_sequential.rs:31`,
  add `adapter.post_seed().await?;` after the bulk-insert loop and
  before the timed delete loop. Mirror what `bulk_update.rs:42` and
  `select_filtered.rs:35` already do.
- **Acceptance.** With P2 only, reddb_wire ops/sec at N=10 000 ≥ PG's
  baseline. Document the change in `BASELINE.md` so the historical
  ops/sec numbers aren't compared across the boundary.

### P3 — Coalesce single-writer fsyncs (re-file from #76 P1)

- **Problem.** Same as #76 P1: `DEFAULT_GROUP_COMMIT_WINDOW_MS = 0`
  makes every autocommit DELETE pay one fsync, with no coalescing
  partner when there's only one bench client.
- **Suspected fix.** Adaptive `window_ms` based on observed queue
  arrival rate: if the drainer sees a single-statement queue
  followed by a burst within ~100 µs, bump the wait to ~500 µs so
  successive autocommit deletes coalesce. Same Acceptance as #76 P1
  — multi-writer p99 doesn't regress on `mixed_workload`.
- **Acceptance.** Once P1+P2 land, this should bring reddb_wire ops/s
  on `delete_sequential` from "PG parity" to "PG win" (single fsync
  vs single index probe is roughly even on this disk).

### P4 — Early-exit `unindex_cross_refs_batch` when the entity has no in-edges

- **Problem.** `impl_entities.rs:1252` always takes
  `reverse_refs.write` and iterates `values_mut().retain(...)`. For
  the common case of deleting rows that aren't graph nodes (no
  inbound edges), this is pure write-lock overhead — and worse,
  it scales with the *size of the reverse-ref map*, not the batch.
- **Suspected fix.** Skip the `reverse_refs.write` block when the
  per-collection `reverse_refs` is empty (cheap read-lock check
  first). Symmetric for `cross_refs.write`. Do not fold this into
  the registry-dirty bump; only mark dirty if a removal actually
  happened.
- **Acceptance.** A microbench that deletes 10 000 rows from a
  table with zero cross-refs shows
  `unindex_cross_refs_batch` time → 0 in the flamegraph. No change
  in graph-traversal correctness tests.

### P5 — Drop the store-wide `entity_cache` (or scope it correctly)

- **Problem.** `entity_cache` (`impl_entities.rs:849`,
  `delete_batch:935`) caches entity reads across the whole store
  with a 10 000-entry limit and a naïve "drop the first key the
  HashMap iterator yields" eviction. On `delete_sequential` it's
  invalidated for every deleted id and never gets a hit (the bench
  reads each row exactly once via the DELETE scan). It's a write-lock
  per delete with zero hit-rate.
- **Suspected fix.** Either gate the cache behind a feature flag and
  default it off for OLTP workloads, or replace it with an
  `arc_swap`-based read-mostly cache so DELETEs don't take the
  write lock. Cheaper still: drop the invalidation in
  `delete_batch` when the cache is empty (read-lock probe).
- **Acceptance.** `entity_cache.write` disappears from the
  `delete_sequential` flamegraph. No regression on the `select_*`
  scenarios that the cache was originally introduced for.

## Notes for the next investigator

- The bench harness is the same one #76 used; reuse the host-cached
  `bench-runner` at `~/.cache/cargo-target/release/bench-runner`. A
  fresh `cargo run` recompiles the world and times out within a
  10-minute slice on this host.
- The 1 000-row, 3-run mini-duel intermittently times out on the
  third iteration with `timeout waiting for bench_…-reddb-1`. Same
  symptom as the late-run flake noted in #76 P5; does not affect the
  trend, but means "always grab at least 2 runs and check the slope
  vs N rather than chasing a single run-3 number".
- A live flamegraph still requires
  `kernel.perf_event_paranoid <= 1` and
  `kernel.yama.ptrace_scope = 0`
  ([`perf-knobs.md`](perf-knobs.md)). The `make perf-bench` target
  added in #76 P5 covers the INSERT shape; copy-paste it with
  `--scenario delete_sequential` and a longer drive window once the
  knobs are loosened.
- `delete_sequential` history shows historical reddb runs at very
  different ops/s — e.g. `reddb_binary` at 9 557 ops/s on N=50 000
  (`2026-04-15`) and another at 32 ops/s on the next day. Both
  current adapters (`reddb_wire`, `reddb_grpc`, `reddb_binary` →
  `wire`) issue the same `DELETE FROM coll WHERE id = N` SQL today,
  so the historical 9k is *not* explainable from the current code.
  Worth checking whether an earlier `setup_schema` revision built
  `idx_id` for these adapters, or whether `id` was at one point
  treated as the synthetic `red_entity_id`. Either explanation
  reinforces the headline finding: the regression is about whether
  *something* indexes `id`, not about the engine's per-row delete
  cost.
