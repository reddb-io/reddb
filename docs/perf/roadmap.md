# RedDB Performance Roadmap

Status: 2026-04-19 — mini-duel baseline via `reddb_wire` (consistent transport).

## Methodology

All benchmarks run via `make mini-duel` in `reddb-benchmark`, which maps
`--database duel` to `reddb_wire + postgresql`. One transport, apples to
apples. Any result discussed in this document must come from a run made
under this setup — no cherry-picking transports per scenario.

## Known server-side bottlenecks

Listed by scope and return-on-investment. Each is a dedicated PR.

### 1. UnifiedRecord layout (scan hot paths)

**Problem.** `UnifiedRecord` stores fields in
`HashMap<String, Value>`. Every scan row materialisation allocates a
fresh HashMap plus `N` owned String keys. Flamegraphs show
`HashMap::insert` + `UnifiedRecord::set` at **~60 % of CPU** on
`select_range`, `select_filtered`, `mixed_workload_indexed`.

**Fix.** Replace per-record HashMap with a schema-shared layout:

```rust
pub struct UnifiedRecord {
    schema: Arc<Vec<String>>,  // shared across all records of one result
    values: Vec<Value>,        // parallel to schema
    overflow: Option<HashMap<String, Value>>,  // only for ragged rows
}
```

- `Arc::clone` per record instead of N heap allocations.
- `values` access via `schema.binary_search(col).map(|i| &values[i])`
  is O(log N) with cache-friendly Vec.
- Overflow HashMap only materialises for schemaless inserts.

**Blast radius.** 746 call sites reference `UnifiedRecord` or its
`values` field. Mostly mechanical (grep replace `.values.insert(k, v)`
→ `.set_owned(k, v)`), but every caller needs review for read
semantics.

**Estimated effort.** 2-3 dias dedicados + regression tests.

**Estimated win.** 2-3× on scan-heavy scenarios:
- `select_range` 6.8× → 2-3× gap
- `select_filtered` 3.7× → 1.5-2× gap
- `mixed_workload_indexed` 4.2× → 1.8-2× gap
- `select_complex` currently 1.15× paridade, minor upside

### 2. WAL append lock-free path (concurrent writes)

**Problem.** Every commit takes `Mutex<WalWriter>` across `Begin +
PageWrite×N + Commit` append. Under 16-way concurrent workers
inserts serialise on this mutex *and* then on the state-condvar
wait loop (notify_all thundering herd).

Prior attempts that moved encode-out-of-lock *regressed* because 16
threads then converge on the mutex in a burst, causing park
convoys. The real fix is lock-free append.

**Fix.** Replace `Mutex<WalWriter>` with a lock-free append ring:
- `crossbeam::queue::SegQueue<(u64 seq, Vec<u8>)>` for pending
  encoded records.
- Writer CAS-es a sequence from atomic `next_seq`, pushes bytes.
- A single leader (whichever thread calls `drive_flush` first)
  takes the WAL file lock, drains the queue in LSN order, writes
  to BufWriter, fsyncs, publishes `durable_lsn` via atomic.
- Waiters atomic-load `durable_lsn` and park with a timeout; the
  leader `unpark_all()` after publish.

Separately: convert `commit.rs` state Mutex + Condvar to
`parking_lot` primitives (non-poisoning, much lighter park path).

**Blast radius.** WAL writer API surface + commit coordinator
rewrite. Recovery path must still be able to read the new
format (unchanged on-disk layout, only in-memory buffering
changes).

**Estimated effort.** 3-5 dias + fuzz + crash tests.

**Estimated win.** 3-5× on concurrent-bound scenarios:
- `concurrent` 9.5× → 2-3× gap (via wire, server-side ceiling)
- `insert_sequential` 1.2× → 1.0× paridade
- `bulk_update` — already at paridade via in-place upsert, minor

### 3. Pager cache striped locks

**Problem.** `PageCache::insert` takes a single `RwLock<entries>`
write lock per page mutation. Under heavy concurrent write
workloads, all workers contend on it even for disjoint pages.

**Fix.** Shard the cache into N buckets (8 or 16), each with its
own RwLock. `insert(page_id)` picks bucket via
`page_id % N`. Readers for different pages never collide.

**Blast radius.** `page_cache.rs` internal refactor. No API
changes visible to callers.

**Estimated effort.** 1 dia + correctness review (SIEVE eviction
semantics across shards).

**Estimated win.** 1.3-1.5× on write-heavy scenarios. Compounds
with #2.

### 4. BTree batch upsert by leaf

**Problem.** `persist_entities_to_pager` calls `btree.upsert()` in
a loop for each mutated entity. For `bulk_update` of 50 rows,
that's 50 separate btree walks to maybe 10 distinct leaves. Each
walk is O(log n) + page read + page write.

**Fix.** Sort keys within a single entity batch, walk to each leaf
once, apply all updates for that leaf in one page write, move to
the next leaf.

**Blast radius.** Additive — new `BTree::upsert_batch_sorted`
helper, one caller change in `persist_entities_to_pager`.

**Estimated effort.** 1-2 dias.

**Estimated win.** 1.5-2× on bulk UPDATE paths. Complements #1.

## Deferred / out of scope

- **gRPC transport overhead** — `reddb_binary` adapter is slower
  because tonic does `spawn_blocking` per RPC (~150µs handoff).
  We no longer measure via gRPC; using wire consistently sidesteps
  the noise. Real fix = switch tonic to sync handlers, which is a
  tonic-wide change, not a RedDB one.

- **select_complex already at paridade** (1.15×). No action.

- **Correctness audit of scan paths after UnifiedRecord refactor**
  — baked into #1 acceptance criteria.

## Execution order

Hard dependencies: none between #1/#2/#3/#4. Pick by ROI.

1. **#1 UnifiedRecord** — biggest aggregate win, touches 4 scan
   scenarios. Do this first.
2. **#2 WAL lock-free** — closes the concurrent gap. Independent
   of #1. Second.
3. **#4 BTree batch upsert** — small focused PR, compounds with
   #2. Third.
4. **#3 Pager cache striped** — nice-to-have, smaller impact in
   isolation, but compounds with #2+#4. Last.

Total realistic effort: **2 weeks of focused work** to close most
remaining gaps.

## Non-goals

- Hitting PG parity on every scenario (we're a different database).
- Maintaining durability guarantees weaker than PG's
  `synchronous_commit=off` — async mode is the floor.
- Adding feature flags for each optimisation; land them
  unconditionally or don't land them.
