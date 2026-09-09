# Index topology during row admission

Row inserts through `MutationEngine::apply` retain a collection topology guard
from uniqueness validation and memory estimation until both row storage and
secondary-index maintenance finish. A concurrent runtime index creation, drop or
rebuild cannot change the registered index set inside that interval.

Previously the index registry read ended before memory admission. A paused,
already-admitted writer could resume against a newly created index that its
reservation did not include. Conversely, an index builder could collect its
backfill snapshot, miss a concurrent insert while metadata was still absent,
and publish an incomplete index.

## Synchronization

The guard is shared for ordinary inserts, including whole batches. Runtime
`CREATE INDEX`, `DROP INDEX`, table and typed-collection index teardown, tenant
index creation/removal, rebuild and registry rehydration take it exclusively.
Builders acquire exclusivity before collecting existing rows and retain it
through physical build and metadata publication. The low-level `IndexStore`
build/register/drop primitives rely on that caller-owned scope.

The first row carrying `id` may need an automatic index. That path releases its
shared guard, acquires exclusivity, and estimates again against the current
registry. It reserves segment and index growth together, creates the implicit
index before row publication, then downgrades to shared access for the write.
Other first writers recheck the registry, so they share one initialized index.
The existing first-row trigger and `auto_index_id` opt-out remain in effect.

A standalone UNIQUE HASH index can appear while a writer waits. If the writer
previously observed no need for the row constraint mutex, it releases topology
and retries acquisition in constraint-first order before checking keys.

Lock order is row constraint mutex, collection topology guard, memory reservation
mutex, and sampled storage/index locks. Registry/backend guards do not cross
admission. The memory sampler never acquires a collection topology guard;
pressure maintenance does not change runtime index topology. Index maintenance
releases its backend locks before reservation completion.

The row topology guard is released before CDC/event callbacks, which can invoke
more writes. The memory reservation keeps its existing lifetime through the
whole mutation. Collection-drop events also run before exclusive topology
acquisition; this change does not make their payload snapshot atomic with DDL.

## Cost and limits

Cost sketch: N inserted rows in one batch add one shared collection guard and
one warmed lock-directory lookup, rather than N locks; index build blocks that
collection for O(existing rows) backfill work. Other collection DDL and ordinary
row writers can proceed independently of this topology gate. Shared memory and
backend locks can still contend. This is not an online index-build algorithm or
a measured throughput improvement.

The lock directory retains one identity per collection name encountered during
the runtime lifetime, including drop/recreate, so queued operations cannot split
across two locks for the same name. Its metadata cost is O(distinct collection
names), not O(rows). Warmed lookup and shared acquisition allocate no heap memory.
Complete metadata accounting and reclamation remain follow-up work; this is not
a hard RSS bound.

This slice protects inserts through the unified row mutation engine against the
runtime topology callers listed above. Updates/deletes, other model writers,
replication/internal store writes and direct low-level index API users do not
acquire this guard yet. It therefore does not claim general concurrent DDL/DML
serializability, atomic tenancy-policy changes, or reader isolation during index
replacement. Those paths need their own scoped integration. Existing index-build
estimation and temporary backfill-buffer accounting remain unchanged. WAL format,
transaction rollback and existing index repair are outside this change.

## Verification

Tests pause a real row mutation after admission and try create/drop/rebuild,
tenant-index creation/removal, registry rehydration and table removal, for single and batch writes. They also
pause a real CREATE INDEX after its snapshot and before building, then verify
that every subsequently inserted key appears exactly once. Separate tests cover
concurrent first automatic indexes, UNIQUE HASH publication with eight waiting
writers, shared writer progress, independent collection DDL, and releasing
exclusivity on admission rejection. Allocation instrumentation
checks warmed lock lookup and shared acquisition.
