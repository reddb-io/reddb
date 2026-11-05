# Local KV comparison — verified product paths

Engine revision: `a3af4d6261f05fcc2dccd5caca178c5ddf6936a3`. Classification: **shared-host diagnostic**.

The table contains medians of five round-level metrics, not pooled request percentiles. All raw round samples and their ranges remain in the JSON. CPU percentages use 100% per core; memory is sampled cgroup working set, not per-key memory or RSS. Missing telemetry is unavailable.

## GET without pipelining

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 7,999.1 | 119.9 | 140.9 | 161.6 | 43.6 / 62.1 | 4.6 |
| 1000 | 1 | reddb-query (cap64) | 1,754.7 | 558.5 | 616.5 | 668.9 | 77.6 / 40.9 | 85.2 |
| 1000 | 1 | reddb-rest (cap64) | 1,201.4 | 818.1 | 895.1 | 1,041.8 | 81.8 / 23.1 | 71.5 |
| 1000 | 1 | reddb-rest | 759.2 | 1,128.3 | 2,045.5 | 4,229.3 | 84.5 / 20.7 | 71.4 |
| 1000 | 1 | redis | 5,023.8 | 192.8 | 223.3 | 251.2 | unavailable / unavailable | unavailable |
| 1000 | 8 | kividb | 9,207.7 | 735.5 | 1,950.5 | 2,731.6 | 36.3 / 144.3 | 5.1 |
| 1000 | 8 | reddb-query (cap64) | 3,068.9 | 2,415.4 | 4,658.8 | 6,068.6 | 112.3 / 124.9 | 85.3 |
| 1000 | 8 | reddb-rest (cap64) | 4,003.8 | 1,838.2 | 3,387.4 | 4,451.5 | 305.0 / 118.1 | 76.8 |
| 1000 | 8 | reddb-rest | 1,947.7 | 2,655.9 | 12,329.4 | 17,971.2 | 301.1 / 90.3 | 76.3 |
| 1000 | 8 | redis | 8,570.7 | 793.8 | 2,007.2 | 2,776.7 | unavailable / unavailable | unavailable |
| 1000 | 32 | kividb | 9,715.5 | 2,768.1 | 7,748.6 | 10,596.8 | 42.2 / 154.8 | 5.9 |
| 1000 | 32 | reddb-query (cap64) | 3,414.5 | 8,544.5 | 18,693.1 | 24,668.8 | 121.8 / 130.3 | 85.5 |
| 1000 | 32 | reddb-rest (cap64) | 4,384.0 | 6,103.1 | 16,693.7 | 23,270.6 | 330.4 / 126.0 | 79.2 |
| 1000 | 32 | reddb-rest | invalid | — | — | — | — | — |
| 1000 | 32 | redis | 9,990.9 | 2,733.4 | 7,352.8 | 10,282.3 | unavailable / unavailable | unavailable |
| 10000 | 1 | kividb | 5,224.8 | 161.9 | 268.5 | 757.3 | 42.6 / 58.0 | 6.3 |
| 10000 | 1 | reddb-query (cap64) | 1,173.0 | 828.2 | 1,257.1 | 1,629.5 | 80.2 / 34.4 | 114.6 |
| 10000 | 1 | reddb-rest (cap64) | 95.9 | 10,037.7 | 13,332.6 | 17,158.3 | 96.5 / 4.0 | 94.8 |
| 10000 | 1 | redis | 4,902.4 | 195.6 | 232.5 | 262.3 | unavailable / 0.0 | 8.1 |
| 10000 | 8 | kividb | 7,346.1 | 827.6 | 2,633.3 | 4,897.8 | 40.9 / 141.9 | 6.5 |
| 10000 | 8 | reddb-query (cap64) | 2,133.6 | 3,039.9 | 9,078.7 | 15,335.5 | 150.2 / 122.3 | 120.2 |
| 10000 | 8 | reddb-rest (cap64) | 235.8 | 29,423.6 | 67,923.2 | 81,055.4 | 384.6 / 12.2 | 138.9 |
| 10000 | 8 | redis | 8,068.8 | 838.6 | 2,159.1 | 2,978.8 | 81.6 / 142.1 | 8.9 |
| 10000 | 32 | kividb | 8,522.7 | 2,952.4 | 9,553.0 | 15,143.7 | 43.8 / 151.6 | 7.3 |
| 10000 | 32 | reddb-query (cap64) | 2,760.5 | 10,366.7 | 23,971.8 | 33,262.0 | 159.8 / 130.3 | 120.5 |
| 10000 | 32 | reddb-rest (cap64) | 304.5 | 98,782.1 | 172,436.2 | 233,354.9 | 393.0 / 13.3 | 278.9 |
| 10000 | 32 | redis | 9,837.3 | 2,798.5 | 7,393.2 | 10,258.9 | 89.3 / 154.8 | 9.4 |

