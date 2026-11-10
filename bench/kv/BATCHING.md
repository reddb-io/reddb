# Durable KV and queue batch diagnostics

This matrix exercises network APIs, not embedded calls. `compare.py` now supports
HTTP/1.1 pipelines for both REST KV and `/query`, as well as RESP pipelines.
Every response is consumed; GET values and the complete post-write dataset are
checked. A pipeline is independent commands, **not an atomic multi-key write**.

The engine change routes ordinary paged KV replacements through the existing
single WAL publication for replacement, deletion and metadata. Transactions,
versioned KV, Config and Vault retain their existing paths. Approximate cost:
an overwrite with metadata previously needed up to three durable publications;
one batch removes two persistence waits without acknowledging before durability.
Strict still flushes pages; grouped mode still waits for its WAL durability fence.

The recovery test also exposed a cold tag-cache omission. Cold GET/INCR/CAS read
authoritative metadata on demand. Explicit tag invalidation traverses candidate
keys and resolves their current versions before deletion. That fallback is O(N)
and keeps keys, not payloads, in its temporary set. It does not add a boot scan,
persist an auxiliary tag file, or claim indexed cold tag invalidation.

## Matrix and limits

`--red-read-concurrency 8` splits a RedDB window into up to eight concurrent READ
requests. With `--red-ack-batch`, each READ result gets its own atomic ACK BATCH,
and those ACK requests run concurrently. The matrix applies the READ setting to
both RedDB versions; use `--baseline-red-ack-batch` as well when comparing versions
that both support batch ACK. RabbitMQ/Kafka adapters retain their native consume
paths. The [concurrent-publication capture](../../docs/perf/queue-cross-request-batching-2026-11-08.md)
records this request pattern separately from the earlier single-READ window.

- Servers: four CPUs pinned to IDs 0–3, 2 GiB process memory, isolated Docker
  network and persistent local Docker volumes. RedDB engine budget: 1 GiB;
  four HTTP execution workers, admission cap 64, result cache disabled.
- Client: Python 3.12, two CPUs pinned to 4–5, 1 GiB. Reused connections.
- KV: 100-byte deterministic values; 256 keys/operations, three rounds;
  1/8 connections × pipeline 1/16. The last incomplete pipeline is counted
  accurately. Each cell starts with its own verified collection.
- Queues: 100-byte unique payloads, 256 complete produce → consume → ACK cycles,
  three rounds, outstanding batch 1/16, one queue/topic per round. Startup and
  a full warmup cycle (including Kafka assignment) are outside the clock.
- Whole-batch p50/p95/p99 and all batch samples are retained. Batch latency is
  never divided by batch size and presented as per-message latency.
- SIGKILL/restart follows every KV cell. A separate queue recovery phase kills
  after 32 confirmed publications, verifies every payload, then kills after
  consumption and checks that committed progress survived. These are process
  crash checks, not power-loss simulations.

Durability profiles are deliberately explicit:

| Product | Publication confirmation | Consumption completion |
| --- | --- | --- |
| RedDB native paged | WalDurableGrouped, 200 µs window; WAL durable before response | READ plus durable ACK response |
| Redis 8.4.0 | AOF, `appendfsync always`, snapshots disabled | KV readback |
| RabbitMQ 4.1.4 | Single-node quorum queue, persistent messages, mandatory routing, publisher confirms | Push consumer with batch-sized prefetch; manual ACK followed by ordered channel RPC |
| Kafka 4.1.0 | Single broker, RF=1, `acks=all`, idempotence, `flush.messages=1`, linger=0 | Explicit synchronous offset commit; records remain retained |

RabbitMQ consumer ACK is one-way: the ordered RPC is not a durable ACK response.
Kafka offsets/retention are different from message deletion and retry. All are
single-node diagnostics, not HA comparisons. Kafka's explicit fsync profile is
different from its usual replication-based durability recommendation. Redis can
group fsync across pipelines while keeping `appendfsync always`.

