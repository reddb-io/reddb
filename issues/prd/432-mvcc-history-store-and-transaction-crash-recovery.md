# PRD: MVCC history store and transaction crash recovery

Labels: enhancement

GitHub: https://github.com/reddb-io/reddb/issues/432

GitHub issue number: #432

## Problem Statement

RedDB currently has transaction and snapshot concepts, but the product guarantee is not sharp enough for a database engine that wants to compete with systems developers already trust. The most visible gap is that `UPDATE` behaves like an in-place mutation in important paths, which makes historical snapshots, savepoint reversal, and crash-recovery semantics weaker than the mental model developers bring from PostgreSQL and MongoDB/WiredTiger.

The concern is not whether RedDB has words like WAL, transaction, snapshot, or MVCC in the codebase. The concern is whether users can rely on these as hard guarantees:

- A transaction sees a stable snapshot.
- Writers do not corrupt readers' historical view.
- `UPDATE` creates a new logical version instead of destroying the previous committed version.
- `DELETE` is a visible tombstone decision, not an immediate physical disappearance.
- A committed transaction either fully recovers after crash or is absent.
- A transaction is acknowledged only after its commit record and live apply are both durable enough for the stated contract.
- Indexes never become an alternate visibility system with weaker MVCC semantics.

PostgreSQL teaches the tuple-version and WAL-durability side of this. MongoDB/WiredTiger teaches the global history-store side: keep old versions in a history structure and resolve visibility at read time. RedDB should not clone either engine blindly, but it should learn the architectural lesson: transaction visibility and crash recovery must be explicit contracts, not incidental behavior scattered through mutation paths.

## Solution

Introduce an engine-wide MVCC and transaction recovery architecture centered on logical identity, a global history store, an atomic transaction commit batch in WAL, and a single read resolver.

The first product slice is table-row correctness, because that is where SQL users most directly expect PostgreSQL-like behavior. The design must still be engine-shaped so document, graph, vector, queue, KV, and time-series models can adopt the same contract later without creating per-model transaction semantics.

Core decisions:

- Add stable logical identity distinct from physical version identity.
- Treat `UPDATE` as creation of a new physical version for the same logical entity.
- Preserve prior committed versions in a global history store keyed by collection/model/logical identity/version.
- Treat `DELETE` as a tombstone version visible according to MVCC rules.
- Keep transaction writes in an in-memory write set until commit.
- Commit with one atomic `TxCommitBatch` WAL record containing all writes for the transaction.
- Recover only complete commit batches in the first cut; incomplete and in-flight transactions disappear.
- Use snapshot isolation with first-committer-wins conflict detection by logical identity.
- Route user reads through a single MVCC resolver that resolves the visible version before returning data.
- Keep current indexes pointing at logical identity and require MVCC recheck on every indexed read.
- Provide manual `VACUUM`/GC for old history entries before adding an autovacuum daemon.

The guiding product claim becomes: after this PRD ships, RedDB can explain its transaction architecture in the same serious vocabulary as PostgreSQL and MongoDB, while remaining honest about what is still out of scope.

## User Stories