## RESP GET, pipeline 16 — batch completion latency

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 85,067.0 | 178.8 | 204.7 | 231.5 | 40.3 / 71.4 | 5.1 |
| 1000 | 1 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 1 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 1000 | 1 | reddb-rest | unsupported | — | — | — | — | — |
| 1000 | 1 | redis | 60,253.9 | 255.0 | 290.7 | 323.1 | unavailable / unavailable | unavailable |
| 1000 | 8 | kividb | 88,868.2 | 1,224.8 | 3,150.1 | 4,316.3 | 29.6 / 129.0 | 5.5 |
| 1000 | 8 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 8 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 1000 | 8 | reddb-rest | unsupported | — | — | — | — | — |
| 1000 | 8 | redis | 89,082.0 | 1,222.7 | 3,118.3 | 4,280.1 | unavailable / unavailable | unavailable |
| 1000 | 32 | kividb | 57,009.6 | 6,936.1 | 22,729.0 | 36,958.2 | 34.1 / 135.0 | 5.9 |
| 1000 | 32 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 1000 | 32 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 1000 | 32 | redis | 100,220.7 | 4,191.5 | 11,491.2 | 15,904.9 | unavailable / unavailable | unavailable |
| 10000 | 1 | kividb | 66,291.4 | 199.3 | 310.9 | 529.5 | 40.2 / 67.8 | 6.6 |
| 10000 | 1 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 1 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 1 | redis | 54,493.3 | 274.6 | 374.5 | 428.5 | 54.0 / 51.2 | 8.6 |
| 10000 | 8 | kividb | 69,889.6 | 1,461.5 | 4,218.2 | 6,667.3 | 31.8 / 128.9 | 6.9 |
| 10000 | 8 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 8 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 8 | redis | 81,544.0 | 1,340.0 | 3,446.0 | 4,681.9 | 67.5 / 129.4 | 9.1 |
| 10000 | 32 | kividb | 82,111.0 | 5,017.3 | 15,066.4 | 23,249.1 | 36.6 / 135.7 | 7.9 |
| 10000 | 32 | reddb-query (cap64) | unsupported | — | — | — | — | — |
| 10000 | 32 | reddb-rest (cap64) | unsupported | — | — | — | — | — |
| 10000 | 32 | redis | 95,015.3 | 4,515.3 | 11,964.0 | 16,633.4 | 79.2 / 137.1 | 9.6 |

## Durable overwrites — KiviDB includes SET + WAITAOF

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 195.1 | 4,464.2 | 6,822.1 | 12,759.0 | 6.4 / 3.7 | 6.9 |
| 1000 | 1 | reddb-rest (cap64) | 109.2 | 8,508.7 | 10,582.9 | 16,467.3 | 9.9 / 3.9 | 103.9 |
| 1000 | 1 | reddb-rest (sync) | 121.8 | 7,925.0 | 9,434.9 | 10,594.8 | 8.2 / 3.1 | 80.0 |
| 1000 | 1 | redis | 334.1 | 2,808.9 | 3,891.0 | 4,251.9 | 12.1 / 7.1 | 8.3 |
| 1000 | 8 | kividb | 1,425.8 | 5,192.0 | 7,919.8 | 8,760.1 | 18.5 / 38.0 | 9.4 |
| 1000 | 8 | reddb-rest (cap64) | 125.6 | 62,072.4 | 71,431.1 | 103,526.5 | 11.4 / 3.4 | 116.6 |
| 1000 | 8 | reddb-rest (sync) | 126.2 | 62,189.0 | 67,993.7 | 77,830.7 | 9.8 / 3.3 | 84.6 |
| 1000 | 8 | redis | 1,703.9 | 4,125.4 | 7,008.6 | 9,967.8 | 28.5 / 36.8 | 9.0 |

## Qualification

HTTP/JSON and RESP are different product paths. Python threads and the two-core client budget can limit throughput. Pipeline-16 latency refers to a complete batch; it is not directly comparable with a single-request percentile. Server order is fixed and the host is shared. These measurements cannot establish maximum engine throughput or ADR 0076 leadership.

See [the experiment contract](../../kv/COMPARE.md), `manifest.json`, `summary.json`, the raw cell JSON and resource JSONL files for setup, unsupported cells, failures and recovery checks.
