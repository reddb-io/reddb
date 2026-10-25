# Local paged WAL and concurrent read qualification, 2026-10-25

The synthetic table has 1,000 rows, an integer primary key, a bucket index and
512-byte payloads. Whole-table updates change only payload text. Every read must
retain count 1,000 and ID sum 499,500. These are shared-host diagnostics, not an
independent performance study or qualification of deployed production stores.

Released 1.26.8 retained 131,931,435 bytes of recovery WAL after 100 updates and
65 seconds idle. An explicit checkpoint drained logical WAL length to its 8-byte
header in 770 ms. Reserved disk blocks remain a separate measurement.

The proposed automatic worker on source `78aec6f25` drained WAL in three rounds.
The debug run was rejected: its third round returned HTTP 503 during a 33-second
checkpoint; earlier checkpoint times were 10.8, 23.4 and 23.8 seconds. The optimized
run measured 2.055, 4.690 and 7.114 seconds but failed its concurrent-read invariant.
A second instrumented optimized run captured count 89 and ID sum 46,616. Every
post-round full-row and bucket-index check still found all 1,000 rows. Crash/reopen
qualification was therefore not attempted. The optimized fixture used a separate
partition on the same host, so these numbers are not a paired speedup claim.

Released 1.26.8 also failed a periodic-checkpoint HTTP control with ten incomplete
concurrent reads, including count 999. Negative controls observed no mismatch in
3,280 queries without checkpoints or 6,268 queries with faster synchronous
checkpoint boundaries. Those finite negative samples do not exclude the race.
The released manager source and the failing candidate manager were byte-identical
(SHA-256 `bf5d60612e7aa6ef031066372c3f0908a29945d6aecbc8323e063809ac2c5e55`).

The minimized regression pauses a real snapshot scan after its growing row,
then attempts the actual sealed-to-growing relocation. The old path removes the
sealed preimage before waiting for growing's lock: the scan returns `[2]` rather
than `[1, 2]`. It fails in milliseconds without the HTTP transport, WAL pressure,
large datasets or automatic checkpoint worker.

The fix retains the growing topology read gate across sealed probes and publishes
relocation under its exclusive gate. It pins sealed membership against consolidation
and installs the preserved destination before removing its source. It changes no
stored format or WAL ordering. Bounded consumers remain outside storage guards.

Validation so far: the minimized test fails before the fix; all 38 manager tests
and all 400 engine tests pass afterward (two and three existing ignored tests,
respectively). Full server regressions, the optimized HTTP workload, crash/reopen,
large-store writer latency and released-artifact qualification remain required.

```bash
cargo test --locked -p reddb-io-engine --lib snapshot_scan_does_not_miss_a_sealed_entity_moving_into_growing
cargo test --locked -p reddb-io-engine --lib
```
