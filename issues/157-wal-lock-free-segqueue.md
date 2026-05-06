# perf(wal): lock-free append via SegQueue + leader-flush (Roadmap #2) [AFK]

GitHub: reddb-io/reddb#157
Parent: #152

Replace `Mutex<WalWriter>` with `crossbeam::SegQueue<(seq, Vec<u8>)>`. Atomic `next_seq` for ordering. Single leader (first thread calling `drive_flush`) drains queue in LSN order, fsyncs, publishes `durable_lsn`. Waiters atomic-load + park (`parking_lot`). Recovery format unchanged.

## Acceptance Criteria

- [ ] WAL append uses SegQueue + atomic `next_seq`; no mutex on hot path.
- [ ] Leader-flush coordinator drains in LSN order; publishes `durable_lsn` atomically.
- [ ] Waiters use `parking_lot` park/unpark.
- [ ] Recovery format unchanged; recovery tests pass.
- [ ] Crash-injection tests cover writer/leader/waiter crashes.
- [ ] Fuzzing target: no LSN gaps under concurrent writers.
- [ ] Bench: `concurrent` and `insert_sequential` improve under canonical config (#154).
