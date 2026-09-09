# Index topology during updates and deletes

SQL `UPDATE` (including read-modify-write), SQL `DELETE`, native entity
`PATCH`/delete, and versioned-update undo now share the collection topology
guard introduced for [row admission](index-topology-admission.md). The guard
keeps the index set stable from source hydration through storage publication
and secondary-index maintenance.

Without this scope, native PATCH can finish after a SQL index builder snapshots
records but before it publishes metadata. The builder then indexes the old
value and the new stored value has no index posting. Direct runtime index DDL
can similarly race SQL UPDATE: it does not pass through SQL dispatch's existing
collection intention locks. Both interleavings were reproduced before the fix.

## Scope and lock order

Ordinary SQL mutations acquire shared topology access for the target-ID scan,
release it, then acquire it for each existing apply/delete chunk before loading
source entities. Read-modify-write UPDATE retains its existing candidate batch
and table/row lock granularity. Native PATCH/delete hold shared access for the
single operation. Versioned-update undo holds it while removing the new version
and restoring the original version and its index entries.

The existing constraint mutex and table/claim locks precede topology; per-row
read-modify-write locks follow topology. Index builders never take those
constraint/RMW locks. These changes do not acquire a constraint or table lock
while already holding topology. Internal helpers consume the shared guard so
its lifetime includes both persistence and index maintenance, including errors.

Savepoint undo snapshots the affected undo identities, releases the pending
journal lock, then acquires topology before reacquiring that journal. Each entry
is revalidated and removed only after successful restoration. This preserves
the topology-before-journal order used by mutations; waiting for topology while
holding the journal would permit a deadlock with a writer recording its undo.
Full transaction rollback already detaches its pending journal before undo.

UPDATE and native mutations release topology before CDC emission and event
callbacks. SQL DELETE retains it through its existing internal CDC log/ring
emission, which does not synchronously invoke user callbacks, and releases it
on batch return before queue/event callbacks. No event payload buffer is added.

This is per-operation/per-chunk coordination with runtime create/drop/rebuild
and the other exclusive topology callers. It does not hold a lock for the
whole transaction or make target selection and every chunk a single atomic
statement. Existing SQL intention locks and MVCC visibility still apply.

## Cost and remaining work

Cost sketch: N ordinary SQL mutations in chunks of B add one warmed collection
lookup, one shared acquisition for the scan and ceil(N/B) shared acquisitions
for application. Native operations add one lookup/acquisition. No new network
round trip, fsync, per-row payload copy or event queue is introduced. Backfill
can block mutations on that collection for O(existing records); writers retain
shared access and unrelated collection DDL uses a different guard. This is not
online index creation or a measured performance gain.

Savepoint undo copies O(affected updates) journal identities, without copying
record payloads. Reverse-order removal searches from the journal tail, so the
ordinary savepoint suffix is processed in linear time.

Updates and patches still need growth admission for larger values/index keys.
Other model-specific writers, raw store/replication paths, low-level index API
users and other maintenance remain separate integration work. This slice does
not redesign UNIQUE UPDATE preflight, general concurrent DDL/DML serializability,
collection drop/recreate semantics, tenant-policy atomicity, index reader
isolation, WAL/recovery, or existing error atomicity. There is no file or wire
format change.

The public native entity seam includes document fields and node properties;
this coverage does not establish edge index backfill or universal model parity.
Strong shared guarantees support the [multimodel program](multimodel-building-block-program.md),
but complete public workflows and equivalent competitive measurements are
still required before claiming superiority over SurrealDB.

## Verification

`runtime::mutation_index_topology_tests` exercises real storage and index paths:

- Backfill paused after snapshot versus SQL/native mutations, using SQL and
  direct runtime DDL: tables/documents cover UPDATE, RMW, PATCH and both deletes;
  graph nodes cover native PATCH/delete (24 interleavings).
- Storage publication paused before maintenance versus create/drop/rebuild
  across five mutation paths (15 interleavings).
- Same-collection DDL at the event boundary, validation-error guard release,
  concurrent writer progress and unrelated collection DDL.
- Transactional UPDATE followed by rollback while native backfill is paused;
  the restored value is queryable and the undone key has no posting.
- Savepoint rollback blocked on topology leaves the pending journal available
  to writers, restores the indexed value and permits the transaction to commit.

Hooks exist only in test builds. Channel handshakes pause the actual production
seams; bounded waits probe blocking, and index postings/public queries verify
the final state. Broader validation includes parent admission/topology tests,
SQL and transactions, document/graph/time-series suites, events, tenancy/RLS,
allocation checks, compilation and linting.
