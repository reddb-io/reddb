# ADR 0025: Overflow chain MVCC visibility via page-version tagging + per-page COW

Status: Accepted (2026-05-26)

Related: [ADR 0023: Large-value storage via overflow pages](0023-large-value-overflow-pages.md), [ADR 0003: On-disk format v1.0 stable contract](0003-disk-format-v1.md)

## Context

ADR 0023 introduces overflow chains: when a value exceeds `OVERFLOW_THRESHOLD`
(1024 bytes) and does not fit inline even after LZ4 compression, it is spilled
into a chain of dedicated overflow pages. The leaf cell retains a pointer
(`head_page_id`, `total_len`) and flags describing how to read it back.

ADR 0023 left a single open question explicitly flagged as blocking
implementation: **how do overflow chains participate in MVCC snapshot visibility
and atomic rollback?** Without a settled answer, the B-tree spill write path
(issue #702) cannot be coded safely.

Three pressures shape the decision:

1. **Snapshot visibility.** A reader at snapshot T must see exactly the chain
   that belongs to the leaf cell version visible at T, and never a
   half-overwritten chain from a concurrent writer.
2. **Atomic rollback.** A transaction that allocated overflow pages and then
   aborts must leave no orphan chains.
3. **Performance.** Readers walk overflow chains on every large-value read; any
   per-page visibility check has to be cheap.

Four operator-side constraints were stated up front (see issue #701 design
discussion):

- Asynchronous garbage collection is acceptable — short-lived ghost chains are
  tolerated provided a periodic sweep reclaims them.
- Updates of large values must not amplify writes. A 100 MB chain that changes
  4 KB cannot rewrite 100 MB.
- The WAL writer should stay minimal. Adding overflow-aware WAL record types is
  rejected; the existing per-page log path is the budget.
- Snapshot visibility checks for readers must be cheap (one comparison per page
  read, at most).

## Decision

Overflow chains use **page-version tagging with per-page copy-on-write**. The
mechanism reuses the leaf MVCC version for the chain root and extends MVCC
visibility downward to individual overflow pages.

Two pieces.

**Stable head, leaf-anchored chain identity.** The chain's `head_page_id` lives
in the leaf cell and is stable across versions of the same logical row. The
leaf cell already participates in MVCC — its version field governs whether a
reader sees that cell at all. The chain rooted at `head_page_id` is reached only
through the leaf cell, so chain *identity* inherits the leaf's MVCC at no extra
cost. No new WAL records are needed for chain attachment.

**Per-page version, per-page COW on update.** Each overflow page carries its
own MVCC version in its header. Updates rewrite *only* the pages whose payload
changes; unchanged pages stay in place and remain shared between leaf versions
that reference them. A reader walking a chain compares each page's version
against its snapshot and skips versions newer than its read horizon, falling
through to the prior live version via the chain's COW lineage.

The garbage collector — already responsible for reclaiming dead leaf versions —
is extended to walk overflow chains and reclaim overflow pages whose version is
not visible to any live snapshot.

### Sketch of the page header field

```text
OverflowPageHeader {
    page_type:  PageType::Overflow,
    version:    MVCCVersion,
    next:       Option<PageId>,
    payload_len: u16,
}
```

The `version` field is the only addition over the natural overflow header from
ADR 0023's slice B (issue #699). It must be 8-byte aligned so version reads are
atomic on the read path.

### Operation semantics

**Spill write (insert / large insert in `bulk_insert_sorted`).** Allocate
chain pages, stamp each with the transaction's MVCC version, write the head
pointer into the leaf cell as part of the leaf cell's normal MVCC write. WAL
records are exactly the page writes that already exist — no overflow-specific
record types.

**Read.** Resolve the leaf cell at the reader's snapshot. Follow `head_page_id`.
On each page, compare `page.version` to the snapshot horizon. If visible,
consume payload and follow `next`. If newer, follow `next` (the chain is laid
out so the prior visible page sits in the COW lineage — see below).

**Update (in place where possible).** When the new chain length matches the old:
walk the chain page by page, copy-on-write only the pages whose payload bytes
changed, leaving identical pages shared with the previous version. The new
leaf cell version points at the same `head_page_id`; readers at the old
snapshot see the old version of each rewritten page via its own version field.
When the chain grows or shrinks, append or truncate at the tail — the prefix
shared with the prior version is left untouched.

**Delete / abort.** Drop the leaf cell as usual. Do not free overflow pages
synchronously. The garbage collector, on its next pass, sees pages whose
versions are no longer visible to any active snapshot and returns them to the
free list. Aborted transactions leave the same residue and are cleaned up by
the same mechanism — no rollback-time chain walk is needed.

### COW lineage in growing/shrinking updates

The simplifying invariant is that within one chain, page positions are
addressed by chain offset, not by `PageId`. An update that changes the bytes
at offset *k* rewrites the page at offset *k* with a new version and a new
`PageId`; the predecessor's `next` pointer is updated to point at the new
page. The old page becomes unreachable from the new chain head but remains
reachable from old leaf-cell versions whose `head_page_id` is also unchanged —
because the predecessor's `next` was rewritten under MVCC, the old `next`
value still sits on disk as a prior version of the predecessor page.

In practice this means *any* page rewrite cascades into a rewrite of its
predecessor page (to update the `next` pointer), all the way up to the leaf
cell. The cascade length is bounded by chain depth at the modification point,
not chain length — a change near the tail of a 100 MB chain only rewrites
~⌈log₂(chain_position)⌉ pages if we layer the chain as a small skip list, or
the tail-only pages if we keep a flat linked list and the modification is
sequential append/truncate.

This ADR commits to the **flat linked list** today, on the grounds that the
target workload (large value writes and overwrites) is dominated by the
"replace whole value" case, where the new chain is allocated end-to-end with
no shared prefix with the old. For sequential append (tail growth), only the
old tail page and the new tail pages are written. For prefix or middle edits,
the cascade does walk back through `next` pointers — accepted as a corner case
that does not appear in the current workload mix, and revisitable as a future
optimization (skip-list overlay) if profiling shows it matters.

## Alternatives Considered

**Per-chain version tagging (no per-page COW).** Each chain carries a single
version; updates allocate a fresh chain end-to-end. Simpler reader logic, but
rejected — explicitly violates the "no write amplification" constraint. A
44 KB update to a 100 MB chain would rewrite 100 MB.

**Chain-as-cell-payload (chain identity moves with leaf cell version).** The
`head_page_id` is itself part of the leaf cell value, so each leaf version
references a distinct chain. Inherits leaf MVCC trivially and stays out of the
WAL. Rejected for the same reason: every update spawns a new chain end-to-end.

**WAL-anchored chain ownership.** Chain alloc/free are explicit WAL records
keyed by transaction id; rollback replays the WAL backwards to free chains.
Cleanest correctness story, but rejected — adding overflow-specific record
types breaches the "WAL stays minimal" constraint. Reader visibility would
also need to consult WAL state, which is not a hot-path-cheap operation.

## Consequences

**Positive.**

- Write path needs no new WAL record types; overflow page writes are page
  writes, indistinguishable from leaf and internal page writes at the log
  layer.
- Updates to large values are bounded by the change footprint, not the chain
  length, in the dominant append/truncate case.
- Reader cost is one version comparison per page consumed — no extra page
  reads beyond the chain itself.
- Aborted transactions degrade to the same garbage-collection path as ordinary
  dead versions, with no rollback-specific code.

**Negative.**

- The garbage collector grows responsibility for overflow page reclamation. It
  must track per-page visibility against the active-snapshot set, not just
  leaf-cell visibility. Until GC runs, dead overflow pages sit on disk as
  reclaimable-but-not-reclaimed slack.
- Updates that rewrite a page in the middle of a chain cascade back through
  predecessor `next` pointers. Acceptable for the current write mix; revisit
  with a skip-list overlay if profiling demands it.
- The on-disk overflow page header gains an 8-byte version field versus the
  bare minimum described in ADR 0023 (which assumed a versionless overflow
  page).

**Schema impact.**

- `OverflowPageHeader` definition (slice B, issue #699) gains the `version`
  field.
- The B-tree spill write path (slice E, issue #702) stamps every allocated
  overflow page with the writer's MVCC version.
- The garbage collector (separate concern, not introduced by ADR 0023 or this
  ADR) is updated to sweep overflow pages alongside leaf versions.

This ADR resolves the open question in ADR 0023 and unblocks slice E (#702),
which in turn unblocks slices F (#703) and G (#704).
