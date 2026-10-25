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

The minimized test fails before the fix; all 38 manager tests and all 400 engine
tests pass afterward (two and three existing ignored tests, respectively).
Updated server regressions cover checkpoint policy, worker shutdown, commit
acknowledgement fencing, native shrink, checkpoint/GC provenance, statement
snapshots and the vacuum snapshot horizon. The locked server library check passes.

## Optimized candidate after relocation fix

These runs use source `0cfb3568b18befe9c45e74c6e440e7332a8374b3` and binary SHA-256
`75c393d3b5811aebb19705c011286b60c2f57ef428cb2c763ed4f54067925e42`.
They are local candidate results, not official released-artifact or production
qualification. The configured engine budget is 1 GiB on a shared host, without
a dedicated cloud cgroup. No explicit checkpoint requests were made.

| Workload | Concurrent reads / failures | Query p99 / max | Writer p99 / max | Checkpoints |
| --- | --- | --- | --- | --- |
| Three bursts of 100 whole-table updates | 913 / 0 | 148.120 / 205.029 ms | 459.651 / 1013.693 ms | 1.708 / 3.850 / 7.291 s |
| 180 updates, paced 0.5 s apart | 776 / 0 | 67.092 / 87.797 ms | 2826.060 / 4225.061 ms | 1.783 / 3.160 / 4.386 s |

Both workloads verify every row, payload and bucket-index result after each
round, then survive SIGKILL and cold reopen with an acknowledged uncheckpointed
write. The continuous workload records three writer operations overlapping
checkpoints; its final marker was verified in the stopped process's WAL before
the crash. Maximum allocated footprints were 510,246,912 and 230,170,624 bytes,
respectively. Readers pass, but the 4.225 s writer maximum remains material.

A separate drain/reuse run continues the burst fixture for three more 100-update
bursts, draining between bursts. Automatic maintenance reduces physical history
to 1,119 entities and logical WAL to 8 bytes. The main file is 472,219,648 bytes
after **each** of the three drained bursts: zero additional main-file growth.
Rows, payloads and index checks pass every round. The reused fixture now contains
payload cycle 600; it no longer represents the earlier cycle-300 crash fixture.
The WAL can retain allocated disk blocks despite its 8-byte logical length.
The main-file plateau is reuse of an existing high-water mark, not physical
shrinking or a general disk-space bound; metadata/resident memory are separate
measurements.

## Remaining gates

The draft PR skipped the actual code CI lane; documentation-shim checks do not
qualify code. Real code CI and a signed candidate container still need validation.
Auth and the larger Platform native copies require actual container qualification
under their resource limits, including writer latency, concurrent reads and crash
recovery, before stable publication and fleet rollout.

ADR 0038 records a proposed transitional exception: this worker schedules full
atomic checkpoints. It does not satisfy incremental per-tick maintenance or
provide a hard storage cap. These finite tests do not establish universal closure.

```bash
cargo test --locked -p reddb-io-engine --lib snapshot_scan_does_not_miss_a_sealed_entity_moving_into_growing
cargo test --locked -p reddb-io-engine --lib
```
