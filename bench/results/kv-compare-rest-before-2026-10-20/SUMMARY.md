# Local KV comparison — verified product paths

Engine revision: `a3af4d6261f05fcc2dccd5caca178c5ddf6936a3`. Classification: **shared-host diagnostic**.

The table contains medians of five round-level metrics, not pooled request percentiles. All raw round samples and their ranges remain in the JSON. CPU percentages use 100% per core; memory is sampled cgroup working set, not per-key memory or RSS. Missing telemetry is unavailable.

## GET without pipelining

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | reddb-query (cap64) | 1,290.3 | 729.6 | 1,093.8 | 1,248.4 | 74.4 / 41.5 | 90.8 |
| 1000 | 1 | reddb-rest (cap64) | 858.8 | 1,107.0 | 1,644.9 | 1,861.3 | 77.6 / 23.7 | 74.4 |
| 1000 | 8 | reddb-query (cap64) | 2,370.5 | 3,092.3 | 6,110.1 | 8,196.4 | 112.8 / 123.4 | 90.8 |
| 1000 | 8 | reddb-rest (cap64) | 2,755.4 | 2,502.8 | 5,206.6 | 7,117.8 | 289.9 / 110.6 | 75.6 |
| 1000 | 32 | reddb-query (cap64) | 2,580.3 | 11,064.9 | 25,240.5 | 32,904.4 | 121.3 / 129.9 | 91.0 |
| 1000 | 32 | reddb-rest (cap64) | 3,147.2 | 8,493.7 | 21,766.2 | 30,166.4 | 309.6 / 125.0 | 84.8 |
| 10000 | 1 | reddb-query (cap64) | 583.0 | 1,509.2 | 2,409.2 | 5,996.5 | 77.9 / 31.6 | 106.2 |
| 10000 | 1 | reddb-rest (cap64) | 84.2 | 11,420.3 | 15,037.6 | 18,011.1 | 95.4 / 4.0 | 96.5 |
| 10000 | 8 | reddb-query (cap64) | 1,945.4 | 3,663.2 | 7,782.3 | 11,963.9 | 163.1 / 120.0 | 112.0 |
| 10000 | 8 | reddb-rest (cap64) | 239.9 | 32,131.9 | 54,440.1 | 65,004.6 | 377.6 / 13.4 | 131.3 |
| 10000 | 32 | reddb-query (cap64) | 1,821.6 | 15,059.2 | 39,971.6 | 59,040.5 | 170.2 / 127.9 | 112.5 |
| 10000 | 32 | reddb-rest (cap64) | 156.7 | 197,273.3 | 304,315.1 | 343,245.4 | 404.7 / 11.1 | 261.8 |

## RESP GET, pipeline 16 — batch completion latency

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 1 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 1000 | 8 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 8 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 1000 | 32 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 32 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 1 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 1 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 8 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 8 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 32 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 32 | reddb-rest (cap64) | unsupported | — | — | — | — | — |

## Durable overwrites — KiviDB includes SET + WAITAOF

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | reddb-rest (cap64) | 99.8 | 9,221.6 | 13,016.5 | 16,653.5 | 11.2 / 4.5 | 128.5 |
| 1000 | 8 | reddb-rest (cap64) | 98.3 | 74,064.0 | 131,686.7 | 182,310.2 | 14.0 / 4.8 | 122.4 |

## Qualification

HTTP/JSON and RESP are different product paths. Python threads and the two-core client budget can limit throughput. Pipeline-16 latency refers to a complete batch; it is not directly comparable with a single-request percentile. Server order is fixed and the host is shared. These measurements cannot establish maximum engine throughput or ADR 0076 leadership.

See [the experiment contract](../../kv/COMPARE.md), `manifest.json`, `summary.json`, the raw cell JSON and resource JSONL files for setup, unsupported cells, failures and recovery checks.
