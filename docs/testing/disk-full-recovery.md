# Disk-full WAL recovery

Run locally:

```sh
cargo test -p reddb-io-server --test wal_vfs
cargo test -p reddb-io-server --lib disk_full_wal_recovers
```

These tests never fill the host disk. The VFS test limits writable bytes and
returns `StorageFull` after zero bytes or a partial prefix. It exercises both
buffered and larger direct WAL writes. After the first failure, appending,
syncing and truncating must fail, the durable LSN must not advance, and dropping
the writer must leave the WAL unchanged even after capacity returns.

The runtime test uses a test-only, per-writer fault at the real WAL buffer flush.
It runs SQL against a persistent PrimaryReplicaDev database with HASH and BTREE
indexes, in Strict and WalDurableGrouped modes. It checkpoints the baseline,
commits an MVCC update without checkpointing, then fails an INSERT after zero or
17 bytes. It checks reads and rejects another write and checkpoint in the failed
process. After dropping the runtime and exiting the child process, cold recovery
must retain the confirmed update and exclude the incomplete INSERT. Retrying on
the reopened database, checkpointing, checking persisted index completeness and
reopening again must preserve the exact rows.

A failed `write_all` can have written an unknown prefix. The writer is therefore
fenced until close/reopen, even if free space becomes available immediately.
Recovery validates the WAL and handles an ordinary incomplete tail through the
existing recovery path. The operator must restore capacity and reopen before
retrying writes. No storage format changes are introduced.

Reference behavior: [PostgreSQL documents WAL disk exhaustion as a possible
PANIC/shutdown condition](https://www.postgresql.org/docs/16/disk-full.html).
[SQLite reports incomplete writes due to exhaustion as SQLITE_FULL](https://www.sqlite.org/rescode.html#full).
Here the conservative boundary is the failed WAL writer: stop further writes
and require recovery, rather than claiming that restoring space alone resolves
an unknown partial-write position.

This does not establish fault coverage for data-page writes, checkpoint metadata
publication, fsync failures, embedded single-file WAL exhaustion, or all concurrent
writer interleavings. Those need their own injected boundaries. Nor does it imply
that every failed commit is absent: a full write followed by a failed durability
acknowledgement can have an uncertain outcome, which these partial-write cases do
not model.
