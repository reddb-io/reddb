# Queue delivery and ACK batching — 2026-11-07

Queue `READ COUNT N` now groups unkeyed reservations and attempt counters into
publications of at most 256 messages. `QUEUE ACK jobs BATCH GROUP workers
('id1', 'id2')` retires up to 256 messages in one autocommit publication. The
existing publication fence, WAL records, rollback path and durability settings
remain in use. No metadata file or storage-format migration is introduced.

Ordering-key reservations remain sequential. Retry limits, DLQ handling and
FANOUT group isolation retain their existing behavior. All batch ACK handles
are validated before retirement. Explicit transactions retain their existing
commit/rollback behavior; a rolled-back ACK makes messages available for
redelivery rather than restoring the original pending reservation. A large
READ can span multiple publications and is not one atomic operation.

## Measured workload

The final capture is
[`queue-delivery-batch-v2-2026-11-07`](../../bench/results/queue-delivery-batch-v2-2026-11-07/).
It compares v1.28.28 (`59b3da89`) with runtime commit `a1f12745` over HTTP,
RabbitMQ 4.1.4 and Kafka 4.1.0. Both RedDB binaries were compiled from verified
source with the same Linux builder, Rust 1.95.0 and release flags. The manifest
and build-provenance files record image IDs, binary hashes and configuration.

Each engine uses fresh volumes, batches of 1 and 16, three rounds of 256
messages and 100-byte payloads. The native baseline uses concurrent individual
ACK requests; the candidate uses batch ACK. Neither native fixture has a prior
KV workload. Only the measured server and client run during each engine's
capture; the build container is synced and stopped before timing. Server limits
are four CPUs and 2 GiB, with a separate two-CPU, 1 GiB client allocation.

The reported throughput counts complete publish/deliver/ACK message cycles.
Latency covers the whole batch, never latency divided by the batch size.
Values below are medians of three rounds, including the median of their p99s.
These small shared-host diagnostics are not capacity limits or production SLOs.
Resource samples cover setup and recovery too; they do not establish that the
client is unsaturated.

| Engine | Batch | Message cycles/s | Whole-batch p99 (ms) |
| --- | ---: | ---: | ---: |
| native-before | 1 | 63.4 | 23.12 |
| native-before | 16 | 99.9 | 170.49 |
| native-after | 1 | 56.9 | 26.65 |
| native-after | 16 | 374.5 | 50.33 |
| rabbit | 1 | 147.0 | 9.68 |
| rabbit | 16 | 1316.5 | 14.82 |
| kafka | 1 | 60.7 | 38.33 |
| kafka | 16 | 803.7 | 42.77 |

Batch 16 improves 3.75x against the baseline. Batch 1 is 10.2% slower;
the final lookup optimization does not eliminate that regression. RabbitMQ and
Kafka remain faster in this workload. Batch ACK should not be presented as a
single-message performance improvement.


## Durability and comparison limits

RedDB uses `wal_durable_grouped`, a 200 microsecond group-commit window and
bounded disk residency. Strict and WalDurableGrouped wait for the existing
durability barrier before reporting delivery/ACK success; other modes keep
their configured guarantees. The implementation does not weaken fsync policy.

RabbitMQ uses a quorum queue, persistent messages and publisher confirms. Its
consumer ACK is one-way; the harness follows it with an ordered RPC, which does
not prove ACK fsync. Kafka uses one broker, replication factor one, `acks=all`,
idempotence, `linger.ms=0`, `flush.messages=1` and synchronous offset commits;
records remain retained after consumption. These are different contracts, so
throughput ratios do not imply equivalent durable retirement or availability.

Every engine/batch combination also publishes 32 confirmed messages, receives
SIGKILL, restarts and verifies all payloads. After consuming and acknowledging
those messages it receives SIGKILL again and verifies there is no redelivery.
The fixed two-second topology settle occurs before payload publication; there
is no extra delay between confirmation and the kill. This checks process-crash
recovery, not power loss or multi-node failover.

