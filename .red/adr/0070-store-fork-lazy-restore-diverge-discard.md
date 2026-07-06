# ADR 0070 — Store fork: lazy restore-to-LSN, diverge-and-discard

Status: accepted
Date: 2026-07-05

## Context

Neon-style database branching — fork a production store in O(metadata), run an
experiment or a risky migration against it, throw it away — is the
agent-era capability Databricks leads with in the LTAP/Lakebase material
(2026-06). RedDB already holds every primitive it needs: MVCC time-travel
(#1383), an operational manifest with copy-on-write atomic replace (ADR 0042),
immutable append-only segments (ADR 0041), and a backup/restore boundary
defined as "restore a consistent checkpoint, then replay retained WAL to a
target LSN" (ADR 0043).

Two things had to be fixed before this becomes work: what the operation is
called (the VCS data model of #1567 already owns the word *branch*), and how a
fork can be cheap when mutable collection files are altered in place — Neon
sidesteps that only because all its data lives versioned in the PageServer.
The glossary term lives in [persistence.md](../context/persistence.md)
(*Store fork*).

## Decision

**1. The operation is a *store fork*, not a branch.** It forks the entire
store and lives on the storage/deploy axis, next to checkpoint and
backup/restore. The VCS data model's *branch* (#1567, `CHECKPOINT`
vocabulary) is version-control semantics over collection contents; a store
fork is storage mechanics for experiment-and-discard workflows. The two terms
are never interchangeable.

**2. A fork is a lazy in-place restore-to-LSN.** Creation writes a new
manifest generation pinned to a fork LSN — an O(metadata) operation, never a
physical copy. Immutable artifacts (append-only segments, checkpoint
artifacts, columnar projection segments per ADR 0069) are shared with the
parent by manifest reference. Mutable collection files are hydrated lazily —
replay/CoW on first touch by the fork — riding the exact restore contract of
ADR 0043. Cost is deferred to what the fork actually writes. From the fork
LSN forward the fork has its own private WAL.

**3. Lifecycle is diverge-and-discard; there is no merge-back.** A fork ends
one of three ways: discarded, kept as an independent store, or promoted to
new primary (which is also the recovery/PITR path). Merging divergent stores
is not a storage-layer operation — conflict resolution requires domain
knowledge, which is exactly what the VCS data model provides at the right
level.

**4. Fork LSN is bounded by WAL retention.** A fork can pin any LSN between
the operational WAL retention floor and the current durable LSN; forking
further back is a restore-from-backup, not a fork.

## Rejected, and why

- **Overloading `BRANCH` across both models.** Rejected — a single surface
  where the VCS branch is "a special case of store fork" reads elegantly and
  resolves to permanent ambiguity in every conversation, query surface, and
  doc. One word per concept.
- **Page-level copy-on-write / versioned mutable pages (true Neon).**
  Rejected — a heavy storage-engine rewrite duplicating what checkpoint +
  WAL replay already provide. The lazy-restore fork gets the same user-visible
  behavior from machinery that exists; page CoW remains a possible future
  optimization if fork-hydration latency ever matters.
- **Merge-back as a storage operation.** Rejected — reconciling divergent
  WALs needs conflict semantics the engine cannot invent, and no target use
  case (agent experiment, migration rehearsal, recovery) needs it; Neon
  ships without it too.
- **Per-collection fork granularity.** Rejected as the primitive —
  experiment-and-discard is whole-store by nature (schema, catalog, and
  cross-collection consistency come along); a collection-scoped need is the
  VCS model's territory.

## Consequences

- The operational manifest format gains fork metadata: parent store identity,
  fork LSN, and shared-by-reference artifact entries with hydration state.
- Shared immutable artifacts need refcounting (or equivalent) so parent-side
  compaction/retention cannot delete a segment a live fork still references —
  this couples fork lifetime into the WAL retention floor and compaction
  policy.
- Promotion reuses the restore/PITR path; discard is manifest deletion plus
  garbage collection of fork-private files.
- The embedded single-file `.rdb` profile (ADR 0038) forks by exporting
  through the operational layout — single-file stores have no shared-segment
  substrate; that asymmetry is accepted.

## Related

- ADR 0042 — operational manifest and DDL recovery (the manifest being forked)
- ADR 0043 — operational backup/restore boundary (the restore contract forks ride)
- ADR 0041 — operational collection layouts (immutable segment substrate)
- ADR 0069 — columnar analytics projection (forks share projection segments too)
- PRD #1567 — VCS via RQL (`CHECKPOINT`; owns *branch*/merge semantics)
- #1383 — versioned multi-model MVCC (time-travel substrate)
