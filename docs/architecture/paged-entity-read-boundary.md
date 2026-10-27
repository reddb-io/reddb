# Fallible paged entity reads

The native paged runtime still restores every physical entity into resident
segment managers at open. A page-cache cap does not evict these owners. In the
small native reproduction, 2,048 rows with 8,388,608 bytes of payload consumed
10,543,588 accounted bytes before a query, against a 2,097,152-byte budget.
The 32-row control passed two opens, indexed payload reads and a full count.
Rejecting an otherwise valid paged database is not a disk-residency fix.

## Read boundary introduced here

`reddb_engine::storage::paged_collection::PagedCollectionReader` reads the
existing B-tree and entity/metadata formats without constructing a resident
collection. Attaching a known root performs no page reads. Point lookup reads
one physical record; its cursor decodes records individually in physical-ID
order. Both return errors, preserving native open's contextual integrity
classification and underlying cause. A key must be eight bytes and match the
decoded physical entity ID. A failed cursor stays fenced rather than skipping
a damaged row or reporting clean EOF.

The reader preserves metadata, sequence identities and MVCC stamps. It does
not apply transaction visibility or choose a logical row version. It retains
the actual tree handle and cursor retirement pin; these prevent reclamation,
not concurrent in-place changes. Callers must fence mutations or supply a
published immutable generation. Native boot already has exclusive ownership.

Native boot now uses this same decoder/cursor, **but continues restoring the
complete collection**. This is a checked read boundary for the residency change,
not its activation. Existing checkpoint, WAL-before-page, double-write and
metadata publication order remain unchanged. No persistent format changes.

Cost sketch: attaching is O(1) in rows; a point lookup reads O(tree height)
pages plus that value's overflow chain; a full cursor reads O(collection
bytes) while retaining one encoded/decoded value at a time. The existing
B-tree value decoder still needs read-workspace admission before arbitrarily
large values can be served within a small process budget. This boundary does
not impose that admission or a hard RSS bound.

## Isolated read evidence

The native-file regression reopens 2,048 distinct 4,096-byte blob payloads,
verifies all rows/metadata/MVCC stamps/sequence IDs, and performs three point
lookups. An independent test allocator measures opening and the complete scan.
With the runtime's actual 2 MiB serverless budget share (24 cache slots), the
extra allocation peak was 1,446,239 bytes; the three seeks used 12 page-store
probes, including cache hits. Attachment made zero page-store probes. The
cursor retained its retirement pin after the reader was dropped and released
the source when closed. Injected page-read errors propagated; a fresh cursor
could read after the fault was removed, while the failed cursor stayed fenced.
Native open's key/payload mismatch test failed before this change and passed
afterward. These are isolated read results, not runtime/OCI acceptance.

The initial fixed 64-slot fixture failed the same 2 MiB allocation assertion
at 2,104,031 bytes. Stage measurements attributed 1,056,207 live bytes to the
opened pager and another 1,047,824 bytes to its temporary opening peak; the
recovery path reads the fixed 1 MiB DWB zone. Point reads and the complete scan
never increased that peak. The fixture was aligned to the runtime's existing
budget-derived cache share; no production budget, dataset or runtime acceptance
gate was relaxed. The full runtime still eagerly restores the cold payloads.

## Read authority and mutation composition

`storage::collection_read::CollectionReadView` now distinguishes a complete
resident collection from a published paged base plus immutable, admitted
changes. An absent change reads the base; an explicit deletion masks it; a
replacement supplies the current physical record. Overlay record owners are
shared across captured views instead of copying all changed payloads. Map
metadata and retained owners still need budget accounting by their publisher.

The paged visitor merges both sources in physical-ID order, emitting each ID
once and retaining metadata and MVCC stamps. It decodes one cold record at a
time. Owned point/page results restore the shared positional row schema while
preserving explicit schemas and named fields. A failed scan returns an error,
including after an already visited prefix; it never reports a complete page.
Stopping a visitor does not read another record.

Native physical page reads and ten entity-port reads now use this fallible
interface. The legacy resident-only getter no longer falls back to an old
tree after a complete resident miss: a pre-checkpoint physical deletion had
been resurrected by that fallback in the failing regression. Recovery reads
remain explicit paged-reader operations. Current native boot still restores
every row, and its view factory selects only complete resident authority.
The paged composition is not yet activated as runtime cold ownership.

SQL table scans and indexed candidate hydration now use fallible views, with
the same statement visibility and deadline checks. Aggregate, batch, maintenance
and index reads that still access resident managers must be migrated before
that activation. Mutation, replay, allocator
high-water marks and checkpoint composition are also still pending. A captured
overlay is immutable; an Arc to a B-tree alone does not stop in-place writes.
The publisher must preserve the existing fencing and generation rules.

