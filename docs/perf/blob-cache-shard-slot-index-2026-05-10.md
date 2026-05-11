# Blob Cache shard slot-index bench - 2026-05-10

Issue: #225

## Change

`blob/shard.rs::Shard` now stores stable `slots` and each in-memory
`Entry` stores its `slot_index`. Exact-key replacement and removal no longer
scan `Vec<BlobCacheKey>` with `order.iter().position(...)`.

## Criterion W9

Command:

```bash
cargo bench -p reddb-io-server --bench blob_cache_bench w9-shard-insert-remove-slot-index
```

Host run: local development workstation, bench profile, 10 Criterion samples.

| Workload | Time | Throughput |
| --- | ---: | ---: |
| N=10,000 single-shard put + invalidate | 9.06 ms | 2.21 Melem/s |
| N=100,000 single-shard put + invalidate | 106.67 ms | 1.87 Melem/s |

## Pre-225 algorithm check

Standalone release-mode model comparing the removed linear-order algorithm to
the slot-index algorithm:

| N | Linear order | Slot index | Speedup |
| --- | ---: | ---: | ---: |
| 10,000 | 5.590 ms | 0.560 ms | 10.0x |
| 100,000 | 795.002 ms | 6.467 ms | 122.9x |

The N=100,000 ratio clears the issue threshold of at least 10x.
