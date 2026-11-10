# Binary WAL stream microbenchmark — 2026-11-10

This report compares the version 1 protobuf WAL data plane with the legacy
JSON/hex response. It is a local encoding microbenchmark, not an end-to-end
replication SLO or a production capacity claim.

Run from the repository root:

```sh
cargo bench -p reddb-io-server --bench wal_stream_bench
```

The checked-in Criterion target covers 1,000 small records (128 bytes each)
and 16 large records (64 KiB each). It reports encoded network bytes,
encode-time distributions, throughput, and retains Criterion artifacts for
comparison across revisions.

## Local smoke result

The implementation run used Rust 1.95.0 on Windows in `dev --quick` mode after
a clean build. Debug-mode CPU ratios are useful for catching gross regressions,
but release results are required before making product performance claims.

| Case | Protobuf bytes | JSON/hex bytes | Protobuf time | JSON/hex time |
| --- | ---: | ---: | ---: | ---: |
| 1,000 × 128 B | 136,880 | 278,024 | 2.009 ms | 22.586 ms |
| 16 × 64 KiB | 1,048,742 | 2,097,608 | 405.79 µs | 125.61 ms |

The binary contract used 49.2% and 50.0% of the legacy wire bytes in these two
fixtures. The time figures exclude network, disk, apply, relay persistence,
cursor persistence, and ACK latency.

## Qualification boundary

Before treating the structural change as a production performance result, run
the benchmark in release mode and add an end-to-end primary/replica capture
with multiple concurrent replicas. That capture must record allocation counts,
peak RSS, throughput, and p50/p95 catch-up latency for both transports. Existing
replica replay and relay crash tests remain the correctness gate for
apply → relay durability → cursor → ACK ordering and idempotent retry; the wire
change does not move those steps.
