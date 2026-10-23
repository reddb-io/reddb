# Persistence — RedDB Domain Glossary

Part of the [glossary map](../CONTEXT-MAP.md). The storage engine shared by **every** deployment profile (standalone, primary-replica, cluster). Profile-specific packaging lives in the sibling files; this file owns the engine underneath them.

## Cache

- **Blob Cache** — the native tiered (L1 RAM + L2 disk) byte-oriented cache module living under `crates/reddb-server/src/storage/cache/`. Operates by `(namespace, key)` lookup. Distinct from the page cache (which caches database pages, not user-visible blobs) and the result cache (which caches SQL result rows; itself now a Blob Cache adapter per #143).
- **L1** — in-memory, sharded SIEVE-evicted hot tier of the Blob Cache. Default 256 MiB, 64 shards.
- **L2** — durable on-disk tier of the Blob Cache. Default 4 GiB. Uses B+ tree metadata + native blob chains. WAL-ordered metadata-last.
- **Synopsis** — per-namespace negative-only Bloom filter for fast L2 misses. Default ~12 KB/namespace at 1% FPR. Returns `MaybePresent` on hit (caller must verify against L2 metadata for an authoritative answer).
- **CachePresence** — three-valued return type from `BlobCache::exists`: `Present`, `Absent`, `MaybePresent`. The synopsis can return `MaybePresent`; only the metadata B+ tree can return `Present` authoritatively.
- **Namespace** — top-level partition in Blob Cache, capped at 256 in MVP. Separate quota, separate generation counter (for O(1) flush), separate synopsis filter.
- **Generation counter** — per-namespace u64 used to invalidate all entries in a namespace in O(1) by bumping the counter; old entries become invisible without walking each key.
- **L1Admission** — policy enum (`Always`, `Auto`, `Never`) deciding whether a put inserts into L1 or skips straight to L2.
- **AsyncPromotionPool** — bounded background task pool that runs L1 promotion when an L2 hit happens, so the read caller doesn't pay the promotion cost in their latency budget.
- **L2BlobCompressor** — content-type-aware zstd wrapper that compresses L2 blobs above a size threshold. Two on-disk variants: `Raw` (tag=0) and `Zstd` (tag=1 + 4-byte original_len).
- **ExtendedTtlPolicy** — opt-in extension to the cache policy carrying `idle_ttl_ms`, `stale_serve_ms`, and `jitter_pct`. Off by default; per-entry rather than global.
- **Page cache** — internal sharded SIEVE cache of database pages. Distinct from the Blob Cache. Lives at `storage/engine/page_cache.rs`.

## Resource governance

- **Memory budget** — per [ADR 0073](../adr/0073-memory-budget-governance.md), the explicit memory ceiling every RedDB process runs under, resolved at boot (operator config > profile default > host/cgroup detection — never "unlimited"). Big structures (page cache, segment arenas, index memory, WAL buffers) are pre-sized from it; admission that would exceed it fails didactically or triggers reclamation. The kernel OOM killer is never the limit mechanism.
- **Budget share** — the slice of the memory budget assigned to one subsystem pool at startup. Shares draw from one shared accounting pool; per-subsystem caps that don't share accounting are explicitly rejected (OOM by summation).
- **Segment consolidation** — budget-driven reclamation mechanism: merges sealed segments and garbage-collects tombstones, returning memory to the budget when tombstone/fragmentation ratios cross thresholds. Replaces the vestigial LSM compaction scaffolding in the unified layer. Always paced (see *Paced maintenance*).
- **Paced maintenance** — per [ADR 0038](../adr/0038-embedded-single-file-zoned-rdb.md) §3, the rule that all storage maintenance (consolidation, scrub, append-only compaction, archival) runs as incremental tick-paced work with bounded per-tick cost. Bursty background maintenance threads (the Cassandra compaction model) are prohibited in every profile.
- **Scale contract** — the per-profile promise about dataset size vs memory: embedded contracts working set ≤ memory budget; the server profile carries the disk-residency roadmap (dataset > RAM, ADR 0038 phase 4); serverless contracts strict boundedness. Documented in ADR 0038 §1.

Physical collection scans retain global entity-ID order while cloning only the
requested page and restoring its owned row schemas. The ordering metadata still
scales with entity count; payload memory does not scale with the skipped prefix.
HTTP offsets pass directly to the runtime cursor. Total counts, next offsets and
empty beyond-end pages describe the same physical scan, including historical
versions; this does not introduce logical MVCC filtering or a transaction snapshot.

## Checkpoint reuse

Checkpoints exclude row creators whose transactions are active or aborted and
clear their delete stamps on committed preimages. Live transaction state stays
unchanged. Transaction visibility revisions invalidate reuse after begin, commit
or rollback; equivalent exclusion sets can retain the existing proof. Commit
WAL publication and visibility changes share the checkpoint publication fence.
While store WAL actions are captured for a transaction, row inserts and
upserts stay in the live segment and deferred WAL, leaving published B-tree
pages unchanged. Incidental pager flushes cannot publish unfinished inserts
or delete stamps; a successful commit makes the captured WAL batch durable.
Bulk inserts hold the checkpoint publication fence before installing their
segment rows through WAL capture or append, including transactions that begin
after checkpoint visibility has already been frozen.

Owned row reads restore missing positional column names from the collection's
shared schema before mutation or index backfill. Explicit row schemas and named
fields remain authoritative. This changes neither the stored record format nor
the raw segment revisions used by checkpoint reuse.

Paged checkpoints reuse unchanged collection trees using volatile segment mutation
revisions and manager identity. Active and aborted transaction visibility, pending vault
versions and record format also participate in reuse admission.
Visibility changes outside a collection are checked against borrowed row headers
without serializing its payloads. Each checkpoint shares one frozen excluded-XID
set across collections for both reuse admission and row normalization.
WAL replay and every entity or metadata mutation invalidate the corresponding
generation. New mutation paths must advance the segment revision before exposing mutable data.
When embedded options open an existing paged file, collection definitions remain
in physical metadata. The binary dump's auxiliary contract blob is only used for
an actual embedded artifact; paged checkpoints do not serialize that blob.
Catalog model snapshots reuse one physical metadata observation for their
declared and operational resource lists; index reconciliation retains its native
artifact checks and fallback when persisted index declarations are empty.
Aggregate readiness derives all flags from one health observation. Catalog HTTP
responses share that same health observation with their readiness fields and
reuse their catalog snapshot for health diagnostics. Connection-pool counters
are observed without holding the pool lock through storage inspection.
Cached generations are accepted only after durable catalog publication; current
format roots loaded from disk are seeded before WAL replay. Physical metadata
fingerprints additionally retain their existing statistics seed. The caches do
not change persistent formats or release the checkpoint writer fence. A first
metadata fingerprint or a changed collection still requires work proportional
to that collection; unchanged history is not serialized on every checkpoint.
Fingerprint scans sort compact entity ID/source references and borrow one row at
a time. They preserve the existing hash order and values without materializing
a second collection of payloads; source segments remain pinned for the scan.

Paged loading and WAL replay preserve stored sequence identities. Automatic
inserts encode the segment-assigned identity and fence the complete inline
mutation plus WAL append against checkpoint publication. A completed inline
insert may advance its existing tree-reuse proof only when the before/after
segment revisions show exactly that one mutation. Untracked edits, sealing,
visibility changes and versioned rows retain conservative validation. Durable
replay reconciliation seeds the current raw tree generation before runtime
visibility normalization is attached.

Completed WAL-backed physical delete batches may advance that proof when every
tree removal succeeds and the segment revision delta accounts for exactly the
deleted rows. Earlier untracked edits, changed segment topology, pending vault
versions and format changes retain full checkpoint validation.

Store-managed physical frees remain quarantined outside the allocator until
their removal from the durable collection catalog has been published. Dirty
empty or forwarding nodes remain flushable while an older catalog can still
name them. A trailing optional `RDF1` ledger, owned by `reddb-file`, preserves
detached page IDs across reopen. A later publication forgets eligible IDs
before returning them to the free list; outstanding tree readers defer that
release. Physical delete batches publish this metadata without re-encoding
unchanged survivors, keeping background reclamation paced under grouped WAL.
Legacy files without the extension remain readable. Older readers ignore the
extension, while older writers cannot preserve its pending-reclamation state;
writer downgrades require a separately qualified recovery procedure.

Changed checkpoints encode ordered borrowed rows in batches capped at 1 MiB or
512 records (a single larger value keeps the existing value-size contract).
Comparison with a published tree is streamed; a replacement copies the already
verified prefix only after the first difference. WAL reconciliation uses the
same bounded builder, without cloning a whole collection of payloads.

Replaced and dropped trees enter an optional `RDR1` retirement-root extension
after the paged catalog's cross references, owned by `reddb-file`. The immediately
preceding published generation and older reader-pinned generations remain in
that ledger. A later durable publication removes eligible roots before their
structural and overflow pages enter the free list. Tree handles and cursors pin
retirement independently; reopening restores retirement roots from the ledger.
This recycles physical checkpoint copies, not historical MVCC row versions.
Legacy catalogs without the extension remain readable; older writers ignore it
and therefore cannot preserve automatic retirement across a downgrade. Failed
unpublished allocations remain conservative recovery garbage; this mechanism
does not claim a general orphan-page vacuum of pre-upgrade files.

Free-list trunks consume already-free pages instead of extending the file.
Before a consumed trunk can be overwritten, its old allocator role is removed
durably from both superblock copies. A sync that makes reclaimed pages live
also mirrors allocator ownership, so sibling-superblock recovery cannot hand
out a live page again.
If the consumed-trunk fence fails, its pending state survives in memory and
every allocation retry must complete both durable writes before using the
loaded free pages.

Exact WAL images retained for sparse checkpoint projection have a per-collection
row limit and one shared byte budget. Pressure first evicts images from the
collection responsible, preserving earlier proofs for other collections when
the shared budget permits. Missing images retain full validation; oversized
images do not invalidate unrelated cached proofs.

## Fault model & integrity tooling

- **Storage fault model** — per [ADR 0074](../adr/0074-storage-fault-model.md), the explicit list of modeled physical faults: torn writes, misdirected writes, bit rot, lost writes, and crash-at-any-point. Each zone (superblock, manifest, WAL region, pages/blocks) has a written detection contract; out-of-model faults are documented as such.
- **Scrub** — operator-invocable (and paceable background) integrity pass verifying every checksum in a store and reporting a structured result without mutating anything. RedDB's `PRAGMA integrity_check` counterpart, with fault-class attribution where evidence allows.
- **Salvage** — best-effort extractor that reads a damaged `.rdb` and exports every recoverable entity into a fresh store plus a loss report, skipping and enumerating what fails verification. RedDB's `.recover` counterpart; DST-proven against injected fault classes. Never writes into the damaged file.
- **Paranoid cluster clause** — ADR 0074 §5: cluster/replica profiles never serve data whose checksum failed, run scrub continuously, and treat repair-from-replica as a mandatory profile capability, not an optimization.
- **Test-scoped determinism** — ADR 0074 §6: execution determinism (SimClock, seeded randomness, `buggify!`) is a property of the DST harness, not a runtime guarantee. Replication copies bytes (WAL shipping); deterministic state-machine replication is out of horizon unless a future ADR reverses this.

## Files & layout (operational directory)

- **File/protocol ownership boundary** — per [ADR 0046](../adr/0046-wire-file-crate-authority-boundary.md), the published crate `reddb-io-file` uses Rust import `reddb_file`, and local crate `reddb-file` is the authority for file contracts: names, paths, sidecars, manifests, superblocks, WAL artifacts, checkpoint artifacts, and recovery metadata. The published crate `reddb-io-wire` uses Rust import `reddb_wire`, and local crate `reddb-wire` is the authority for communication contracts: frames, codecs, payloads, topology, connection strings, sanitizers, and replication wire messages. The published crate `reddb-io-server` uses Rust import `reddb_server`, and local crate `reddb-server` orchestrates runtime, SQL, auth, storage engine policy, and calls into those crates; it must not introduce new persistent file formats or protocol payload formats directly.
- **Mutable collection file layout** — operational directory layout where ordinary mutable collections and indexes are stored in stable physical files such as `collection_id.rdb` and `collection_index_id.rdb`, following a WiredTiger/Postgres-like separation of logical objects into independently managed files. The operational layout uses a single physical WAL per node/store with record-level collection, index, range, transaction, and LSN identity rather than one WAL per collection.
- **Stable physical file identity** — operational storage naming rule where physical collection, index, range, and segment files use stable internal IDs rather than human names. Human collection/index names live in manifest/catalog metadata so renames do not move files and filesystem naming edge cases do not affect correctness. Logical IDs may be compact/sequential for catalog and WAL efficiency, while physical `file_id`s use globally unique identifiers to avoid collisions across restore, import/export, migration, and orphan quarantine.
- **Append-only segment layout** — collection storage layout for append-only tables, timeseries, event-shaped data, and similar models where immutable table/segment files plus compaction/retention are a better fit than in-place mutable pages. Native append-shaped models such as timeseries may infer this layout, while ordinary tables must declare append-only intent explicitly. Once closed, append-only segments are immutable; compaction and retention create replacement segments and retire old ones rather than patching closed segments in place. The first contract is append-only in the strict sense: no logical `UPDATE` or `DELETE`; data retirement happens through retention, TTL, or compaction policy.
- **Append-only segment** — immutable closed storage unit containing data, primary index, min/max statistics, checksum coverage, manifest metadata, and optional lookup accelerators such as bloom filters or summaries.
- **Append-only segment chunk** — fixed-size 512 KiB block inside an append-only segment. Segment indexes/manifests point to chunks and store their expected checksums, supporting predictable prefetch, validation, multipart copy, and future fine-grained repair.
- **Append-only segment compression** — initial compression boundary for RedDB storage. Compression is supported first at immutable append-only segment granularity, while mutable collection files avoid page/block compression in the initial design. The default segment codec is zstd, with `none` available for already-compressed or CPU-bound workloads.
- **Operational manifest** — authoritative, versioned, checksummed manifest for operational directory layouts. It maps logical collection, index, range, and append-only segment identities to physical files and records the checkpoint boundary required for backup, recovery, repair, and range movement. The first update strategy is copy-on-write atomic replace: write the next manifest with generation/checksum, fsync it, atomically rename, then fsync the directory.
- **Storage checksum coverage** — minimum corruption-detection boundary for RedDB storage. Mutable pages carry checksums in their page headers; append-only segment blocks/chunks are verified against checksums stored in segment index/manifest metadata; manifests/checkpoint metadata are checksummed as well.

## DDL & recovery

- **Operational DDL create order** — crash-safe create flow for operational collection/index files: create and fsync the physical file first, then publish it in the operational manifest. If a crash happens before manifest publication, recovery treats the unpublished file as an orphan candidate.
- **Operational DDL drop order** — crash-safe drop flow for operational collection/index files: mark the manifest entry `drop_pending`, fsync/atomically publish the manifest, delete or quarantine the physical file, then publish a manifest version that removes the entry. Recovery can distinguish interrupted drops from unrelated orphan files.
- **Operational orphan quarantine** — repair policy for physical files found in an operational directory but absent from the manifest. RedDB quarantines them under `lost+found/` with recovery metadata rather than deleting or reattaching them automatically.

## Backup, restore & WAL

Main-file-only remote backups require an actual self-contained embedded file.
An existing paged file opened with embedded options still requires its physical
metadata sidecars and must use a complete, fenced physical-directory backup.
The runtime rejects incomplete remote snapshots before advertising success.

- **Operational backup boundary** — initial backup contract for operational layouts: create a consistent checkpoint, copy manifest/data/index/segment files covered by that checkpoint, and retain/copy WAL from the checkpoint boundary forward. The first contract targets local and primary-replica stores; coordinated cluster-wide backup is a separate distributed protocol.
- **Operational restore boundary** — initial restore contract for operational layouts: restore a consistent checkpoint, validate manifest and file checksums, then replay retained WAL up to the requested target LSN before opening the store.
- **Operational WAL retention floor** — WAL pruning boundary for operational primary-replica stores. It is the maximum safety requirement across backup/PITR retention and the slowest replica's durable restart LSN, so pruning cannot break point-in-time recovery or force unnecessary replica rebootstrap.
- **Columnar analytics projection** — derived, disposable columnar representation of collection data for analytical reads. The physical WAL and row-oriented collection files remain the sole source of truth (per ADR 0032's derived-projection rule); columnar segments are rebuildable at any time, may be dropped and regenerated, and recovery/backup never depend on them. Decided 2026-07-05: this is explicitly *not* the LTAP-style inversion where columnar files become the durable store and row pages a cache. Derivation happens on the checkpoint/compaction path — the step that already stabilizes data into immutable artifacts also emits the columnar counterpart; the transactional write path never dual-writes columnar, and the un-materialized tail is everything past the last checkpoint boundary. First-encarnation scope is append-only/timeseries collections only (the append-only segment layout), where the un-materialized tail is pure inserts and merging degenerates to concatenation; mutable `table` collections (which would require delete-vector/visibility reconciliation over columnar segments) are a later extension, not part of the initial contract. The physical format is a native RedDB columnar chunk owned by `reddb-file` (ADR 0046), under the same storage checksum coverage and crypto page-envelope authority (ADR 0054) as every other persistent artifact; Parquet/Iceberg interop, if ever offered, is an explicit export feature, never the projection's storage contract. The projection is automatic for every in-scope collection — there is no opt-in list of "columnar-enabled" collections — with a size floor below which materialization is skipped (tiny collections cost more in bookkeeping than they save) and a per-collection opt-out as the escape hatch.
- **LSN-pinned analytical scan** — read protocol for the columnar analytics projection. An analytical read is an ordinary MVCC read: it pins the current snapshot/LSN, scans columnar segments up to the last materialized LSN, and concatenates the un-materialized tail (rows past the last checkpoint boundary) through the normal row read path. There is a single consistency class — analytical reads are never a separate stale tier — and `AS OF` time-travel composes on top by pinning a historical LSN instead of the current one.
- **Store fork** — metadata-level fork of an entire store: a new operational manifest generation pinned to a fork LSN, sharing the parent's immutable artifacts and writing its own private WAL from that point forward. Creation is an O(metadata) operation, never a physical copy of the dataset. Deliberately distinct from the VCS data model's *branch* (#1567 vocabulary): a store fork is storage/deploy mechanics for experiment-and-discard workflows (e.g. testing a risky migration or letting an agent run against production-shaped data), not version-control semantics over collection contents. Mechanically, a fork is a lazy in-place restore-to-LSN riding the operational backup/restore boundary (ADR 0043): immutable artifacts (append-only segments, checkpoint artifacts, columnar projection segments) are shared by manifest reference; mutable collection files are hydrated lazily (replay/CoW) the first time the fork touches them, so creation stays O(metadata) and cost is deferred to what the fork actually writes. Lifecycle is diverge-and-discard: a fork ends by being discarded, kept as an independent store, or promoted to new primary (the recovery/PITR path) — there is no merge-back of divergent stores at the storage layer; merge semantics belong to the VCS data model, which has vocabulary for conflicts.
- **WAL spool** — versioned (v2) write-ahead log records used for replication streaming. Includes magic, version byte, lsn, timestamp, payload-len, payload, crc32. `sync_all` after every append. (The replica-side consumer is the **logical change applier** — see [Primary-replica](primary-replica.md) / [Clustering](clustering.md).)

Row mutation batches and tombstone batches take the publication fence before
changing live segments and retain it through WAL capture or append. Fencing only
COMMIT is insufficient: a transaction may begin after checkpoint visibility was
frozen, and an autocommit's recovery records must not be truncated before its
mutations belong to the published generation.

Sparse paged checkpoints may project a bounded set of changed table rows into
an unpinned live tree after the existing WAL sync fence. A contiguous volatile
segment mutation journal accounts for every changed ID, and each different
row must exactly match a retained successful WAL append. Up to 1,024 deferred
row IDs per collection remember which raw MVCC rows were omitted or normalized
in the published image. Later commits and rollbacks revisit those IDs alongside
new mutations instead of scanning unchanged payloads. A different projected
image still requires an exact retained canonical WAL record. Low-level rollback
of a non-active xid invalidates this reuse proof. Pinned readers, unknown edits,
segment replacement and tracking exhaustion retain the conservative
replacement-tree path. Mutation tracking is bounded by 256
mutations per segment, 1,024 changed IDs and at most 8 MiB of retained records
(a smaller page-cache-derived allowance applies to smaller budgets); its bytes
participate in store memory observations, as do the deferred-ID vectors. A successful publication resets the
retained-record allowance. Exhaustion evicts retained images without disabling
later retention; evicted or oversized images still require conservative validation.
The ordinary inline KV path retains no records.
The catalog publication and WAL truncation fences remain in their existing
order. Versioned table updates encode the canonical segment-assigned sequence
image in their WAL, rather than a pre-insertion caller clone.
