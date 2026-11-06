# Offline native compaction

For a physical scrub of the supported closed, checkpointed standalone layout:

```sh
red scrub-paged --source /preserved/data.rdb --json
```

This checks both superblocks, the checkpointed WAL boundary, allocator and
catalog, collection and index trees, supported native metadata and blob
checksums, and a marker-only operational manifest. A tree finding leaves the
auxiliary checks explicitly incomplete. Allocated pages not identified as free
or owned are counted as unclassified: they may be garbage, but prevent a clean
result. When a Vault is present, it authenticates and decodes the Vault using
the configured certificate or passphrase; missing or invalid credentials fail
the scrub. It refuses pending recovery, nonempty logical WAL, slot or audit files, operational
segments, columnar ownership, replicas and other unsupported layouts. It does
not check the logical correspondence between index entries and rows. An error
means the scrub could not verify the full supported layout; it is not a clean
result. Page checksums alone do not prove that a checksum-valid page is the
latest published generation. The source is never written.

For a read-only diagnostic of collection, key-index and secondary-index trees,
close and checkpoint the store, then run:

```sh
red scrub-paged-trees --source /preserved/data.rdb --json
```

This checks the catalog, allocator, tree ownership and forward leaf chains,
key ordering and readable records. The command exits unsuccessfully when it
finds a damaged tree or cannot safely inspect the source. It reports the
affected collection and root, but cannot determine how many records were lost.
Its scope is **trees only**: it does not validate the Vault, native physical
assets, logical correspondence between indexes and rows, or replicated and
serverless state. It is not a complete store scrub or a salvage operation.
It applies the same source restrictions and per-value allowance listed below.

If a closed, checkpointed store has damage limited to secondary index trees,
the following operation copies verified data and surviving trees to a new
directory and discards damaged secondary roots for rebuilding on reopen:

```sh
red salvage-paged-indexes --source /preserved/data.rdb --destination-dir /new/recovered --json
```

`salvage.json` lists the discarded roots and their reasons. Verify the recovered
database and its indexes before serving it; retain the source. Data-tree,
primary-key tree, catalog and pending-WAL corruption are not recovered by
this command. A Vault must authenticate before recovery and again after it is
copied. Nonempty logical WAL, replication slot or audit files, operational segments and nonempty operational collection
files are refused because this operation cannot verify them. The compaction qualification boundaries below
also apply. Recovery also refuses unclassified pages. A failed attempt may leave a diagnostic directory but will not
publish the output file.

For a legacy paged store with no cross references or physical metadata, the
following command copies intact collections and intact leaves of damaged
multi-page collections to a new directory:

```sh
red salvage-paged-collections --source /preserved/data.rdb --destination-dir /new/recovered --json
```

The loss report identifies skipped pages; the number of lost entities remains
unknown. A checksum-damaged single-leaf root leaves that collection empty. For
a multi-page tree, intact parent links, leaf depth, key ordering and surviving
overflow ownership must verify; records in damaged leaves cannot be recovered.
Interior damage, system collection damage, unclassified pages (including
overflow pages whose owner was in a damaged leaf), unauthenticated Vaults,
pending recovery, and more recent catalog formats are refused. Validate the
recovered records and application-level relationships before using them.
The legacy format does not provide a trusted identity high-water mark after
this loss. Both superblocks are fenced to a read-only recovery version before
publication; older engines reject that version. Export verified data into a
new store after reconciling identities against an authoritative source. The
low-level pager can inspect this version read-only; the server and SQL dump
paths have not yet been qualified for it.

For a supported closed store with a pending unified WAL, replay on an isolated
copy and verify the result with:

```sh
red recover-paged-wal --source /preserved/data.rdb --destination-dir /new/recovered --json
```

The source read lock excludes normal writers; the original file and sidecars
remain unchanged. The operation
strictly reads the WAL before making a copy, replays and checkpoints the copy,
then rewrites and scrubs it before publishing the final filename. A private
`.wal-replay` subdirectory retains the copied source and validation artifacts;
do not use its intermediate files as a database.
The current qualification excludes interrupted checkpoints, pending
double-write or non-unified WALs, saved Vaults, physical metadata, WALs larger
than 256 MiB, and runtime-owned vector or probabilistic records. Incomplete or
corrupt WAL frames are refused. This command requires readable data trees; it
does not combine WAL replay with salvage of damaged trees.

`VACUUM FULL` currently vacuums MVCC history and checkpoints. It does not rewrite
or shrink a native paged file. Use the explicit offline operation to reclaim
historical physical allocation:

```sh
red compact --source /preserved/data.rdb --destination-dir /new/compacted --json
```

The source must be closed and fully checkpointed. The command takes a shared
read lock that excludes the writer's exclusive lock, opens the source without
write/recovery privileges, and refuses pending WAL, checkpoint or double-write
recovery. Recover and checkpoint a separate preserved copy with its compatible
engine first when necessary.

The destination directory must not exist. The command rebuilds current collection,
key-index and secondary-index trees, preserving every persisted key/value byte,
including MVCC versions, deletes and record metadata. It preserves cross references,
resume/identity/transaction counters, native summaries, opaque native index blobs,
and encrypted Vault contents. Metadata page addresses and artifact descriptors are
relocated. Retired generations, free pages and allocations unreachable from the
supported durable roots are not copied. This is not a retention-policy change.

Interior ownership and the complete forward leaf chain must agree. Historical
stale backward links from non-rightmost splits are reconstructed in the new file
and counted as `repaired_leaf_backlinks`; forward skips, cycles, overlapping owners,
invalid checksums and unordered records still fail. The source is never repaired
in place. Both directions are verified in the destination.

The new file is reopened and all tree records compared in order. Native auxiliary
payloads and copied logical/operational files are verified on readback. Output
remains named `.compacting.rdb` until checks and durability barriers succeed. A
failure leaves diagnostic output and the untouched source; use another fresh
output directory for retry. Never start a server on the incomplete filename.
`compaction.json` reports main-file apparent bytes, allocated bytes on Unix, and
verified tree-record counts and reconstructed historical leaf backlinks;
record counts include derived indexes and are not SQL row counts. Measure allocated
bytes separately (for example, `du -B1`) and account for retained recovery copies.

This command does not switch the running database, delete backups, reduce PVC
capacity or compact in the request path. Before a serving replacement, independently
validate SQL/index results, authentication, reopen, subsequent writes/checkpoint and
crash recovery using the target engine. Keep the source family as rollback evidence.

Current qualification boundaries:

- Native paged record formats 11/12 and canonical resume trailers 4/5. Embedded
  snapshots, encrypted pagers, unknown catalog extensions and pending recovery fail.
  Encrypted Vault pages are preserved without needing to disclose their key.
- Columnar page owners, non-empty tiered layouts, replicated/serverless directories,
  store forks and
  historical metadata journals require their own relocation qualification and fail
  closed. Empty standalone support-directory scaffolding is preserved. Standalone
  operational collection files are copied with their manifest.
- Tree values have a 32 MiB per-value read allowance. Tree batches are bounded by
  512 records/1 MiB plus one value; the target page cache is 16 MiB. Catalog and
  metadata documents are capped at 64 MiB each, and the source ownership bitmap at
  64 MiB (one byte per addressable page). This is not a global process RSS guarantee.

The regression suite is `cargo test -p reddb-io-server --test offline_compaction`.
It covers abandoned allocations before live roots, overflow values and catalogs,
Vault authentication, index/resume reopen and subsequent mutations, pending WAL,
corruption, writer locks, existing destinations and forced process loss.
