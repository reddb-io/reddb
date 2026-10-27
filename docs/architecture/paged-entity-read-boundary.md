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
