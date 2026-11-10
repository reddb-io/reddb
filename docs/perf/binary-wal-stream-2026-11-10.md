# Binary WAL stream qualification — 2026-11-10

This report compares the version 1 protobuf WAL data plane with the legacy
JSON/hex response. It contains both a codec microbenchmark and a loopback gRPC
catch-up benchmark. It is not a production replication SLO or capacity claim.

Run from the repository root:

```sh
cargo bench -p reddb-io-server --bench wal_stream_bench
cargo bench -p reddb-io-server --bench wal_stream_rpc_bench
```

The checked-in Criterion target covers 1,000 small records (128 bytes each)
and 16 large records (64 KiB each). It reports encoded payload bytes before
gRPC/HTTP/2 framing,
encode/decode time, allocation count and bytes, isolated peak RSS, and
four-replica codec throughput. The loopback target uses 128 small records or
16 large records per replica across four concurrent connections. It warms both
transports and alternates their sampling order before reporting p50/p95 catch-up
latency and user-payload throughput.

## Local smoke result

The qualification run used Rust 1.95.0 on Windows in `dev --quick` mode after
a clean build. Debug-mode ratios are useful for catching gross regressions, but
release results on a dedicated runner are required before making product
performance claims.

| Case | Protobuf bytes | JSON/hex bytes | Protobuf time | JSON/hex time |
| --- | ---: | ---: | ---: | ---: |
| 1,000 × 128 B | 136,880 | 278,024 | 2.009 ms | 22.586 ms |
| 16 × 64 KiB | 1,048,742 | 2,097,608 | 405.79 µs | 125.61 ms |

The original smoke row above is retained for comparison with the foundation
change. The expanded rerun measured the following codec paths:

| Case/path | Protobuf time | JSON/hex time | Protobuf throughput | JSON/hex throughput |
| --- | ---: | ---: | ---: | ---: |
| Small encode | 737.01 µs | 15.511 ms | 165.63 MiB/s | 7.87 MiB/s |
| Small decode | 1.096 ms | 7.524 ms | 111.39 MiB/s | 16.22 MiB/s |
| Small, four-replica round trip | 6.881 ms | 93.454 ms | 70.96 MiB/s | 5.22 MiB/s |
| Large encode | 223.98 µs | 97.438 ms | 4.36 GiB/s | 10.26 MiB/s |
| Large decode | 70.265 µs | 35.954 ms | 13.90 GiB/s | 27.81 MiB/s |
| Large, four-replica round trip | 1.650 ms | 563.54 ms | 2.37 GiB/s | 7.10 MiB/s |

The binary contract used 49.2% and 50.0% of the legacy wire bytes in the two
fixtures. These codec figures exclude network, disk, apply, relay persistence,
cursor persistence, and ACK latency.

## Allocation and memory result

Allocation counters start after fixture construction and cover one encode or
decode operation. RSS probes run each transport in a fresh child process and
retain four encoded large-record responses concurrently.

| Case/path | Protobuf allocations / bytes | JSON/hex allocations / bytes |
| --- | ---: | ---: |
| Small encode | 1,001 / 160,880 | 4,021 / 1,976,169 |
| Small decode | 1,009 / 209,760 | 6,025 / 1,410,537 |
| Large encode | 17 / 1,049,126 | 79 / 6,176,472 |
| Large decode | 19 / 1,049,696 | 109 / 5,255,617 |

| Four-replica large response | Protobuf | JSON/hex |
| --- | ---: | ---: |
| Retained wire bytes | 4,194,968 | 8,390,432 |
| Peak RSS | 12,435,456 | 19,791,872 |
| Peak RSS growth during probe | 1,449,984 | 8,691,712 |

## Four-replica loopback result

The loopback fixture sends deterministic non-trivial alphanumeric values so
the legacy path cannot hide its expansion behind an unrealistically repeated
payload. Twenty warmed samples alternate transport order. The measured window
includes gRPC, server WAL reads, protobuf/JSON decode, and all stream chunks;
it excludes replica apply, durable relay/cursor writes, and ACK.

| Case | Transport | p50 catch-up | p95 catch-up | p50 throughput |
| --- | --- | ---: | ---: | ---: |
| 128 × 128 B per replica | Protobuf stream | 180.0 ms | 282.5 ms | 0.347 MiB/s |
| 128 × 128 B per replica | JSON/hex unary | 209.1 ms | 502.6 ms | 0.299 MiB/s |
| 16 × 64 KiB per replica | Protobuf stream | 472.5 ms | 594.9 ms | 8.466 MiB/s |
| 16 × 64 KiB per replica | JSON/hex unary | 874.6 ms | 960.2 ms | 4.574 MiB/s |

The one-MiB binary chunk ceiling remains active in this benchmark, so the large
case exercises multi-chunk streaming and capacity-one backpressure rather than
turning the binary path into another unbounded unary response.

## Correctness coverage

`e2e_issue_828_replication_tail_follow` now runs the binary stream through a
real authenticated gRPC server. It applies a record, drops the connection
before ACK, reconnects at the unadvanced slot, requires the same LSN to replay,
and verifies that the applier reports `Idempotent` before the final ACK drives
lag to zero. The existing relay crash-point suite and issue #813 replay suite
remain the durability and state-convergence gates for relay persistence and a
wiped cursor.

## Qualification boundary

The structural gates now cover network bytes, encode/decode CPU, allocation
counts, isolated peak RSS, throughput, p50/p95 loopback catch-up latency, small
and large records, and four concurrent replicas. Before treating the result as
a production performance claim, rerun both targets in release mode on a
dedicated host and capture a non-loopback network profile. Apply → relay
durability → cursor → ACK ordering is unchanged by the wire format and remains
guarded by the binary retry test plus the relay crash and replay suites.
