# Index integrity and recovery

Run the functional fault scenarios locally:

```sh
cargo test -p reddb-io-server --test index_integrity_recovery
```

The suite injects checksum damage and removes a real persisted posting while
keeping its page checksum valid. It checks HASH and BTREE indexes after a
committed MVCC update and delete, then verifies all surviving field values and
the absence of deleted rows across two cold reopens after repair.
It also removes a whole derived tree while retaining the SQL definition and
requires cold boot to rebuild it, followed by a clean logical verification.

## Physical damage

For a closed, checkpointed paged database, `storage::compact::scrub_paged`
identifies damaged trees. `salvage_paged_indexes` writes a new database directory,
preserves the original bytes and retains index definitions for reconstruction.
It refuses collection-data corruption: rebuilding an index cannot recover a
damaged authoritative row. Do not replace the original until the recovered
database has been verified. Pending WAL recovery is a separate operation.

## Missing postings with valid checksums

A physical scrub cannot prove index completeness. The embedded runtime API
`runtime.verify_persisted_indexes("records")` compares the registered HASH/BTREE
checkpoint projections against their source generation and returns findings
with index names and reasons. It does not repair or mutate the database. It
also checks BTREE equality side indexes. Other index methods are rejected.

The pass examines persisted generations, not uncheckpointed WAL changes or
resident overlays. It uses bounded per-row workspace but can take
O(rows × indexes × log(postings)) time and holds the publication lock while
verifying an index; schedule it as an explicit maintenance operation. It does
not replace the physical scrub or certify all database structures.

After preserving the source and verifying the authoritative rows, the regression
rebuilds a non-unique index with `DROP INDEX by_org ON records` followed by its
original `CREATE INDEX` definition, checkpoints, and verifies again. Quiesce
writers for this operator workflow, particularly for UNIQUE indexes: DROP and
CREATE are separate operations, not an atomic repair. Keep the original index
definition, including uniqueness and column order. Checksum-damaged trees use
the isolated offline salvage workflow above.
