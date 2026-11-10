# Grouped durable publications and queue transitions — 2026-11-07

Concurrent KV throughput increased by 2.83–2.98x in the final local matrix.
Queue throughput including delivery and ACK increased by 1.60x at batch 1 and
2.15x at batch 16. Single-connection KV remained close to the baseline (0.97–1.02x).
Redis and the brokers still have substantially higher batched throughput.
These are short shared-host diagnostics, not production sizing or HA claims.

## Changes and recovery boundary

- Native paged `WalDurableGrouped` gathers independent ordinary KV replacements
  and plain, uncapped queue publications before taking the checkpoint fence.
  It uses the configured statement limit and an estimated WAL-byte threshold.
  Staging, durable WAL publication, checkpoint references and failure rollback
  remain inside that fence. Callers wait for the shared durable result.
- A ready group requests immediate flushing from the existing WAL coordinator;
  it does not pay a second grouping window. This changes flush scheduling, not
  the durable-LSN condition used to acknowledge callers.
- Autocommit queue delivery writes the reservation and attempt counter in one
  WAL batch. WORK ACK retires the message and associated metadata together;
  FANOUT ACK retires only the consumer group's state. Existing record kinds,
  legacy delivery handles, retry policy and explicit transaction paths remain.
- The recovery test exposed a resident-only queue projection. Queue SELECT now
  uses the fallible merged visitor, retaining snapshot and RLS filtering and
  propagating cold-read budget failures instead of returning partial results.

Storage sketch: N concurrent publications can share one fsync; ordinary WORK
READ changes from two WAL batches to one, and ACK from three to one. The test
counts actual `TxCommitBatch` records. READ COUNT still commits each delivered
message separately, and ACK retains one durable publication per request; these
are remaining costs, not a new multi-message ACK wire command.

Strict mode retains its WAL-before-page-flush contract. No file format or WAL
record type changes, no new persistent metadata files, and no new collection
kinds were introduced. Database state stays in `.rdb` and WAL; independent audit
logs retain their existing exception.

## Final KV matrix

256 keys, 256 durable overwrites per round, 100-byte values, three rounds per
cell. Values are medians of round metrics. HTTP pipelining is independent
requests, not an atomic multi-key operation. p99 is whole-batch completion.

| Connections | Pipeline | RedDB before ops/s | RedDB after ops/s | Ratio | Redis ops/s | RedDB after p99 ms | Redis p99 ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 267.0 | 258.6 | 0.97x | 311.4 | 6.07 | 11.41 |
| 1 | 16 | 289.7 | 295.6 | 1.02x | 4,313.8 | 60.71 | 4.67 |
| 8 | 1 | 293.9 | 830.4 | 2.83x | 1,357.9 | 15.41 | 8.80 |
| 8 | 16 | 295.6 | 879.9 | 2.98x | 14,781.4 | 159.07 | 10.05 |

All 12 KV cells passed full readback after SIGKILL/restart.

## Final queue matrix

Three rounds of 256 messages, 100-byte payloads. Each full cycle includes
publication, delivery and consumption completion. Publication throughput below
uses only the measured publication phase; it is not full-cycle throughput.
Both RedDB revisions have the same preceding KV workload and population.

| Product | Batch | Publication messages/s | Full cycles/s | Whole-batch p99 ms |
| --- | ---: | ---: | ---: | ---: |
| RedDB before | 1 | 259.4 | 40.4 | 29.68 |
| RedDB before | 16 | 296.3 | 45.5 | 376.59 |
| RedDB after | 1 | 253.6 | 64.7 | 20.02 |
| RedDB after | 16 | 692.3 | 97.8 | 172.36 |
| RabbitMQ | 1 | 164.4 | 149.6 | 14.25 |
| RabbitMQ | 16 | 1,661.3 | 1,396.6 | 13.91 |
| Kafka | 1 | 222.3 | 65.9 | 23.12 |
| Kafka | 16 | 1,833.0 | 808.4 | 39.30 |

All eight queue publication/recovery checks and eight post-ACK restart checks
passed. Each publication recovery seeds 32 confirmed messages, kills the server,
verifies their payloads and acknowledges consumption, then kills it again and
checks that progress survived.

The contracts differ: RedDB returns a durable ACK response; RabbitMQ uses a
one-way consumer ACK followed by an ordered channel RPC (which does not prove
ACK fsync); Kafka synchronously commits offsets and retains message records.
Redis uses AOF `appendfsync always`; RabbitMQ uses a single-node quorum queue;
Kafka uses one broker, `acks=all`, idempotence and `flush.messages=1`. Versions
are Redis 8.4.0, RabbitMQ 4.1.4 and Kafka 4.1.0. Protocols and client libraries
also differ, so these tables do not establish equivalent delivery/HA guarantees.

Every queue/topic gets a fixed two-second settle after declaration, before any
payload is published. There is no added delay between confirmation and SIGKILL.
The earlier newborn RabbitMQ recovery failure remains documented in the
[previous report](kv-batching-2026-11-07.md); this established-topology profile
does not invalidate that result.

## Evidence and validation

- [Final raw capture](../../bench/results/grouped-publication-final-2026-11-07/):
  manifests, exact binary hashes, every round, recovery outcomes and resource samples.
- [Initial raw capture](../../bench/results/grouped-publication-2026-11-07/):
  the intermediate `23d4bdcd5` implementation, before removing the second wait.
  This capture also passed all recovery checks. Its first baseline KV cell was
  unusually slow (70.9, 76.0 and 90.7 ops/s), unlike its later baseline cells;
  it is retained and is not used to claim a single-writer speedup. The final
  capture explicitly synced the build filesystem and stopped the build
  container before timing, and used fresh containers and volumes.
- [Method and reproduction](../../bench/kv/BATCHING.md).
- [Full CI](https://github.com/reddb-io/reddb/actions/runs/42400406529), on the
  final Rust revision `e32f4ed7e`. Subsequent report/evidence commits do not
  change the Rust implementation.

Final focused Linux validation: three grouped-publication/failure/checkpoint
tests; one blocked-WAL test proving ready publications still wait for fsync;
16 queue adapter tests including transaction rollback, legacy handles, retry/DLQ
and one-batch delivery/ACK assertions; all eight `no_metadata_sidecars`
integration tests. The latter run process-exit recovery across Strict/grouped
and bounded/unbounded native layouts, followed by repeated cold boots and
checkpoints. The cold queue tests also reject an unreadable oversized record
and succeed after the read allowance is restored. Windows `cargo check`, Linux
format checking and the 17 Python benchmark harness tests passed.

Both benchmark servers use four CPUs and 2 GiB container limits; the client uses
two CPUs and 1 GiB. Timings are from one shared Docker/WSL host. Resource samples
include setup/recovery and do not prove client non-saturation. Small samples do
not establish tail-latency SLOs, power-loss behavior or multi-node durability.

Binary provenance:

- Before: main `64a33c6c6e9c025ca0181b9a56f051eb58e9a09f`, SHA-256
  `0f3ab3b71a704f9b9d7e0f38ba131890d1d63e782d68f2f2ec5af372f26dc78a`.
- After: `e32f4ed7e10dfb3650b2eb2402ca7b7b47ab9eec`, SHA-256
  `5212182f256cbb26879aa5c9205f6ff92764215da425b0fa6eb607573a204795`.

The Linux build's 2,069 Rust/Cargo source files were compared with the worktree
with newline normalization; final implementation changes were copied and
touched before rebuilding. No compiler ran during either timed capture.
