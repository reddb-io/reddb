# Queue policy lookup and singleton ACK — 2026-11-07

Delivery and ACK resolved one queue's lifecycle policy by building a full
catalog snapshot. That snapshot observes collection payloads and scans resident
queue metadata. Batch ACK built it twice, including once for a lifecycle object
it immediately discarded. This made queue operations depend on unrelated data.

The fix uses the existing indexed queue configuration loader, which resolves
the same persisted policy row without a full catalog snapshot. It does not
cache policy: ALTER remains visible on the next operation. Batch ACK constructs
only the store/transaction it needs, and singleton ACK skips duplicate-set
allocation. No format, sidecar, fsync policy, publication fence or transaction
contract changes.

## Deterministic regressions

The catalog observation guard records actual entity visits in test builds.
Before the fix, a READ and individual ACK visited 354 catalog entities in a
fixture with 64 unrelated table rows. The test requires zero such visits for
both individual and batch ACK. It failed on the old implementation and passed
after the fix, without a wall-clock threshold.

The cold-boot policy regression also failed before the fix: a FANOUT queue
resolved to the default WORK policy. The old snapshot's resident observation
could miss the disk-backed configuration. The new test checks FANOUT, delivery
deadline and DLQ, changes all three via ALTER, verifies immediate visibility
and repeats checkpoints/reopens. It passes with the indexed loader.

## Measurement design