Both completed captures passed all 24 timed rounds, eight publication-recovery
checks and eight post-ACK restart checks. The raw results include every round;
`summary.json` in the final capture contains the table aggregation.

## Earlier captures

The directory named
[`queue-delivery-batch-final-2026-11-07`](../../bench/results/queue-delivery-batch-final-2026-11-07/)
is the first complete capture, before the final ordering-key lookup optimization.
Its original name is retained for traceability. At batch 16, native throughput
rose from 95.3 to 378.2 message cycles/s (3.97x); batch 1 fell from 62.6 to 55.7
(11%). Inspection found a redundant ordering-key lookup in batch formation.
Commit `a1f12745` removes that duplicate read and the final capture repeats the
entire comparison with fresh volumes. Both captures are retained, including
their outliers and recovery results.

The initial
[`queue-delivery-batch-2026-11-07`](../../bench/results/queue-delivery-batch-2026-11-07/)
contains a setup failure, not a measured sample: the published GNU baseline
binary required newer glibc symbols than the Debian bookworm fixture supplied.
The completed captures instead build the exact baseline source in the same
environment as the candidate. No performance or data-loss conclusion is drawn
from that loader failure.

## Regression verification

Final runtime source passed Linux `cargo check`, 20 PrimaryQueueStore tests,
55 QueueLifecycle tests and all nine `no_metadata_sidecars` integration tests.
Parser boundaries and the Python benchmark harness (26 tests) also passed.
Coverage includes the 256/257-message boundary and WAL publication counts,
invalid/duplicate ACKs, WORK/FANOUT behavior, transaction rollback, injected
publication failure, ordering, retry/DLQ and process-exit recovery. The recovery
matrix covers Strict/WalDurableGrouped and bounded/unbounded storage, repeated
cold boots and checkpoints while checking that metadata stays in `.rdb`/WAL.

Windows compilation was blocked by the host's missing NASM prerequisite;
compilation and persistence verification were performed on Linux. The first CI
run reached 7,917 passing unit tests but exhausted its job time budget; it is
not counted as an overall successful run.

The final [Linux CI run 42415182947](https://github.com/reddb-io/reddb/actions/runs/42415182947)
passed on runtime commit `a1f12745`: unit tests, all four integration shards,
doctests, persistent multimodel tests, formatting/check/clippy, RQL and driver
conformance. Subsequent commits only record documentation and benchmark
artifacts; the tested Rust sources, Cargo inputs and CI workflow are unchanged.

## Queue/table relevance joins

Ordinary collection queries already join a queue with a table:

```sql
SELECT q.payload
FROM email_jobs q
JOIN recipients r ON q.payload = r.email
WHERE r.opened = false
```

Here the queue payload is the recipient email string. Tests verify that changing
`recipients.opened` changes this result without consuming either queued message,
including across checkpoints and cold boots. Use the ordinary queue collection
name in `FROM`; the specialized `FROM QUEUE` source and a derived `FROM QUEUE`
subquery do not support this JOIN form.

The query is read-only, can include messages already in flight and does not
reserve or ACK work. The related row can change after selection. Atomic
conditional claiming based on a JOIN would need a separate operation and
concurrency contract; this change does not add one.

## Reproduction

Prepare the fresh containers described by the capture manifest, install the
baseline/candidate binaries, then run:

```sh
python bench/kv/run_batch_matrix.py \
  --prefix reddb-delivery-v2 --suffix 1107 \
  --out bench/results/queue-delivery-batch-v2-2026-11-07 \
  --queues-only --queue-baseline --red-ack-batch \
  --queue-settle-ms 2000 --operations 256
```

Use a new output directory and fresh volumes for a new capture. See
[`bench/kv/BATCHING.md`](../../bench/kv/BATCHING.md) for the fixture contract.
