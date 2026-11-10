# Durable KV batching and network queue diagnostics — 2026-11-07

Paged KV overwrite throughput improved by 2.04–2.25× in this local matrix.
It still does not scale with concurrent pipelines like Redis. Queue delivery
and acknowledgment remain substantially more expensive than publication.
These are short shared-host diagnostics, not production sizing or leadership
claims. All unfavorable cells and the failed RabbitMQ recovery are retained.

## What changed

Ordinary, non-versioned paged KV now uses one WAL transaction for its new value,
prior-row deletion and metadata. Strict page flushing and grouped WAL durability
remain in force. Mutation preimages use fallible disk reads. Explicit transactions,
versioned KV, Config and Vault keep their existing paths.

The new concurrent crash test exposed missing tags after a bounded cold boot:
the resident tag index did not cover cold rows. GET, INCR and CAS now consult
durable metadata when necessary. Explicit tag invalidation resolves cold keys
on demand; its cold fallback is still O(N), retaining keys rather than payloads.
Empty-tag invalidation remains a no-op. No new boot scan, file format, or database
metadata sidecar was introduced.

## KV: before, after, Redis

Each cell uses 256 keys, 100-byte values, three rounds of 256 durable overwrites,
persistent connections and complete readback. A SIGKILL/restart follows every
cell and checks the final expected values. All 12 cells passed recovery.
Values below are medians of the three round metrics; p99 is whole-batch completion,
not per-operation latency divided by pipeline depth.

| Connections | Pipeline | RedDB before ops/s | RedDB after ops/s | Redis ops/s | RedDB after batch p99 ms | Redis batch p99 ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 127.6 | 260.3 | 310.0 | 6.02 | 5.10 |
| 1 | 16 | 131.6 | 277.6 | 4,388.1 | 93.36 | 4.91 |
| 8 | 1 | 125.8 | 277.4 | 1,321.1 | 48.16 | 8.91 |
| 8 | 16 | 113.6 | 255.9 | 14,567.0 | 529.85 | 9.44 |

At one connection/pipeline 1, RedDB's p99 dropped from 12.98 to 6.02 ms and
throughput is now about 16% below Redis. At eight connections/pipeline 16, Redis
is still about 57× faster. Pipelining alone does not solve RedDB's publication
serialization. The HTTP harness now pipelines real requests and checks every
response, including responses prefetched together; this removes the old
RESP-only pipeline limitation without changing command atomicity.

## Queues: established topology

Fresh containers/volumes, 100-byte payloads, three rounds of 256 complete
publish → consume → ACK cycles. Batch means outstanding work, not a new atomic
multi-message API. Every payload and duplicate count is verified. Every product
waits two seconds after queue/topic declaration, before publishing; warmup is
also untimed. There is no added delay after publication confirmation before
the recovery kill.

| Product | Batch | Confirmed publication msgs/s | Complete cycles/s | Whole-batch p99 ms |
| --- | ---: | ---: | ---: | ---: |
| RedDB | 1 | 165.4 | 27.4 | 212.85 |
| RedDB | 16 | 276.9 | 42.5 | 479.40 |
| RabbitMQ | 1 | 143.9 | 130.1 | 20.56 |
| RabbitMQ | 16 | 1,626.9 | 1,331.9 | 13.03 |
| Kafka | 1 | 213.5 | 63.5 | 31.42 |
| Kafka | 16 | 1,728.5 | 783.9 | 39.87 |

Completion contracts differ. RedDB waits for a durable ACK response. RabbitMQ
consumer ACK is one-way; the harness follows it with an ordered channel RPC,
which is not a durability confirmation for the ACK. Kafka commits offsets
synchronously and retains records. Therefore cycle ratios are product-path
diagnostics, not interchangeable queue semantics or evidence that durability
should be weakened. Publication confirmations are reported separately.

All 18 established-topology rounds passed. For each product/batch pair, 32
confirmed publications survived SIGKILL/restart, consumption verified every
payload, and another SIGKILL/restart preserved consumer progress. This tests
process crashes, not hardware power loss or HA quorum failures.

RedDB's batch-1 cycle rates were 27.4, 29.8 and 16.8/s: substantial shared-host
variance, retained rather than replaced by the more favorable initial capture.
The original zero-delay RedDB capture, with a preceding KV workload, measured
36.9/42.6 cycles/s for batches 1/16 and also passed recovery. Those differing
background populations/profiles are not a controlled before/after comparison.
Resource logs include setup/recovery as well as timing; they do not by themselves
prove that a client never limited the workload. Small tail samples are not SLOs.