The baseline is `cfc34458` (PR #2731), using the already verified release binary
whose Rust sources match `a1f12745`. The candidate runtime is `5705a5a2`.
Both use the same Linux builder, Rust 1.95.0 and release flags. Source and binary
hashes are recorded alongside the captures. The builder is stopped before
timing; each capture uses fresh server volumes.

The singleton probe compares both ACK syntaxes on both binaries. Each cell has
three rounds of 256 complete publish/READ/ACK cycles with 100-byte payloads.
ACK syntax order alternates between rounds; queue names and prior workload are
the same for both binaries. Individual ACK and batch ACK with one ID remain
distinct samples. The harness records publication, READ and ACK network
durations separately. These durations include transport, server execution,
locking and durability waits; they are not CPU or fsync profiles.

The final comparison repeats batches 1/16 against RabbitMQ 4.1.4 and Kafka 4.1.0,
with three rounds per cell and publication/post-ACK SIGKILL/restart checks.
Both baseline and candidate use batch ACK, isolating the runtime change. Both RedDB
servers use WalDurableGrouped, a 200 microsecond window and bounded residency.
Servers have four CPUs/2 GiB and the client has two CPUs/1 GiB.

[Singleton capture](../../bench/results/queue-ack-fast-path-probe-2026-11-07/):

| Runtime | ACK syntax | Cycles/s | Cycle p99 ms | Publish median ms | READ median ms | ACK median ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| before | single | 63.1 | 22.48 | 3.75 | 5.67 | 5.61 |
| before | batch | 49.5 | 50.09 | 4.00 | 5.94 | 8.04 |
| after | single | 86.7 | 14.73 | 3.78 | 3.60 | 3.55 |
| after | batch | 87.4 | 17.67 | 3.76 | 3.53 | 3.49 |

Throughput and p99 are medians across three rounds. Phase columns are medians
of the per-round phase medians; they need not sum to the complete-cycle median.

Individual ACK throughput rises 37.4%; one-message batch ACK rises 76.5%.
After the change, the two syntaxes have similar median throughput in this
capture (86.7/87.4 cycles/s), although their p99s still differ. Publication
medians remain near 3.8 ms; the reduction is concentrated in READ and ACK.
This localizes the observed improvement to the changed paths without claiming
that all remaining latency is fsync or that the tail-latency SLO is established.


[Matching-API matrix](../../bench/results/queue-ack-fast-path-v2-2026-11-07/),
medians of three rounds:

| Engine | Batch | Message cycles/s | Whole-batch p99 ms |
| --- | ---: | ---: | ---: |
| native-before | 1 | 57.5 | 25.75 |
| native-before | 16 | 379.1 | 47.46 |
| native-after | 1 | 87.5 | 18.58 |
| native-after | 16 | 464.0 | 39.94 |
| rabbit | 1 | 150.6 | 11.15 |
| rabbit | 16 | 1399.9 | 13.15 |
| kafka | 1 | 61.2 | 36.94 |
| kafka | 16 | 183.8 | 682.72 |

Batch 1 throughput improves 52.1% against the same ACK API. Batch 16 throughput improves 22.4% against the same ACK API. RabbitMQ still leads at batch 16. Kafka varies substantially between captures;
this run does not support a stable cross-product ranking.


The earlier [mixed-API capture](../../bench/results/queue-ack-fast-path-2026-11-07/)
uses individual ACKs on the baseline and batch ACK on the candidate. It is
retained as a separate client-migration scenario; its batch-16 ratio must not be
attributed solely to this patch, since the baseline already supports batch ACK.
The final matrix uses `--baseline-red-ack-batch --red-ack-batch` with fresh
containers and volumes to hold that choice constant.

Kafka batch-16 rounds in the earlier capture were 428.0, 685.7 and 671.0
cycles/s; the matching-API capture measured 197.0, 140.0 and 183.8. Its median
p99 increased from 50.26 to 682.72 ms. These samples are retained without
filtering; the source of the variability has not been established. Do not infer
Kafka capacity or a RedDB leadership claim from this host/run. Native
before/after improvement appears in both captures and the isolated ACK probe.

These are short shared-host diagnostics. Whole-batch p99 is never divided by
batch size. RabbitMQ confirms persistent publications to a single-node quorum
queue, but its consumer ACK is one-way, followed here by an ordered RPC; that
does not establish ACK fsync. Kafka uses one broker/RF1, idempotence, `acks=all`,
`linger.ms=0`, `flush.messages=1` and synchronous offset commits, and retains
records. The comparison does not establish equivalent retirement, HA or
power-loss behavior. The two-second topology settle precedes payload
publication; no extra delay is inserted after confirmation before SIGKILL.

## Verification

Both matrices passed all 24 timed rounds, eight publication-recovery checks
and eight post-ACK restart checks. The singleton probe passed all 12 rounds.

Linux `cargo check` and `cargo fmt --all -- --check` passed. Targeted tests:
22 PrimaryQueueStore, 55 QueueLifecycle, five queue policy descriptor and all
nine `no_metadata_sidecars` integration tests. The Python harness passed 26
tests. Recovery covers Strict/WalDurableGrouped, bounded/unbounded storage,
process exit, repeated cold boots/checkpoints and the 256/257-message boundary.
The database state still uses only `.rdb`/WAL; independent audit logs retain
their existing exception.

The [first full CI run](https://github.com/reddb-io/reddb/actions/runs/42422761876)
exhausted the unit job's 20-minute budget: compilation took 8m17s, and 6,751
unit tests had passed before cancellation during the remaining crash matrices.
No individual test failure was recorded; the other jobs passed. This is not
counted as a complete successful run. The workflow and per-test limits remain
unchanged; the [full rerun](https://github.com/reddb-io/reddb/actions/runs/42424492915)
passed on `f7405adc3`: all unit tests, four integration shards, doctests,
persistent multimodel tests, quality checks and driver/RQL conformance. The
subsequent commits only document results; runtime Rust/Cargo inputs and the
workflow are identical to the successful run.

## Reproduction

Create fresh fixtures from the recorded manifests and install the recorded
binaries, then run the matching-API matrix:

```sh
python bench/kv/run_batch_matrix.py \
  --prefix reddb-ackfast-v2 --suffix 1107 \
  --out bench/results/queue-ack-fast-path-v2-2026-11-07 \
  --queues-only --queue-baseline --red-ack-batch --baseline-red-ack-batch \
  --queue-settle-ms 2000 --operations 256
```

Use a new output directory and fresh volumes for each repeat. The singleton
probe manifest records every exact command and its alternating ACK-mode order.
See [`bench/kv/BATCHING.md`](../../bench/kv/BATCHING.md) for fixture creation and
durability settings.

## Next concurrency boundary

Queue publication grouping between requests remains separate work. The current
row-publication coordinator intentionally bypasses aggregation when the caller
already holds the publication lock. Delivery and ACK hold that lock while
selecting/validating pending state. Simply releasing it before publication
would introduce races between reservations, ACK, retry and checkpoint. The
next change must validate and prepare those transitions inside the coordinator,
with competing-consumer and failed-group recovery tests, before sharing a
durability barrier across requests.
