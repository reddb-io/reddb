# ADR 0014 — MVCC history store and transaction crash recovery

**Status:** Accepted
**Date:** 2026-05-12
**Supersedes:** —
**Superseded by:** —
**Related ADRs:** [0002 — MVCC promotion](0002-mvcc-promotion.md),
[0003 — disk format v1](0003-disk-format-v1.md)
**Related issue:** [#432 — PRD: MVCC history store and transaction crash recovery](https://github.com/reddb-io/reddb/issues/432)

## Context

RedDB has WAL, checkpoints, PITR, snapshot visibility, `xmin` / `xmax`
headers, and transaction control statements. That is not yet the same as a
complete MVCC storage contract.

The sharpest current gap is `UPDATE`: table updates mutate the existing
`UnifiedEntity` in place, so `ROLLBACK TO SAVEPOINT` cannot restore a
pre-update value. The pinned ignored test
`tests/e2e_savepoint_update_reversal.rs` records this known mismatch.

There is also a recovery boundary to tighten. In-process transaction state can
represent open transactions and savepoints, but committed multi-statement work
needs one crash-replayable unit that can be applied atomically after restart.

Two mature systems inform the direction:

- PostgreSQL stores multiple heap tuple versions. `heap_update()` inserts a new
  tuple version, marks the old one with `xmax`, links old to new with `t_ctid`,
  and can use HOT updates when indexed columns do not change.
- MongoDB's WiredTiger keeps update chains for current in-memory versions and a
  global history store for older versions. Recovery combines stable checkpoints
  with oplog replay.

RedDB should learn from both, but should not copy either blindly. RedDB's
storage is unified and multimodel; its MVCC design must fit `UnifiedEntity`,
cross-model transactions, VCS pins, replication, and existing WAL recovery.

## Decision

Introduce a RedDB MVCC storage contract based on a stable logical identity, a
global history store, and an atomic transaction commit record in the WAL.

This ADR ratifies the implementation contract for the first rollout:

- The first product guarantee is SQL table-row MVCC correctness.
- The shared engine concepts may be introduced in common storage modules, but
  non-table models must not claim the new guarantee until their read and write
  paths explicitly adopt the resolver.
- RedDB implements snapshot isolation with first-committer-wins write conflict
  detection. It does not implement serializable isolation or SSI.
- The first rollout includes manual `VACUUM` / GC only. It does not include an
  autovacuum daemon.
- Current secondary indexes plus MVCC recheck and fallback are the first
  correctness target. Historical secondary indexes are deferred.
- Full multi-model adoption is deferred behind table-row correctness.

### 1. Logical identity

Add a logical identity distinct from the physical entity version:

- `logical_id` is the stable identity visible to users, indexes, references,
  and DML targets.
- `EntityId` remains the physical version identity used by the store.
- Existing data lazily maps `logical_id = entity.id` when no persisted
  `logical_id` exists.
- The first rollout targets SQL table rows. Other `UnifiedEntity` models can
  initially keep `logical_id == entity.id` while the storage contract becomes
  available engine-wide.

This avoids overloading one id with two meanings. A future `UPDATE` can create a
new physical version without changing the user's row identity.

Persisted table-row records may add the logical identity in row metadata rather
than changing the meaning of `EntityId`. Legacy records without that metadata
resolve as `logical_id = entity.id`. The exact byte placement belongs to the
logical-identity compatibility slice, but the compatibility rule is final:
missing metadata must remain readable and updatable through the lazy mapping.

### 2. History store

Use a global internal history store, partitioned by collection and model in the
key:

```text
(collection_id, model_kind, logical_id, valid_from_xid desc, version_seq)
```

The main collection store keeps the newest/current version or tombstone. Older
versions move to the history store with an explicit visibility interval:

```text
valid_from_xid
valid_to_xid
physical_version_id
payload
```

The first implementation should store history entries using existing pager /
B-tree primitives where possible. A dedicated compact history record format is a
storage optimization and must not block table-row correctness.

This follows the useful part of WiredTiger's history-store shape: old versions
are kept out of the hot current store, can be garbage-collected with one policy,
and can serve old snapshots without forcing inline tuple chains into every
segment.

### 3. UPDATE semantics

`UPDATE` creates a new version instead of overwriting in place:

1. Read the currently visible version for `logical_id`.
2. Record that pre-image in the history store with `valid_to_xid = writer_xid`.
3. Create a new current version with the same `logical_id` and
   `xmin = writer_xid`.
4. Preserve the old version for snapshots whose xid predates the update.

The first implementation may apply this only to table rows, but the contract is
engine-wide: mutating a logical entity means producing a new visible version,
not destroying the pre-image.

### 4. DELETE semantics

`DELETE` writes a current tombstone instead of physically removing the logical
row immediately:

1. Move/copy the pre-delete current version to the history store with
   `valid_to_xid = delete_xid`.
2. Store a current tombstone for `logical_id`.
3. Remove current secondary-index entries.
4. Let older snapshots resolve the historical version.
5. Let VACUUM/GC reclaim the tombstone when no active snapshot, VCS pin, or
   replica requirement can still need it.

### 5. Transaction write set

Explicit transactions keep their mutations in an in-memory write set until
`COMMIT`.

Reads inside a transaction resolve in this order:

1. The transaction write set, for read-your-own-writes.
2. The current store if its version is visible to the snapshot.
3. The history store if the current version is too new or deleted for the
   snapshot.
4. No row if no visible version exists.

Savepoints are write-set boundaries. `ROLLBACK TO SAVEPOINT` discards write-set
entries at or after that boundary and does not need to undo durable storage.

### 6. WAL commit unit

Autocommit and explicit transactions must use the same logical pipeline. A
single-statement autocommit is an implicit one-statement transaction.

The first durable format should be an atomic `TxCommitBatch` WAL record:

```text
TxCommitBatch {
  xid,
  batch_id,
  mutations[],
  history_puts[],
  index_deltas[],
  crc
}
```

The replay rule is deliberately simple:

- no complete `TxCommitBatch` with a valid checksum: the transaction does not
  exist after restart;
- complete `TxCommitBatch`: apply it idempotently;
- torn record: truncate to the last valid WAL record.

Prepared transactions and two-phase commit are out of scope for this ADR. Any
`PREPARE TRANSACTION` surface must be rejected until a separate design lands.

Replica apply must preserve the primary `xid` and commit-batch ordering when it
replays a primary commit. A replica must not synthesize a replacement local xid
for a primary batch. Replication transport details are outside this ADR, but the
storage contract requires xid-preserving apply.

### 7. Commit ordering

The commit path must preserve WAL-before-data ordering:

1. Validate conflicts.
2. Build the `TxCommitBatch`.
3. Append to WAL.
4. Fsync or group-commit until the batch LSN is durable.
5. Apply current-store, history-store, and index changes synchronously.
6. Publish the xid as committed.
7. Acknowledge success.

The first implementation should not acknowledge after WAL fsync but before
applying to the live store. That optimization would require a
committed-but-not-applied overlay in every read path and is not worth the first
cut's correctness risk.

### 8. Conflict policy

Use snapshot isolation with first-committer-wins by `logical_id`.

Each transaction records the base version it read or intended to update. At
commit time, validation fails if another committed transaction changed that
`logical_id` since the transaction's snapshot.

Readers do not block writers. Writers do not block readers. Concurrent writers
to the same logical row race at commit; one succeeds and the other gets a write
conflict.

`SERIALIZABLE` / SSI remains out of scope. RedDB should continue rejecting
`SERIALIZABLE` rather than silently downgrading it.

### 9. Read API

All user-data reads must resolve through one MVCC API, conceptually:

```rust
resolve_visible(collection, logical_id, snapshot) -> Option<UnifiedEntity>
```

That API owns:

- transaction write-set overlay;
- current-store lookup;
- history-store lookup;
- tombstone handling;
- committed/aborted xid checks;
- VCS / AS OF / replica pins when applicable.

Fast paths that fetch by `_entity_id`, secondary index, zone map, or cached
candidate list must re-enter this API before returning user data. Admin,
VACUUM, forensic export, and physical inspection can use explicitly named
physical-version APIs.

### 10. Indexes

The first cut uses current secondary indexes plus a correct fallback for old
snapshots.

- Current indexes map key to `logical_id`.
- If an update changes an indexed column, the current index removes the old key
  and inserts the new key.
- Reads at current snapshots use the current index and recheck MVCC.
- Reads at old snapshots, `AS OF`, or VCS-pinned snapshots must fall back to a
  version-aware scan/history lookup when the current index cannot prove
  completeness.

Historical indexes are a performance follow-up, not a correctness requirement
for the first cut.

For old snapshots, `AS OF`, or VCS-pinned reads, correctness beats index use. If
the current index cannot prove completeness for the requested snapshot, the
query path must fall back to a version-aware scan or history lookup before
returning user data.

### 11. VACUUM / GC

The first cut must include manual GC, but not an autovacuum daemon.

`VACUUM` removes history versions and tombstones whose xid is older than the
oldest required xid. That horizon must consider:

- active transactions;
- active snapshots / cursors;
- VCS and `AS OF` pins;
- replica or recovery pins, when exposed;
- any other long-lived reader registered with the snapshot manager.

Expose metrics for history-store bytes, version count, oldest pinned xid, and
reclaimable versions. Autovacuum can be designed later using those metrics and
thresholds.

### 12. CDC and replay side effects

`TxCommitBatch` replay is a storage recovery operation and must be idempotent.
It must not duplicate externally visible CDC side effects after restart.

Live commit apply may emit CDC events after the batch has a stable identity, or
replay may feed a durable outbox keyed by `batch_id`, but either approach must
include an idempotence marker. The first MVCC storage slices do not need to
finish the CDC surface; they must only avoid a design that makes duplicate CDC
events unavoidable.

## Consequences

### Positive

- `UPDATE` becomes true MVCC instead of in-place mutation.
- Savepoint rollback can restore update pre-images.
- Crash recovery gets a single durable transaction unit.
- Explicit and autocommit writes share one correctness path.
- The design aligns with RedDB's unified/multimodel architecture while learning
  from PostgreSQL and WiredTiger.

### Negative

- Every read path must be audited for MVCC resolution. Existing fast paths that
  bypass visibility checks become correctness risks.
- Current indexes alone are not enough for performant historical reads.
- The history store adds write amplification and requires GC.
- The format may need a compatibility strategy for `logical_id` persistence and
  history-store records.

### Neutral

- This ADR does not add `SERIALIZABLE`, predicate locking, prepared
  transactions, distributed transactions, or autovacuum.
- PostgreSQL-style HOT updates remain a future optimization. RedDB can add a
  HOT-like shortcut when an update does not change indexed fields, but only
  after the baseline versioning contract is correct.

## Implementation slices

1. [#434](https://github.com/reddb-io/reddb/issues/434): add the
   `logical_id` abstraction with lazy `logical_id = entity.id` compatibility.
2. [#435](https://github.com/reddb-io/reddb/issues/435): route autocommit table
   `UPDATE` through versioned row writes.
3. [#436](https://github.com/reddb-io/reddb/issues/436): add the transaction
   write set for rollback-safe table updates.
4. [#437](https://github.com/reddb-io/reddb/issues/437): make savepoint
   rollback discard transaction-local update pre-images.
5. [#438](https://github.com/reddb-io/reddb/issues/438): implement table-row
   `DELETE` tombstones and old-snapshot visibility.
6. [#439](https://github.com/reddb-io/reddb/issues/439): enforce
   first-committer-wins write conflicts by `logical_id`.
7. [#440](https://github.com/reddb-io/reddb/issues/440): add `TxCommitBatch`
   WAL coverage for autocommit table writes.
8. [#441](https://github.com/reddb-io/reddb/issues/441): route explicit
   transaction commit through `TxCommitBatch`.
9. [#442](https://github.com/reddb-io/reddb/issues/442): add current-index
   MVCC recheck and old-snapshot fallback.
10. [#443](https://github.com/reddb-io/reddb/issues/443): add manual
    `VACUUM` history/tombstone GC.
11. [#444](https://github.com/reddb-io/reddb/issues/444): document table-row
    guarantees and cross-model guardrails.

## Required tests

- `UPDATE` inside savepoint rolls back to the pre-update value.
- Concurrent transactions updating the same `logical_id`: one commit succeeds,
  the other fails with write conflict.
- Snapshot reader sees old value while concurrent update commits.
- Autocommit update crash after WAL fsync but before apply recovers the update.
- Crash before complete `TxCommitBatch` loses the transaction.
- Replay of the same `TxCommitBatch` is idempotent.
- Index lookup never returns a version invisible to the active snapshot.
- Historical snapshot can find the old value after an indexed-column update.
- `DELETE` tombstone hides current reads and preserves old snapshot reads.
- `VACUUM` does not remove versions pinned by active snapshots or VCS commits.

## Resolved questions and deferrals

- `logical_id` encoding: the compatibility rule is accepted now. Legacy rows
  without persisted logical identity map to their physical `EntityId`; new
  table-row versions may persist logical identity in row metadata. Exact byte
  placement is delegated to #434 because it is a format detail, not an
  unresolved semantic contract.
- History-store physical format: the first implementation should reuse existing
  pager / B-tree primitives when they are sufficient. A compact dedicated
  history format is deferred until correctness is proven.
- Replica apply: primary xids and commit-batch ordering are part of the storage
  contract. Replication transport work may be later, but replay/apply must not
  rewrite primary xids.
- CDC emission: recovery replay must be idempotent and must not duplicate
  external CDC events. Exact outbox or live-emission plumbing is deferred, but
  every design must key side effects by `batch_id` or an equivalent durable
  idempotence marker.
- Old-snapshot index performance: v1.0 table-row correctness requires MVCC
  recheck plus fallback scan/history lookup. Historical secondary indexes are
  explicitly deferred as a performance feature.

## Amendment — 2026-06-25: multi-model history adoption (lifting the non-table deferral)

**Status:** Accepted — extends the Decision above.

The original rollout contract deferred non-table models: *"non-table models must
not claim the new guarantee until their read and write paths explicitly adopt the
resolver"* and *"Full multi-model adoption is deferred behind table-row
correctness."* Table-row correctness is now shipped and stable (released in
v1.14.0), so that deferral is lifted by product decision: **non-table collections
explicitly marked `versioned` adopt the MVCC history-store resolver**, so
AS OF / time-travel / VCS merge work across models, not only tables. This is what
makes the VCS ("Git for Data") premise hold for every model.

### Gating principle

History retention for non-table models is **opt-in per collection via the
`versioned` flag** (`runtime.vcs_is_versioned()`). Non-versioned collections —
including the `red_config` KV and the secrets vault — keep last-writer-wins and
the zero-scan fast path unchanged. Table rows remain unconditionally versioned
(the original baseline). The xmin/xmax stamping, `logical_id` version-selection
(`resolve_read_candidate`), commit-time first-committer-wins
(`check_table_row_write_conflicts`), tombstone-on-delete, and VACUUM reclamation
are reused unchanged — every model travels the same machinery rather than
duplicating rules.

### Adopted (branch `feat/versioned-kv-mvcc-history`)

- **KV** — full: tombstone-on-PUT/DELETE, snapshot version-selection (keyed on the
  `key` field), first-committer-wins. Caught a real bug along the way: the
  autocommit-pool xmin is non-monotonic vs commit xids, so the new version's xmin
  is restamped via `begin()/commit()`.
- **Document** — full: stored as `EntityKind::TableRow` with `logical_id`, so reads
  and SQL `UPDATE` already versioned; the only gap was PATCH (mutated in place) —
  now emits a versioned post-image.
- **Graph** (nodes/edges) — full: own `EntityKind` but the machinery is
  kind-agnostic; stamp `logical_id`, open the PATCH/UPDATE and DELETE gates; the
  read path was already snapshot-correct.
- **Vector** — live-read correct + write-side history: the TurboQuant index is
  append-only (no removal), so search post-filters every candidate by
  snapshot/xmax visibility. This also fixed a pre-existing bug where
  `DELETE FROM <vector> WHERE rid=N` matched zero rows (vector deletes never took
  effect) and deleted vectors lingered in search results. Versioned vector deletes
  tombstone with first-committer-wins.

### Still deferred (scoped follow-ups; `#[ignore]`d specs left in `tests/grouped/vcs/`)

- **Vector AS OF read surface** — `VECTOR SEARCH` has no `AS OF` grammar and vectors
  are not row-scannable, so reading a historical vector version back needs
  grammar + planner + a historical-version resolver. Write-side history is retained
  physically, so this lands later without a data-model change.
- **CDC/watch-event transactionality** — KV (`record_kv_watch_event`) and document
  CDC emit eagerly during the write rather than at commit, so a rolled-back or
  conflicted versioned write still emits its event. The table path defers CDC to
  `finalize_pending_*`; the non-table paths should follow (consistent with the CDC
  idempotence deferral above).
- **PATCH vs SQL-UPDATE gating** — SQL `UPDATE` versions table rows unconditionally
  (the baseline); the new non-table PATCH path is flag-gated. For a non-versioned
  document the two verbs therefore differ in history accrual (invisible without
  AS OF, which is rejected for non-versioned). Align if uniform behavior is wanted.