1. As an application developer, I want `UPDATE` inside RedDB to preserve the old committed version, so that concurrent readers do not observe partial mutation or lose their snapshot.
2. As an application developer, I want a transaction to see a consistent snapshot for the duration of that transaction, so that repeated reads do not change because another transaction committed later.
3. As an application developer, I want my own uncommitted writes to be visible to my transaction, so that read-your-own-writes works naturally.
4. As an application developer, I want another transaction's uncommitted writes to be invisible to me, so that dirty reads are impossible.
5. As an application developer, I want an `UPDATE` followed by rollback to leave the previously committed row visible, so that failed work does not corrupt committed state.
6. As an application developer, I want an `UPDATE` followed by savepoint rollback to restore the transaction-local prior view, so that savepoints work for real updates, not only inserts and deletes.
7. As an application developer, I want `DELETE` to create a tombstone version, so that old snapshots can still see the row while new snapshots see it as deleted.
8. As an application developer, I want `INSERT`, `UPDATE`, and `DELETE` in one transaction to commit atomically, so that no crash or reader can observe a partial transaction.
9. As an application developer, I want an acknowledged commit to survive process crash and restart, so that RedDB's durability claim is precise.
10. As an application developer, I want a transaction that crashed before commit acknowledgment to either be absent or clearly uncommitted after restart, so that no ambiguous half-state leaks into reads.
11. As an application developer, I want an autocommit statement to use the same commit pipeline as explicit transactions, so that correctness is not weaker in the common path.
12. As an application developer, I want concurrent updates to the same logical row to detect a write conflict, so that lost updates do not silently occur under snapshot isolation.
13. As an application developer, I want concurrent updates to different logical rows to proceed independently, so that MVCC does not over-lock the database.
14. As an application developer, I want indexed queries to return only rows visible to my snapshot, so that using an index does not bypass MVCC.
15. As an application developer, I want full scans and indexed scans to agree on visible results, so that query plans do not change transaction semantics.
16. As an application developer, I want historical snapshots to remain readable until no active transaction can see them, so that long-running reads are correct.
17. As an operator, I want manual vacuum/GC for obsolete history versions, so that disk use can be controlled before a background daemon exists.
18. As an operator, I want vacuum to respect active snapshots, so that maintenance never deletes a version still visible to a running transaction.
19. As an operator, I want crash recovery to replay complete committed transaction batches exactly once, so that restart does not duplicate or drop committed writes.
20. As an operator, I want recovery to ignore incomplete commit records, so that torn or partial WAL writes do not produce corrupted state.
21. As an operator, I want clear metrics or internal counters for active snapshots, history bytes, oldest retained xid, and vacuum progress, so that MVCC health is inspectable.
22. As a RedDB maintainer, I want logical identity separate from physical version identity, so that every data model can share one transaction vocabulary.
23. As a RedDB maintainer, I want a global history store rather than per-table inline version chains, so that multi-model storage can converge on one history mechanism.
24. As a RedDB maintainer, I want one MVCC resolver for all user reads, so that transaction visibility is not reimplemented differently in each query path.
25. As a RedDB maintainer, I want write-set semantics to be explicit, so that rollback, savepoint rollback, conflict validation, and commit batching are testable as separate concepts.
26. As a RedDB maintainer, I want the WAL commit record to describe the whole transaction batch, so that crash recovery can reason about transaction boundaries directly.
27. As a RedDB maintainer, I want first-committer-wins conflict detection by logical identity, so that snapshot isolation has a simple, defensible first implementation.
28. As a RedDB maintainer, I want legacy persisted rows without a logical identity to map logical identity to their current physical identity, so that existing databases can still boot.
29. As a RedDB maintainer, I want the table-row rollout to be isolated from later graph/vector/document adoption, so that the first slice can ship without pretending all models are complete.
30. As a user comparing RedDB to PostgreSQL or MongoDB, I want documentation that names the supported isolation and recovery guarantees exactly, so that I can evaluate the engine honestly.

## Implementation Decisions

**Product guarantee.** This PRD implements snapshot isolation with first-committer-wins conflict detection. It does not claim serializable isolation. Unsupported stronger levels should be rejected or documented as mapped only when intentionally equivalent.

**Logical identity.** Each user-visible entity has a stable logical identity. Physical version identity remains separate and identifies a concrete stored version. For existing persisted data, missing logical identity is interpreted as equal to the existing physical identity. The first rollout applies to table rows.

**Global history store.** Old committed versions move to a global history store rather than inline per-table heap chains. The conceptual key is collection identity, model kind, logical identity, descending validity xid, and version sequence. This follows the WiredTiger-style lesson more than the PostgreSQL heap-chain layout, because RedDB is multi-model.

**Versioned update.** `UPDATE` creates a new physical version for the same logical identity. The old committed version becomes historical and remains visible to snapshots that started before the update became visible.

**Versioned delete.** `DELETE` writes a tombstone version for the logical identity. Physical removal is a later vacuum/GC concern, never the delete-time visibility mechanism.

**Transaction write set.** Mutations inside a transaction remain in a transaction-local write set until commit. Reads check the write set first, then fall back to the committed snapshot resolver. This makes rollback and savepoint semantics explicit.

**Atomic WAL commit batch.** Commit writes one logical `TxCommitBatch` record containing all row versions and tombstones for the transaction. Crash recovery applies only complete committed batches.

**Commit ordering.** Commit ordering is: validate conflicts, build batch, append WAL, fsync or group-commit according to durability settings, apply live store/history/index changes synchronously, publish commit xid, then acknowledge the client. The first cut does not acknowledge before live apply.