## New-queue recovery failure retained

In the initial zero-delay capture, RabbitMQ 4.1.4 confirmed 32 messages for a
new quorum queue. After restart, consumption timed out with `INTERNAL_ERROR`;
the broker logged `name_not_registered`, and quorum status showed a follower
at term/log index zero. The messages were not successfully recovered by this
test. This stopped that capture before Kafka; it has no passing completion marker.

This is consistent with the separately documented delayed registration sync
window for newly created quorum queues, acknowledged in the upstream discussion.
We did not independently prove the internal root cause or repair the broker.
[RabbitMQ discussion #17710](https://github.com/rabbitmq/rabbitmq-server/discussions/17710).

The established-topology run is a separately labeled control on fresh volumes,
not a replacement for this failure. An earlier listener-startup failure was also
retained: the orchestrator now polls readiness instead of assuming five seconds
is sufficient. Completed samples were never silently rerun or overwritten.

## Next implementation targets

1. **Native grouped commit across requests.** `write_keyed_row` still holds the
   publication lock while the native WAL path waits for durability;
   `enqueue_grouped_actions` currently serves embedded WAL only. A staged native
   commit mechanism must let checkpoints account for queued writes and preserve
   reader visibility, rollback, WAL generation and failure fencing. Simply
   releasing that lock is not a safe optimization.
2. **Delivery/ACK publication batches.** At batch 16, RedDB's confirmed-publication
   capacity is about 277/s but complete cycles reach only 42.5/s. Persisting
   delivery/pending/cursor and ACK changes with fewer durability waits is the
   next target, with regression coverage for redelivery, lease expiry, dedup,
   transaction abort and crash between publication and response.
3. **Native tag projection for large cold invalidations.** The correctness
   fallback remains a scan during explicit invalidation. Its eventual index
   must live in `.rdb` and WAL and reuse checkpoint roots at boot.

## Provenance and validation

- Before binary: `8fc8a1e29`, SHA-256
  `465a6da9d5241b201327a71c513594a92d2242b384a84cd627730c45d15bf890`.
- Measured after binary: `8b2e96182`, SHA-256
  `df1fa1e891bd31f5a2c5f15a3a0a5bbcc78c86b97c3fa637dee4bc98508ac44c`.
  Follow-up `6e6010063` only adds the empty-tag no-op guard and its error-path
  assertions; this path is not exercised in the timed workloads.
- RedDB: native paged, bounded residency, WalDurableGrouped, 200 µs commit window,
  result cache disabled. Redis 8.4.0: AOF `always`, snapshots disabled.
  RabbitMQ 4.1.4: single-node quorum, persistent messages, publisher confirms,
  push consumer/prefetch. Kafka 4.1.0: one broker/partition, RF=1, `acks=all`,
  idempotence, `flush.messages=1`, synchronous offset commits. These are local
  durability profiles, not typical replicated Kafka deployment settings.
- Each server: CPU IDs 0–3, four CPUs, 2 GiB; RedDB engine budget 1 GiB. Client:
  IDs 4–5, two CPUs, 1 GiB. Persistent Docker volumes. Engines run sequentially;
  this task's builds were completed before timed workloads. Other host activity
  remains an uncontrolled variable.
- Linux: 82 keyed/cold-storage tests, 33 KV tests, seven no-sidecar/recovery tests
  and two raw-iteration gates passed (some filters overlap). The WAL regression
  verifies one transaction per replacement with TTL/tags. Windows: `cargo check`
  and the concurrent KV process-exit/cold-boot test passed. Harness: 17 tests.

[Method and reproduction](../../bench/kv/BATCHING.md),
[initial KV and zero-delay raw capture](../../bench/results/kv-batching-2026-11-07/manifest.json),
[initial capture status](../../bench/results/kv-batching-2026-11-07/capture-status.json),
[established queue manifest](../../bench/results/queues-batching-established-2026-11-07/manifest.json),
[derived medians](../../bench/results/queues-batching-established-2026-11-07/summary.json),
[established recovery completion](../../bench/results/queues-batching-established-2026-11-07/complete.json).
