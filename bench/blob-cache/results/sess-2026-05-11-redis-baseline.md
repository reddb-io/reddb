# Blob Cache Redis baseline rollup

Session id: `sess-2026-05-11-redis-baseline`

## Host / build

- Git SHA: `47d5493366329e36f7162eec8628684b9a36bf40`
- Host: `Linux cyber-XPS-13-9300 6.17.0-23-generic #23~24.04.1-Ubuntu SMP PREEMPT_DYNAMIC Tue Apr 14 16:11:48 UTC 2 x86_64`
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`
- Cargo: `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`
- Redis image: `redis:7.4`
- Redis image id: `sha256:d8c9043d4df07381c25b0afc9103697f7bff7a48c20e0af565c5855e0abeae16`
- Redis env: `REDIS_NO_PERSIST_ADDR=127.0.0.1:6379`, `REDIS_AOF_ADDR=127.0.0.1:6380`

## Commands

```bash
chmod +x bench/blob-cache/redis-up.sh bench/blob-cache/redis-down.sh
bench/blob-cache/redis-down.sh --wipe-aof
bench/blob-cache/redis-up.sh
mkdir -p bench/blob-cache/results
REDIS_NO_PERSIST_ADDR=127.0.0.1:6379 \
REDIS_AOF_ADDR=127.0.0.1:6380 \
  cargo bench -p reddb-server --bench blob_cache_bench 'w[1-8]' -- --nocapture \
  2>&1 | tee bench/blob-cache/results/sess-2026-05-11-redis-baseline.raw.log
REDIS_NO_PERSIST_ADDR=127.0.0.1:6379 \
REDIS_AOF_ADDR=127.0.0.1:6380 \
  cargo bench -p reddb-server --bench blob_cache_bench w7-restart-warm-cache -- --nocapture \
  2>&1 | tee bench/blob-cache/results/sess-2026-05-11-redis-baseline-w7-rerun2.raw.log
```

Workload 7 Redis AOF restart used the procedure from
`bench/blob-cache/redis-setup.md`: populate `128 x 8 KiB` keys, run
`BGREWRITEAOF`, stop `reddb-bench-redis-aof-everysec`, start it again
against the same `reddb-bench-redis-aof` volume, poll `PING`, and issue
`GET` for the known key set. Raw output is in
`bench/blob-cache/results/sess-2026-05-11-redis-baseline-redis-w7.raw.log`.

## Rollup

| workload / backend | metric | value |
|--------------------|--------|------:|
| w1 BlobCache L1 | mean time | 0.320 us |
| w1 BlobCache L1 | throughput | 3.12 M ops/sec |
| w1 ResultCache | mean time | 0.300 us |
| w1 ResultCache | throughput | 3.33 M ops/sec |
| w1 Redis no-persist GET | mean time | 143.80 us |
| w1 Redis no-persist GET | throughput | 6.95 K ops/sec |
| w1 Redis no-persist MGET-32 | mean time | 1.1916 ms per batch |
| w1 Redis no-persist MGET-32 | throughput | 26.86 K elements/sec |
| w1 Redis aof-everysec GET | mean time | 160.68 us |
| w1 Redis aof-everysec GET | throughput | 6.22 K ops/sec |
| w2 BlobCache L2 hit | mean time | 50.461 us |
| w2 Redis aof-everysec GET | mean time | 187.14 us |
| w3 BlobCache synopsis miss | mean time | 0.378 us |
| w3 BlobCache synopsis miss | l2 skip-rate | 100.0% |
| w3 Redis no-persist miss | mean time | 148.97 us |
| w4 BlobCache L2 5 MiB | mean time | 24.217 ms |
| w4 BlobCache L2 5 MiB | throughput | 206.47 MiB/sec |
| w4 Redis no-persist 5 MiB | mean time | 3.3099 ms |
| w4 Redis no-persist 5 MiB | throughput | 1.4752 GiB/sec |
| w4 Redis aof-everysec 5 MiB | mean time | 4.2166 ms |
| w4 Redis aof-everysec 5 MiB | throughput | 1.1580 GiB/sec |
| w5 BlobCache generation bump | mean time | 0.000177 ms |
| w5 Redis FLUSHDB | mean time | 1.0801 ms |
| w5 Redis prefix DEL sweep | mean time | 117.50 ms |
| w6 BlobCache dep-tag | mean time | 0.2702 ms |
| w6 BlobCache dep-tag | invalidated count | 250 |
| w6 ResultCache invalidate deps | mean time | 0.0593 ms |
| w6 Redis Lua tag-set sweep | mean time | 0.5956 ms |
| w7 BlobCache reopen + first hit | mean time | 1.1602 ms |
| w7 BlobCache reachable entries | count | 127/128 |
| w7 Redis AOF restart | open time | 265.684 ms |
| w7 Redis AOF restart | first-hit p50 | 1275.310 us |
| w7 Redis AOF restart | reachable entries | 128/128 |
| w8 SIEVE WS 0.5 x L1 | hit-rate | 100.0% |
| w8 SIEVE WS 0.5 x L1 | evictions | 0 |
| w8 SIEVE WS 1.0 x L1 | hit-rate | 100.0% |
| w8 SIEVE WS 1.0 x L1 | evictions | 0 |
| w8 SIEVE WS 2.0 x L1 | hit-rate | 54.2% |
| w8 SIEVE WS 2.0 x L1 | evictions | 10120 |
| w8 Redis allkeys-lru WS 2.0 x L1 | mean time | 124.59 us |
| w8 Redis allkeys-lru WS 2.0 x L1 | hit-rate | 100.0% |
| w8 Redis allkeys-lru WS 2.0 x L1 | evictions | 0 |

## Notes

- Criterion reports confidence intervals, not p50/p99 percentiles. The
  public report records the reported mean as the central latency and uses
  the upper confidence bound as a conservative tail proxy for this session.
- The workload 7 reachable-entry stat comes from the focused rerun after
  the harness was corrected to count successful post-restart `get` calls
  rather than reopened L1 entries.
- The W-TinyLFU row remains `n/a`: there is no W-TinyLFU bench flag in this
  harness. The shipped-policy SIEVE hit-rate cells are populated.