**Recovery rule.** On restart, complete committed batches are replayed exactly once. Incomplete batches and in-flight transaction state are discarded. Prepared transactions and two-phase commit are out of scope.

**Conflict policy.** Snapshot isolation uses first-committer-wins by logical identity. If another transaction commits a new visible version for a logical identity after this transaction's snapshot and before this transaction commits, this transaction's conflicting write fails.

**Read resolver.** All user-visible reads flow through a single MVCC resolver that takes collection/model identity, logical identity, and snapshot, then returns the visible entity or no entity. Query paths may optimize lookup, but they do not own visibility semantics.

**Index semantics.** Current indexes point to logical identity, not physical version identity. Every indexed read rechecks MVCC visibility through the resolver. Historical index structures are out of scope for the first cut; correctness comes from recheck and fallback when the current index cannot directly prove a historical snapshot answer.

**Vacuum/GC.** The first maintenance surface is manual vacuum/GC. It may remove historical versions older than the oldest active snapshot and older than any configured retention floor. An autovacuum daemon is explicitly deferred.

**Documentation.** The transaction documentation must distinguish: supported isolation level, unsupported serializable behavior, commit durability contract, crash-recovery behavior, tombstone behavior, and vacuum requirements.

**Deep modules.** The architecture should produce small, testable modules with clear responsibilities: logical identity assignment, history-store read/write, transaction write set, commit-batch encoding/decoding, conflict validator, MVCC resolver, recovery applier, index recheck adapter, and vacuum candidate selection.

## Testing Decisions

Tests should verify observable database behavior and durable recovery, not private implementation shape.

**MVCC visibility.** Cover committed-before-snapshot visible, committed-after-snapshot invisible, aborted version invisible, deleted-after-snapshot still visible to old snapshot, deleted-before-snapshot invisible to new snapshot, and read-your-own-writes.

**Update semantics.** Cover update commit, update rollback, update savepoint rollback, update followed by delete, delete followed by update rejection or documented behavior, and multiple updates to the same logical identity in one transaction.

**Conflict detection.** Cover two concurrent transactions updating the same logical identity, two concurrent transactions updating different logical identities, update-vs-delete conflict, delete-vs-update conflict, and autocommit conflict behavior.

**Index correctness.** For the same dataset and snapshots, assert indexed lookup and full scan return the same visible rows. Include stale-current-index cases where the current version is not visible to an old snapshot.

**Crash recovery.** Use process-level or storage-level crash harnesses to cover crash before WAL append, after partial WAL append, after complete WAL append before live apply, after live apply before acknowledgment, and after acknowledgment. Expected behavior must match the commit ordering contract.

**Recovery idempotence.** Restart repeatedly from the same WAL and assert committed batches do not duplicate rows, indexes, history entries, or tombstones.

**Vacuum safety.** Cover vacuum with no active snapshots, vacuum with an active old snapshot, vacuum after old snapshot release, and vacuum across tombstones. Assert visible reads remain correct before and after vacuum.

**Compatibility.** Cover opening pre-logical-identity data and reading/updating it under the new logical identity mapping.

**Cross-model guardrails.** Even though table rows ship first, add tests that non-table models either use the new resolver where supported or explicitly reject/retain existing documented behavior. No silent partial MVCC claim.

**Docs conformance.** Add examples that demonstrate snapshot isolation, update versioning, delete tombstones, crash recovery boundary, and manual vacuum.

## Out of Scope

- Serializable isolation or SSI.
- Prepared transactions or two-phase commit.
- Distributed transaction consensus.
- Autovacuum daemon.
- Historical secondary indexes.
- Full multi-model rollout beyond table rows in the first implementation slice.
- PostgreSQL heap-page layout, HOT updates, TOAST, or tuple-header compatibility.
- MongoDB oplog compatibility or WiredTiger file-format compatibility.
- Acknowledging commits before live apply.

## Acceptance Criteria

- `UPDATE` no longer destroys the prior committed version needed by active snapshots.
- Snapshot reads are stable across concurrent commits.
- Rollback and savepoint rollback work for table-row updates.
- Concurrent writes to the same logical row fail with a deterministic conflict error.
- Commit recovery after crash is all-or-nothing at transaction-batch granularity.
- Indexed and non-indexed reads agree under MVCC.
- Manual vacuum reclaims eligible history without violating active snapshots.
- Existing persisted data can be opened through the legacy logical identity mapping.
- Documentation states exactly what isolation and recovery guarantees RedDB provides.
