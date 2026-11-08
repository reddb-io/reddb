# Concurrent queue delivery and ACK publication

The native paginated store now plans compatible autocommit READ and ACK requests
under one publication fence and commits their rows together. In this capture,
the median complete-cycle rate at a 16-message window increased from **179.1 to
268.5 messages/s** (+49.9%). Median per-round whole-window p99 decreased from
**97.60 to 69.07 ms** (-29.2%). RabbitMQ and Kafka still handled this window faster.

This is a shared-host diagnostic, not a production capacity or HA comparison.
The capture started on November 7 and finished on November 8, UTC-3. All samples,
recovery results, resource observations, image identities and binary hashes are
in [the capture directory](../../bench/results/queue-cross-request-2026-11-07/).

## What changed

Previously, READ held the queue read lock through durable reservation, and ACK
held the publication lock before entering the batch coordinator. Concurrent
requests therefore paid for separate publications. The coordinator can now run
read-only preparation under its publication fence. A READ excludes messages
reserved by earlier accepted plans; an ACK rejects overlap before contributing
rows. Results are released after durable publication. The existing preimage
rollback restores every accepted request when that publication fails.

Both the statement limit and the existing row-byte accounting bound a group.
An oversized request publishes alone. This changes neither the `.rdb`/WAL format
nor the recovery boundary and creates no metadata sidecars.

The READ fast path covers ordinary unkeyed autocommit requests with counts 1–256.
Explicit transactions, RLS-enabled queues, ordered keys, expiry/reclaim,
exhausted attempts and larger READ counts retain the lifecycle path. That path
also holds the publication fence across selection and reservation so it cannot
race a prepared READ. WORK and FANOUT retain their existing retirement rules.
The request's captured MVCC snapshot is installed during READ preparation;
preparation does not borrow the coordinator thread's ambient snapshot.

Cost sketch: N compatible requests can replace N durable publications with one,
while each response still waits for the WAL. The configured 200 µs collection
window is a latency/throughput tradeoff; singleton improvement is not claimed.

## Workload and provenance

- Baseline: main `dcf7510babf5cd9b33e0aeaa1864d26e315b9348`, using the optimized
  binary from the [preceding indexed-policy capture](queue-ack-fast-path-2026-11-07.md).
- Candidate runtime: `b4d4562e0950b51d128b4721601c962f41c3c32e`.
- Capture/harness revision: `fc0153acaa73a689f8f5053c1f33435a43abbb75`; its later
  commits change benchmark/test coverage, not the runtime implementation.
- Before SHA-256: `05de552c37f72192b6e37f6842672cade532fd7a92facc8f322a1c58662d550f`.
- After SHA-256: `f74d7ec780cd16bdb99b3d746057d5f5e1cc2565104c2d87abffe4251fcb393e`.
- Three rounds per cell, 256 unique 100-byte messages per round, complete
  publication → delivery → ACK cycles. Rates below are medians of the three
  round rates; p99 values are medians of the three per-round whole-window p99s.
- Both RedDB versions use identical APIs. Window 16 sends eight concurrent
  `READ COUNT 2` requests, then eight concurrent `ACK BATCH` requests, each for
  that READ's two messages. Window 1 uses one READ and one singleton ACK BATCH.
- RabbitMQ and Kafka keep their existing adapters: one push consumer with
  window-sized prefetch, or one polling consumer with synchronous offset commit.
  They are not eight independent Kafka consumer processes.
- Four server CPUs, 2 GiB container memory; two client CPUs, 1 GiB. RedDB engine
  memory budget is 1 GiB, with grouped WAL durability and a 200 µs window.
  Fresh volumes were used. The build container and other benchmark servers were
  stopped during timing. The manifest records the exact configuration.

Reproduction after creating matching isolated fixtures:

