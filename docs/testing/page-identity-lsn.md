# Page identity and durable LSN authority

Run locally:

```console
make test-page-identity
```

The functional runtime tests checkpoint two tables, a UNIQUE HASH index and a BTREE index. They
replace a collection's leaf with another table's complete leaf, or stamp `u64::MAX` as the page
LSN and recompute its checksum. Previously a cold SQL scan served the foreign table's values,
or accepted the impossible LSN. Both faults must now fail with a named integrity error while
preserving the original data file and WAL bytes.

Checksummed reads compare the embedded page ID with the requested address before caching bytes.
Ordinary page writes check identity before capturing undo or changing the cache. The encrypted
host checks plaintext identity after authentication and before encryption; ciphertext is never
interpreted as a plaintext page header.

For nonzero LSNs, the shared pager asks its host for durable authority. The default host accepts
only LSNs at or below the completed physical checkpoint. The native host can also use the
attached WAL writer's durable boundary. This check does not flush the WAL or advance a boundary.
A cold page beyond the checkpoint without an available WAL authority is refused. The existing
WAL-first regression additionally reopens a real stamped page with its durable WAL attached and
requires the read to succeed. LSN-zero pages retain the existing DWB contract.

The positive recovery journey stages a complete current DWB image before damaging its home page.
Cold reopen must replay that trusted image, return exact HASH/BTREE/scan results, verify persisted
indexes, checkpoint, and survive another cold reopen. Without that recovery evidence, the
authoritative page is refused rather than recreated.

## Remaining generation-proof gap

Identity and LSN validation do not detect a previous valid image of the same reused physical page
when its LSN is zero. A functional counterexample reproduces an older value after a confirmed
MVCC update: checkpoint until the allocator reuses a recorded collection-page address, replace
the current page with that address's old checksummed image, then read at cold boot.

The current resume catalog stores root addresses, counts and index definitions; it does not seal
every expected page image independently. A stamp read from the suspect page itself is not an
independent generation proof. This case remains unfinished under scenario 2, with its rejecting
regression retained on the follow-up branch. Resolving it requires generation evidence anchored
outside the suspect page, including interior/leaf and overflow ownership; an extra local CRC
comparison alone must not be presented as complete lost-write detection.

Reference fault patterns: [MariaDB's future-LSN test recalculates the page CRC](https://github.com/MariaDB/server/blob/11.8/mysql-test/suite/innodb/t/corrupted_during_recovery.test).
[PostgreSQL's amcheck documentation](https://www.postgresql.org/docs/current/amcheck.html) distinguishes
logical corruption from physical checksum failures. This qualification follows the explicit
misdirected/lost-write fault model in [ADR 0074](../../.red/adr/0074-storage-fault-model.md).

Resource sketch: identity adds one integer comparison after a disk read; nonzero LSNs additionally
consult a durable boundary. Neither path scans the database or allocates a page inventory.
