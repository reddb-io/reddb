# ADR 0069 — Columnar analytics projection: derived, native, automatic

Status: accepted
Date: 2026-07-05

## Context

Databricks' LTAP announcement (June 2026) validated a storage-layer idea RedDB
can adopt without adopting their architecture: analytical reads served from a
columnar representation that is *derived* from the transactional log, computed
off the write path, and merged with the not-yet-materialized tail at read time
so analytics is always fresh and never contends with OLTP.

RedDB's position is different — one engine, one transaction domain across
models — so the question was never "two engines, one copy" but: what role does
a columnar representation play inside our own storage hierarchy? The columnar
read win is already proven (#962 typed zero-copy decode); Analytics v0
(metrics/materializations) needs a substrate to scan. This ADR fixes that
role. Glossary terms live in [persistence.md](../context/persistence.md)
(*Columnar analytics projection*, *LSN-pinned analytical scan*).

## Decision

**1. Derived and disposable, never truth.** The physical WAL and row-oriented
collection files remain the sole source of truth. Columnar segments extend ADR
0032's derived-projection rule (the logical spool's): rebuildable at any time,
droppable and regenerable, and recovery/backup never depend on them. Repair
story is drop-and-rebuild, not restore.

**2. Derived on the checkpoint/compaction path.** The step that already
stabilizes data into immutable artifacts also emits the columnar counterpart.
The transactional write path never dual-writes columnar; the un-materialized
tail is everything past the last checkpoint boundary.

**3. First scope: append-only/timeseries collections.** In the append-only
segment layout the tail is pure inserts, so read-time merging degenerates to
concatenation. Mutable `table` collections require delete-vector/visibility
reconciliation over columnar segments and are a later extension, not part of
the initial contract.

**4. One consistency class: the LSN-pinned analytical scan.** An analytical
read is an ordinary MVCC read: pin the current snapshot/LSN, scan columnar
segments up to the last materialized LSN, concatenate the tail through the
normal row read path. Analytics is never a separate stale tier; `AS OF`
composes by pinning a historical LSN instead.

**5. Native format under existing authorities.** The columnar chunk is a
RedDB-native format owned by `reddb-file` (ADR 0046), under the same storage
checksum coverage and crypto page-envelope authority (ADR 0054) as every other
persistent artifact. Parquet/Iceberg interop, if ever offered, is an explicit
export feature — never the projection's storage contract.

**6. Automatic, with a floor and an escape hatch.** Every in-scope collection
gets the projection by construction — there is no opt-in list of
"columnar-enabled" collections. A size floor skips materialization where
bookkeeping would cost more than it saves (LTAP's tiny-table lesson); a
per-collection opt-out is the escape hatch for storage-sensitive deployments.

## Rejected, and why

- **LTAP-style inversion (columnar durable, row pages as cache).** Rejected
  for now — it couples recovery, backup, and PITR to an analytics format.
  Promotion to durable status for append-only collections remains a possible
  future ADR; decision 1 is deliberately the reversible choice.
- **Parquet/Iceberg in place as the projection format.** Rejected — it would
  bypass the crypto page envelope (ADR 0054) and move a persistent-format
  authority outside `reddb-file` (ADR 0046). Ecosystem interop is better
  served by an explicit export than by a storage contract frozen to an
  external spec.
- **Continuous WAL-chasing derivation.** Rejected — columnar wants batches and
  the checkpoint path batches naturally; freshness comes from the scan
  protocol (decision 4), not from keeping the projection hot.
- **On-demand/lazy materialization.** Rejected — the first analytical query
  would pay an unbounded materialization bill, exactly the latency cliff the
  projection exists to remove.
- **A second, stale consistency class for analytics.** Rejected — "analytics
  lags" recreates the user-visible OLTP/OLAP divide the unified engine exists
  to eliminate.
- **Opt-in per collection.** Rejected as the default posture — a list of
  columnar-enabled collections recreates the selective-CDC/mirroring pipeline
  category LTAP correctly buries ("a table that exists is already queryable").

## Consequences

- The checkpoint/compaction path gains a transcoding stage; it needs a budget
  so columnar emission can never starve checkpointing itself.
- `reddb-file` gains a versioned, checksummed columnar chunk format; the
  operational manifest gains projection entries (which must be marked
  derived, so backup/restore can skip or rebuild them).
- The query planner gains a hybrid scan: columnar segments up to the last
  materialized LSN + row-path tail concat, both under one pinned snapshot.
- Storage for in-scope collections grows by the compressed columnar copy; the
  size floor and per-collection opt-out are the cost levers.
- Analytics v0 metric materializations get their scan substrate without a
  second consistency story.

## Related

- ADR 0032 — WAL as source of truth, projections derived (the rule decision 1 extends)
- ADR 0041 — operational collection layouts (append-only segment layout)
- ADR 0046 — wire/file crate authority boundary (format ownership)
- ADR 0054 — crypto page-envelope authority (encryption coverage)
- #962 — typed zero-copy columnar decode (the proven read win)
- Databricks LTAP (2026-06) — the storage-layer freshness/merge pattern this adapts, and the engine-inversion it deliberately does not adopt
