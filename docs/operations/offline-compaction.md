# Offline native compaction

`VACUUM FULL` currently vacuums MVCC history and checkpoints. It does not rewrite
or shrink a native paged file. Use the explicit offline operation to reclaim
historical physical allocation:

```sh
red compact --source /preserved/data.rdb --destination-dir /new/compacted --json
```

The source must be closed and fully checkpointed. The command takes an exclusive
file lock, opens the source without write/recovery privileges, and refuses pending
WAL, checkpoint or double-write recovery. Recover and checkpoint a separate
preserved copy with its compatible engine first when necessary.

The destination directory must not exist. The command rebuilds current collection,
key-index and secondary-index trees, preserving every persisted key/value byte,
including MVCC versions, deletes and record metadata. It preserves cross references,
resume/identity/transaction counters, native summaries, opaque native index blobs,
and encrypted Vault contents. Metadata page addresses and artifact descriptors are
relocated. Retired generations, free pages and allocations unreachable from the
supported durable roots are not copied. This is not a retention-policy change.

The new file is reopened and all tree records compared in order. Native auxiliary
payloads and copied logical/operational files are verified on readback. Output
remains named `.compacting.rdb` until checks and durability barriers succeed. A
failure leaves diagnostic output and the untouched source; use another fresh
output directory for retry. Never start a server on the incomplete filename.
`compaction.json` reports main-file apparent bytes, allocated bytes on Unix, and
verified tree-record counts;
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
- Columnar page owners, tiered/replicated/serverless directories, store forks and
  historical metadata journals require their own relocation qualification and fail
  closed. Standalone operational collection files are copied with their manifest.
- Tree values have a 32 MiB per-value read allowance. Tree batches are bounded by
  512 records/1 MiB plus one value; the target page cache is 16 MiB. Catalog and
  metadata documents are capped at 64 MiB each, and the source ownership bitmap at
  64 MiB (one byte per addressable page). This is not a global process RSS guarantee.

The regression suite is `cargo test -p reddb-io-server --test offline_compaction`.
It covers abandoned allocations before live roots, overflow values and catalogs,
Vault authentication, index/resume reopen and subsequent mutations, pending WAL,
corruption, writer locks, existing destinations and forced process loss.