References: [Redis persistence](https://redis.io/docs/latest/management/persistence/),
[RabbitMQ confirms](https://www.rabbitmq.com/docs/confirms),
[RabbitMQ persistence details](https://blog.rabbitmq.com/blog/2025/01/17/how-are-the-messages-stored),
[Kafka 4.1 topic settings](https://kafka.apache.org/41/configuration/topic-configs/).

These short shared-host samples are useful diagnostics, not public leadership
claims or steady-state saturation measurements. Protocols, envelopes and client
implementations differ. Review client/server CPU samples before inferring an
engine bottleneck. Small operation counts do not establish tail-latency SLOs.

The November capture keeps two distinct topology profiles. The zero-delay
profile failed to recover a newly created RabbitMQ quorum queue after 32
publisher confirmations; it remains a failed recovery, not a passing sample.
The established-topology continuation waits two seconds **after declaration,
before publishing any payload**, for every product. It adds no wait after the
last publication confirmation and before SIGKILL. This isolates message-path
durability from the observed queue-registration window. See the
[results and qualifications](../../docs/perf/kv-batching-2026-11-07.md).

## Reproduction

Build optimized Linux `red` binaries from the recorded before/after revisions:
`cargo build --release --bin red`. Keep them separately. If extracting an archive
over a warm build tree, use `tar -xm` so Cargo sees current source timestamps.

Create isolated containers with names matching `run_batch_matrix.py` (default
prefix `reddb-batch`, suffix `1107`). The capture's `manifest.json` records exact
commands, environments, images/digests, mounts and binary SHA-256 hashes. Do not
reuse populated volumes for an independent repeat. Do not run a build during
timing. The runner stops other **named benchmark containers** between engines;
it does not delete containers, volumes or data.

For both RedDB containers (`native-before` and `native-after`), mount a fresh
volume at `/data`, copy the corresponding executable to `/usr/local/bin/red`,
and use these environment variables:

```text
REDDB_DURABILITY=wal_durable_grouped
REDDB_DURABILITY_MODE=wal_durable_grouped
REDDB_GROUP_COMMIT_WINDOW_US=200
REDDB_MEMORY_BUDGET=1073741824
REDDB_RUNTIME_RESULT_CACHE_ENABLED=false
```

Command:

```sh
/usr/local/bin/red server --http-bind 0.0.0.0:5000 --path /data/data.rdb \
  --no-auth --http-max-handlers 64 --workers 4 --bounded-disk-residency
```

These anonymous test services must remain on the isolated Docker network with
no published host ports. Redis command: `redis-server --appendonly yes
--appendfsync always --save ''`. RabbitMQ uses fixture credentials `bench/bench`.
Kafka's broker/controller environment is captured in the manifest; set
`KAFKA_LOG_FLUSH_INTERVAL_MESSAGES=1` as well, including the offsets topic.

The full matrix also measures queues on `native-before`, using the same KV
population as `native-after`; `--queues-only` retains the after/broker
comparison. Stop builds before measurement and flush their pending
filesystem writes (`sync` inside the Linux build container) before stopping
that container. Keep the first capture if a follow-up changes code or setup;
use a new prefix, volumes and output directory for the follow-up.

The client container mounts this repository read-only at `/source`, stays up
with `sleep infinity`, and installs pinned dependencies:

```sh
pip install aiohttp==3.13.2 aio-pika==9.5.7 confluent-kafka==2.12.2
python bench/kv/run_batch_matrix.py --out /path/to/new-capture-directory
python -m unittest discover -s bench/kv -p test_compare.py
```

Run the orchestration command on the Docker host, not inside the client. It saves
raw results plus resource samples, manifests and a `complete.json` only if every
measurement and recovery check succeeds. A failed capture must remain invalid.
The runner polls listener readiness; each adapter also performs an untimed
protocol warmup. `--resume-queues` can continue only at fully completed engine
boundaries, with identical binaries/containers and all KV recoveries passed.
Partially consumed queue workloads require fresh fixtures instead of a retry
that could conceal loss or duplicates.

For the separate established-topology queue capture, create fresh client,
`native-after`, `rabbit` and `kafka` containers/volumes under a different prefix:

```sh
python bench/kv/run_batch_matrix.py --prefix reddb-batch-mature --queues-only \
  --queue-settle-ms 2000 --out /path/to/new-established-queue-capture
```

Engine regression commands:

```sh
cargo check -p reddb-io-server
cargo test -p reddb-io-server --lib runtime::impl_kv::tests
cargo test -p reddb-io-server --lib keyed_write::tests
cargo test -p reddb-io-server --lib cold_keyed_models
cargo test -p reddb-io-server --test no_metadata_sidecars
```

### Multi-message delivery and ACK comparison

Use `--queues-only --queue-baseline --red-ack-batch --queue-settle-ms 2000`
with `run_batch_matrix.py` to compare fresh `native-before`, `native-after`,
RabbitMQ and Kafka fixtures without a preceding KV workload. Only `native-after`
uses the new atomic ACK command; the baseline retains concurrent individual
ACK requests. Each engine still consumes the same messages, checks every payload,
and runs publication and post-ACK SIGKILL/restart checks. The manifest records
this choice. The protocol and ACK durability differences described above remain.

RedDB samples also include `read_batch_ms` and `ack_batch_ms`, measured at the
client around each network operation. They separate reservation and ACK latency
from publication, but include transport, locks and durability waits; they are
not server CPU or fsync measurements. Compare both ACK syntaxes on the same
binary when investigating singleton overhead.

When the baseline already supports batch ACK, add `--baseline-red-ack-batch`
alongside `--red-ack-batch` to compare the same ACK syntax on both versions.
The manifest records both choices and resume rejects a change to either mode.