```sh
python bench/kv/run_batch_matrix.py --prefix reddb-cross --suffix 1107 \
  --out bench/results/queue-cross-request-2026-11-07 \
  --queues-only --queue-baseline --red-ack-batch --baseline-red-ack-batch \
  --red-read-concurrency 8 --queue-settle-ms 2000 --operations 256
```

Use a fresh output directory, containers and volumes for another capture.

## Results

| Engine | Window | Messages/s | Whole-window p99, ms |
| --- | ---: | ---: | ---: |
| RedDB before | 1 | 78.6 | 17.80 |
| RedDB after | 1 | 80.4 | 16.99 |
| RabbitMQ 4.1.4 | 1 | 146.7 | 9.41 |
| Kafka 4.1.0 | 1 | 56.2 | 46.12 |
| RedDB before | 16 | 179.1 | 97.60 |
| RedDB after | 16 | 268.5 | 69.07 |
| RabbitMQ 4.1.4 | 16 | 1364.1 | 14.32 |
| Kafka 4.1.0 | 16 | 795.9 | 39.37 |

RedDB window-16 round rates were **178.7 / 179.1 / 207.1** before and
**214.6 / 271.5 / 268.5** after. Singleton ranges overlap: **76.4–93.7** before
and **76.3–81.3** after. The small singleton median difference is not evidence
of an isolated-request speedup.

Median RedDB phase times over all timed samples:

| Phase | Window 1 before / after, ms | Window 16 before / after, ms |
| --- | ---: | ---: |
| Publication | 3.95 / 3.93 | 24.55 / 25.20 |
| READ | 3.80 / 4.10 | 28.85 / 17.22 |
| ACK | 3.71 / 4.00 | 29.15 / 18.89 |

The concurrent gain is in delivery and ACK. Publication remains essentially
unchanged in this capture. Singleton READ/ACK phase medians rose slightly;
these timings include transport, locking and durability, not just engine CPU.
At window 16, RabbitMQ remains about 5.1× and Kafka about 3.0× the candidate's
complete-cycle rate. This work does not close that gap.

Protocol contracts differ: RedDB returns a durable ACK response; RabbitMQ uses
one-way consumer ACK plus an ordered RPC; Kafka returns a synchronous
offset-commit response and retains records. These are the single-node profiles described in
[BATCHING.md](../../bench/kv/BATCHING.md), not equivalent HA guarantees. Kafka
was more variable across previous captures, so no cross-run leadership claim
is made. Each window-16 round has only 16 latency samples; its p99 is a diagnostic
maximum, not a tail-latency SLO. Resource samples include setup and recovery and
do not establish that the workload is free of client-side bottlenecks.

## Recovery and automated verification

All 24 benchmark recovery stage artifacts passed: for each engine and window,
32 confirmed publications survived SIGKILL/restart, and confirmed consumption
survived a second SIGKILL/restart with an empty queue/committed Kafka progress.
The two-second topology-settle delay occurs before publication. There is no
added wait between the last confirmation and SIGKILL. This verifies process
crashes, not sudden power loss.

Linux validation:

- `cargo check -p reddb-io-server`;
- `cargo clippy -p reddb-io-server --lib -- -D warnings`;
- `cargo fmt --all -- --check`;
- 182 queue-related unit tests and six row-publication tests;
- nine `no_metadata_sidecars` integration tests and five queue-policy tests;
- an additional successful run of the process-exit queue test after extending
  its paginated grouped-mode child to concurrent READ and ACK requests;
- 28 Python benchmark-harness tests.

New assertions cover exclusive ownership between eight concurrent consumers,
one winner for duplicate ACKs without aborting unrelated ACKs, fewer durable
publications than requests, failed-group rollback, invalid-preparation
isolation, oversized-request separation, WAL recovery and repeated checkpoints
and cold boots. The process-exit test also verifies that only allowed database
state files exist after recovery.

Standard CI: [run 42440163312](https://github.com/reddb-io/reddb/actions/runs/42440163312)
validates runtime revision `b4d4562e0`; completion is recorded before merge.
