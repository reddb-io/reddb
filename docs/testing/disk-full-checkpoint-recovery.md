# Disk-full checkpoint recovery

Run the focused recovery journey locally:

```console
cargo test -p reddb-io-server --lib disk_full_checkpoint_recovers
```

The test does not fill the host disk. A child process injects `StorageFull` into the native pager
before writing anything or after writing a 17-byte prefix. The matrix targets both an ordinary
protected page write and the physical page-1 catalog write.

Before the failure, the child commits an MVCC update and insert to the WAL. It then verifies that
the failed checkpoint fences both checkpoint retries and new mutations until reopen, while reads
of the already-committed state remain available.

The parent reopens the database and queries the HASH and BTREE indexes before an authoritative
scan can warm them. It verifies the exact recovered rows, checkpoints after storage becomes
available, checks the persisted indexes, and reopens once more to prove the recovered catalog is
stable.

The durable publication point is intentionally not inferred from the injected error: the reopen
may select the previous generation plus WAL or complete a valid double-write-buffer recovery.
Both paths must converge on the same committed logical state without admitting the rejected row.