## Remaining integration, in order

1. Route query, maintenance, index and mutation reads through fallible views of
   published cold records plus current resident changes. An `Option`-only
   resident lookup cannot safely hide an operating error as a missing row.
   Evicted rows and completed physical deletes must remain distinguishable:
   a resident miss alone cannot authorize fallback to an older disk record.
2. Preserve complete scans, logical visibility, metadata and physical identities
   when cold records leave segment ownership. Cold bootstrap also needs durable
   collection counts and allocator high-water marks; current open reconstructs
   them by loading every entity. Avoid ID reuse after deletes and WAL replay.
3. Compose checkpoint/index inputs from cold state and the admitted mutation
   delta. Publishing an empty or partial resident manager must never replace
   the complete durable tree. Reader-pinned generations remain unreclaimable.
4. Charge read workspace, resident changes, retained generations and decoded
   hot state to the shared budget. Publish durable changes before eviction;
   retain snapshot-required versions until readers finish.
5. Make the native runtime budget reproduction pass with complete data and
   indexed reads, then qualify the same final artifact on the frozen Auth and
   Platform sources under actual limits. Preserve the 10-second gate and the
   content, index, snapshot, recovery, shutdown and repeated-space checks.

ADR 0038 keeps embedded's resident scale contract distinct from the server
disk-residency roadmap. ADR 0073 still requires shared budget enforcement;
neither contract is silently changed by introducing this read boundary.

Fleet rollout, observation and supported OCI downscale with rollback follow
engine acceptance. Partitioned BigQuery resource/configuration/version/AI
metering and the STG free database suspend/wake experiment remain separate,
unfinished requirements of the original mission.

## Development feedback loop

Use the shared engine cases while changing read authority and composition:

```sh
cargo test --locked -p reddb-io-engine --test portable portable_collection_read_conformance -- --nocapture --test-threads=1
```

These cases reopen actual native-format bytes through the shared pager and a
memory VFS. They cover overlay ordering/replacements/deletions, schema and
metadata, preserved visibility stamps, I/O failure versus absence and early
stop. They are also registered in the WASM conformance runner. They avoid
building the server, but changing engine code still requires its recompilation;
the initial isolated dependency graph also needs its own build cache.

The runtime acceptance reproduction remains a separate, deliberately failing
regression until native cold ownership is integrated. It is not ignored:

```sh
cargo test --locked -p reddb-io-server --test paged_open_memory_budget -- --nocapture --test-threads=1
```

Passing the fast cases does not qualify runtime memory, native host I/O, Auth,
Platform or the historical source. Retain those later gates and the fixed data,
limits, durability and recovery requirements.


## Indexed candidate materialization

The owned ID iterator now hydrates one candidate per `next()` for resident and
paged authority. Indexed SQL projection paths stop requesting candidates when
their output limit is satisfied; missing IDs and duplicates retain their input
positions. An I/O failure fences the iterator and propagates to the query.
The resident SELECT-star visitor retains its existing borrowed fast path.

The native engine regression `collection_read_memory` uses an independent test
allocator and 2,048 resident records with distinct physical IDs and 8 MiB of
blob payload. Creating the previous iterator copied all candidates: 9,027,584
extra bytes, with a 9,043,968-byte peak despite consuming only one row. With
lazy hydration, construction adds zero bytes and the same consumed prefix peaks
at 8,240 extra bytes; dropping it retains zero payload bytes. The unchanged
workspace assertion is 128 KiB. This regression runs without compiling the
server. It measures extra candidate ownership, not total process RSS, cold boot
or arbitrary-value workspace admission.

```sh
cargo test --locked -p reddb-io-engine --test collection_read_memory -- --nocapture --test-threads=1
```

This bounds ownership of the unconsumed suffix, not the size of a single value,
the ID candidate set or accumulated query results. Read-workspace admission and
native cold ownership remain required before accepting the full runtime budget.


The native SQL regression also caught hash-index results returning before
ORDER BY, OFFSET and LIMIT finalization. Hash, bitmap-intersection and combined
hash/range paths now use the same existing finalizer as sorted-index reads.
Ordered/offset queries retain their complete matching input before slicing;
sort-only source columns are removed by the existing projection finalizer.
The regression checks projected and SELECT-star payloads, ascending/descending
windows, offsets, hidden sort columns, update/delete and native checkpoint/reopen.
All 38 selected native read/checkpoint/recovery tests passed. The unchanged
full-runtime opening gate still fails at 10,543,588 accounted bytes for 8 MiB
of payload and a 2,097,152-byte budget. Query ownership improvements therefore
do not admit cold-runtime activation, historical qualification or fleet rollout.
