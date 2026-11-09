# ADR 0082 — Bounded TTL, composite seeks, and maintenance isolation

Status: accepted
Date: 2026-11-09

## Context

Per-entity `WITH TTL` and `WITH EXPIRES AT` metadata was durable, but physical
deletion depended on an explicit retention call that scanned metadata. Paged
composite B-tree seeks only accepted a range on the final index column and then
scanned the complete resident WAL overlay. Both behaviours made work grow with
the database instead of with the requested batch or key interval. Synchronous
transports also admitted every query through one worker pool, so enough long
queries could delay a scalar liveness query.

## Decision

- Every collection maintains an ordered in-memory projection of persisted
  entity expiry metadata. Resume trailer version 6 persists the sorted
  deadlines, including a completeness marker for the empty case, so current
  files restore it without decoding rows. Older trailers are inventoried by the
  maintenance thread in bounded ticks; opening the database never performs a
  second full scan. The primary sweeper selects at most one configured batch
  from this projection and materializes deletes through the normal locking,
  MVCC, WAL, CDC, and replica paths. Replicas never sweep.
- A paged composite B-tree consumes a leading equality prefix and the first
  range immediately after that prefix. Its bounds include all suffix keys for
  an inclusive range endpoint. Post-checkpoint mutations live in an ordered
  overlay; lookup seeks that overlay instead of scanning every resident row.
  Matching ascending `ORDER BY` clauses consume candidates in index order, so
  execution avoids a sort and materializes only `OFFSET + LIMIT` rows.
  The overlay is cleared only after a successful checkpoint while the storage
  publication fence is still held.
- Background retention and MVCC reclamation use dedicated threads and bounded
  ticks. Server transports reserve one execution slot for explicitly classified
  scalar latency probes when at least two slots are configured; ordinary work
  cannot consume that slot.
- MVCC reclamation returns deleted record pages to the pager free list for
  reuse. Returning filesystem extents remains an explicit offline compaction:
  the source is locked and unchanged, output is written into an exclusively
  created directory, then completely read back before publication.
- `red.retention` and Prometheus expose TTL sweep counts, backlog signal and
  duration. Prometheus also exposes total and free physical pages. Offline
  compaction writes phase, tree and record progress to
  `compaction.progress.json` in its exclusive destination directory while work
  is active, then removes it before the single-file result is complete.

## Resource sketch

At 1,000,000 expiring rows, the deadline tree plus entity-to-deadline map is
approximately 64–128 MiB depending on allocator/node overhead; a 1,000-row tick
touches at most 1,001 deadline entries and can materialize at most 1,000 deletes,
while a composite seek touches only postings inside its encoded prefix/range.

## Consequences

- Expiry projection memory is proportional to the number of live scheduled
  expirations, not total rows. Rows without TTL add no projection entry.
- A crash during a sweep is equivalent to a crash during ordinary deletes:
  committed WAL outcomes replay and remaining deadlines survive in the resume
  projection. A crash while migrating an older trailer merely restarts its
  bounded inventory.
- Replicated deployments reclaim MVCC history only behind their existing
  replica low-water mark; TTL does not weaken that reuse proof.
- Physical compaction is intentionally not an online rewrite. An online file
  swap would require a separate generation/reader-pinning protocol and is not
  implied by this decision.
