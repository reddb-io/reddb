# Structural hot-path sweep, Issue #2011

Continuation of the mechanical wins in PR #2010. Each candidate was measured
before and after with the `CountingAllocator` pattern (allocations/op) and a
simple `Instant` loop (ns/op), driven by the in-crate micro-measurement test
`storage::no_malloc_hot_paths::structural_hot_path_report`.

Re-run:

```
cargo test -p reddb-io-server --lib -- --nocapture structural_hot_path_report
```

Criterion/measurement rule: compare rows only within one run. The ns/op figures
are machine-dependent and are never asserted; the load-bearing invariants are
the zero-alloc entries added to `COVERED_OPERATIONS` in `no_malloc_hot_paths.rs`,
which run under `cargo test`.

## Fase 0 — baseline and post-change measurements

| operation | allocations/op | ns/op | status |
|:--|--:|--:|:--|
| binding-merge-per-joined-row (a) | 2 | ~5773 | item 1 applied (Var interned) |
| hash-join-probe-key-extract (b) | 2 | ~2384 | item 3 skipped |
| group-by-existing-group-key-write (c) | 0 | ~1706 | ratcheted (already 0-alloc) |
| plan-cache-hit (d) | 0 | ~1760 | item 4 applied — was allocating |
| wal-commit-encode-into (e) | 0 | ~1150 | guard control |
| wal-pagewrite-encode-into (e) | 2 | — | item 2 skipped (evidence) |
| columnar-transpose clone (f, before) | 42 | — | item 5 before |
| columnar-transpose move (f, after) | 10 | — | item 5 after |

ns/op figures are indicative single-run numbers on the CI-class host; treat the
allocations/op column as the acceptance signal.

## Items

### 1. Intern `Var` (`Arc<str>`) — APPLIED

`Var` now holds `Arc<str>` instead of an owned `String`. Cloning a `Var` — which
happens per joined row in `Binding::merge`/`extend`/`project` and per hash-join
key extraction — is now a refcount bump rather than a fresh string allocation.
Equality and hashing still compare the underlying `str`, so `HashMap<Var, _>`
semantics are byte-for-byte identical (all existing binding/join/aggregation
tests pass unchanged).

The residual 2 allocations/op in `Binding::merge` are the `HashMap` backing
store (the clone of the map arena plus one insert grow); they are structural to
the immutable-binding design and independent of the key type. The task's
secondary suggestion — *move* the consumed side's map in `join::merge_bindings`
— is **not applicable**: the hash/nested-loop join executors hold each side as a
shared `&Binding` (a build row can match many probe rows and vice versa), so the
map cannot be moved out without changing the join contract. Interning is the
safe, contained win; it is confined to the `binding` module (the `name` field is
private and never constructed outside it).

### 4. Plan cache LRU O(1) — APPLIED

`PlanCache` replaced its `Vec<String>` recency list (linear `position()` +
`remove()` on every hit and removal) with a monotonic `clock` counter stamped
onto each entry's `lru_seq`. Promotion on the cache-hit hot path is now O(1) and
**allocation-free** (0 allocations/op, ratcheted), where before it scanned and
rebuilt a `Vec<String>`. Eviction selects the entry with the smallest stamp,
which is the exact same victim the old front-of-`Vec` policy chose for any given
access sequence — proven by `test_cache_lru_eviction` (unchanged) and the new
`lru_eviction_picks_least_recently_used_victim` test.

### 5. Columnar transpose move-not-clone — APPLIED

`ColumnarProjection::verify_and_decode_segment` transposed the decoded
column-major buffer into rows by cloning every cell. It now pre-allocates the
row vectors and drains each column with `into_iter()` in lockstep, **moving**
each `Value` into place. `decoded_columns` is local and dies at function end, so
moving is safe. Measured on a heap-owning-cell model (8×4), allocations dropped
from 42 to 10 (the residual 10 are the row-vector allocations, which are
inherent to producing owned rows). Row contents and ordering are unchanged.

## Skipped items (justified)

### 2. WAL `to_file_frame` without page clone — SKIPPED

First, the required proof of what the existing zero-alloc guard covers: the
`wal-record-encode-into-group-commit-buffer` guard exercises **only**
`WalRecord::Commit` (`no_malloc_hot_paths.rs`). It does *not* cover
`PageWrite`/`FullPageImage`/`TxCommitBatch`, whose `to_file_frame` clones the
full page payload — confirmed by the `wal-pagewrite-encode-into` measurement
(2 allocations/op for a 256-byte page vs 0 for Commit).

Skipped because the fix is genuinely invasive and format-adjacent:
`to_file_frame` returns an owned `MainWalRecordFrame` (defined in the
`reddb-file`/wire boundary and consumed by `encode_main_wal_record_frame_into`).
A borrowed-frame variant requires threading lifetimes through that cross-crate
encoder, and the byte-format-invariance requirement (bit-identical round-trip
against the recovery oracle) makes it a dedicated change rather than part of a
mechanical sweep. Recommend a follow-up issue scoped to the encoder seam.

### 3. Borrowed hash-join probe key — SKIPPED

`extract_key` clones the join-key `Value`s into an owned `HashKey`
(`Vec<Option<Value>>`) on every probe row (2 allocations/op measured). A
zero-clone probe needs either the unstable `raw_entry` API (nightly-only) or a
manual hashed-bucket table (`HashMap<u64, Vec<(HashKey, &Binding)>>`) with
by-reference comparison. The latter is a self-contained but non-mechanical
rewrite of `hash_join` with real correctness surface around the outer-join
`ptr::eq` build-row tracking. Deferred to keep this sweep low-risk; the `Var`
interning (item 1) already removes the per-key *string* allocation from the
build-side key construction.
