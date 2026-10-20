# KV point lookup by physical key candidates

KV GET previously materialized all collection entities before comparing the
requested key. Versioned KV filtered a full scan and selected the visible
version with the greatest `xmin`.

Each segment now maintains a disposable hash lookup from randomized key
fingerprint to physical entity IDs. A probe verifies the complete text
key before applying the existing visibility predicate. Every retained version
is indexed: current-only secondary indexes cannot prove historical completeness.
Non-versioned KV preserves its physical-row lookup policy.
Both named rows and compact schema/column rows participate; point reads use
the shared field accessor so bulk-loaded and consolidated rows remain readable.

Cost sketch: with N collection rows, S segments and V retained versions of the
requested key, a normal point lookup changes from O(N) traversal/materialization
to expected O(S + V) candidate work, plus hashing/comparing the key bytes.
Fingerprint collisions add candidates and are always checked against the full key.
The segment probe borrows rows without allocations; the manager clones only a
selected candidate per segment. This does not establish constant-time lookup
independent of segment count or end-to-end superiority to another database.

Insertion, bulk insertion, deletion, key-changing replacements, snapshot adoption
and consolidation maintain the lookup under the existing segment write guard.
Raw mutable access disables it, since the caller can change the key after the
reference is returned. Those exceptional segments use an allocation-free borrowed
scan until consolidation/adoption creates a fresh indexed segment. Reads never
allocate a replacement index outside write admission. Updates to `xmin`/`xmax`
with an unchanged key preserve the index. The index is not serialized and does not alter
the WAL or file format. Normal recovery reconstructs it through entity insertion.

Hash buckets and version-vector capacity contribute to segment memory accounting.
Runtime row admission also reserves estimated index growth for text keys.
As with other segment estimates, this is not a hard allocator/RSS bound; direct
low-level mutations do not add a new admission API.

## Verification

```sh
cargo check -p reddb-io-server --locked
cargo test -p reddb-io-server --lib key_lookup --locked
cargo test -p reddb-io-server --lib segment_key_lookup_hit_allocates_nothing --locked
cargo test -p reddb-io-server --lib covered_storage_hot_paths --locked
cargo test --test grouped_general_multimodel e2e_vcs_kv --locked
```

The structural test loads 4,096 unrelated rows and two versions of the requested
key: exactly those two versions reach visibility evaluation, and an absent key
evaluates no rows. Other tests cover flat and hash storage, bulk ID gaps, mutable
key changes, forced sealed mutations, collision rechecks, snapshot parity with
the existing scan on another thread, paced consolidation and persistent reopen.
The allocation ratchet requires zero allocations for the borrowed segment hit.

An optional diagnostic compares the indexed segment probe with an early-exit
borrowed scan over 100, 10,000 and 100,000 rows (1,000 reads each):

```sh
cargo test -p reddb-io-server --lib key_lookup_scaling_diagnostic --locked -- --ignored --nocapture
```

This isolates lookup work. It excludes transport, parsing, durable writes and
the old full-collection cloning overhead; it is not a competitive benchmark.
