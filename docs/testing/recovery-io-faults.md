# A storage failure during recovery

Run the functional recovery journeys locally:

```console
cargo test -p reddb-io-server --lib interrupted_dwb_recovery -j 1
cargo test -p reddb-io-server --lib overflow_checkpoint_undo_survives -j 1
```

The DWB journey commits an MVCC update that changes an indexed key and an insert, then leaves a
complete durable DWB frame by failing a partial write of the checkpoint catalog's home page and
exiting without shutdown. A second process fails during reopen at each of these boundaries:

| Recovery boundary | Injected failure | Evidence required after process death |
| --- | --- | --- |
| Writing the catalog's home page | `StorageFull` before any bytes or after 17 bytes | DWB frame and WAL unchanged |
| Syncing the home pages | I/O error before `fsync` | DWB frame and WAL unchanged |
| Clearing the DWB | `StorageFull` before any bytes or after 17 bytes | WAL unchanged; home pages already synced |
| Syncing the cleared DWB | I/O error before `fsync` | WAL unchanged; home pages already synced |

Home-write and home-sync failures repeat twice before healthy recovery. A partial DWB clear is
allowed to invalidate the consumed frame only because the home-page barrier already succeeded.
Every subprocess has a 60-second deadline, including the final recovery and new-write phase.

The final process checks exact field values from an independent expected history through cold
HASH lookups, BTREE range reads, and a full scan. It checks that the old indexed MVCC key stays
absent, rejects a duplicate UNIQUE key, accepts a new insert, checkpoints, verifies the persisted
indexes, and reopens again. No host disk is filled.

The checkpoint-undo journey additionally interrupts restoration of WAL before-images with empty
and partial writes and a sync failure, checks that the complete WAL remains unchanged, then
interrupts recovery twice more and verifies every large row through three recovery/persist cycles.

These are native process-death tests with the OS page cache retained. They supplement the
power-loss models in SimVfs; they do not emulate a dishonest storage device or a machine power cut.

The approach follows [SQLite's compound-failure tests](https://sqlite.org/testing.html#compound_failure_tests),
[MySQL's doublewrite recovery tests](https://github.com/mysql/mysql-server/blob/trunk/mysql-test/suite/innodb/t/doublewrite.test),
and [TigerBeetle's explicit progress checks](https://tigerbeetle.com/blog/2023-07-06-simulation-testing-for-liveness/).
