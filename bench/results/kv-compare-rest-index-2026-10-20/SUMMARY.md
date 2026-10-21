# Local KV comparison — verified product paths

Engine revision: `5ad0766eda49f7472bf85ee91524a17b3ec10673`. Classification: **shared-host diagnostic**.

The table contains medians of five round-level metrics, not pooled request percentiles. All raw round samples and their ranges remain in the JSON. CPU percentages use 100% per core; memory is sampled cgroup working set, not per-key memory or RSS. Missing telemetry is unavailable.

## GET without pipelining

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | reddb-query (cap64) | 985.3 | 935.1 | 1,359.5 | 2,223.1 | 72.0 / 42.7 | 80.6 |
| 1000 | 1 | reddb-rest (cap64) | 2,025.5 | 451.2 | 640.3 | 764.9 | 61.5 / 46.7 | 71.2 |
| 1000 | 8 | reddb-query (cap64) | 1,820.2 | 3,764.0 | 9,041.8 | 13,657.8 | 113.8 / 123.1 | 80.9 |
| 1000 | 8 | reddb-rest (cap64) | 2,911.0 | 2,210.0 | 6,394.8 | 9,751.2 | 79.9 / 119.8 | 71.7 |
| 1000 | 32 | reddb-query (cap64) | 1,785.1 | 14,843.8 | 42,941.8 | 59,378.1 | 111.6 / 130.7 | 85.1 |
| 1000 | 32 | reddb-rest (cap64) | 2,617.5 | 9,513.2 | 31,731.9 | 46,400.8 | 82.3 / 126.7 | 72.9 |
| 10000 | 1 | reddb-query (cap64) | 836.2 | 938.1 | 2,004.6 | 2,411.9 | 77.3 / 31.4 | 103.6 |
| 10000 | 1 | reddb-rest (cap64) | 1,782.0 | 537.4 | 725.5 | 950.1 | 60.1 / 47.5 | 86.5 |
| 10000 | 8 | reddb-query (cap64) | 1,511.3 | 3,927.3 | 12,983.5 | 24,948.8 | 157.1 / 119.6 | 110.1 |
| 10000 | 8 | reddb-rest (cap64) | 2,212.8 | 2,069.5 | 12,363.1 | 23,858.9 | 87.7 / 128.9 | 87.0 |
| 10000 | 32 | reddb-query (cap64) | 1,835.7 | 11,970.4 | 28,993.7 | 40,929.8 | 156.1 / 128.9 | 117.2 |
| 10000 | 32 | reddb-rest (cap64) | 2,404.8 | 9,612.1 | 38,338.7 | 61,066.5 | 79.2 / 125.4 | 90.0 |

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
| 1000 | 1 | reddb-rest (cap64) | 76.7 | 11,628.3 | 22,285.5 | 45,941.3 | 2.7 / 0.9 | 105.4 |
| 1000 | 8 | reddb-rest (cap64) | 102.0 | 75,380.8 | 101,213.7 | 122,354.7 | 14.9 / 5.0 | 122.1 |

## Qualification

HTTP/JSON and RESP are different product paths. Python threads and the two-core client budget can limit throughput. Pipeline-16 latency refers to a complete batch; it is not directly comparable with a single-request percentile. Server order is fixed and the host is shared. These measurements cannot establish maximum engine throughput or ADR 0076 leadership.

See [the experiment contract](../../kv/COMPARE.md), `manifest.json`, `summary.json`, the raw cell JSON and resource JSONL files for setup, unsupported cells, failures and recovery checks.
