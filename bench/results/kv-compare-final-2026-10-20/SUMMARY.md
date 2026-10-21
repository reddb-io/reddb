# Local KV comparison — verified product paths

Engine revision: `fae39eb6d07c32f8bc734973f10a9349a8e56a45`. Classification: **shared-host diagnostic**.

The table contains medians of five round-level metrics, not pooled request percentiles. All raw round samples and their ranges remain in the JSON. CPU percentages use 100% per core; memory is sampled cgroup working set, not per-key memory or RSS. Missing telemetry is unavailable.

## GET without pipelining

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 4,769.7 | 171.0 | 268.4 | 763.6 | 40.0 / 59.5 | 4.9 |
| 1000 | 1 | reddb-query | 1,868.1 | 518.7 | 623.9 | 721.2 | 74.2 / 45.6 | 77.2 |
| 1000 | 1 | reddb-rest | 2,462.3 | 386.6 | 512.2 | 623.0 | 62.7 / 47.1 | 71.0 |
| 1000 | 1 | redis | 4,767.5 | 197.6 | 253.3 | 303.6 | 60.0 / 41.1 | 7.2 |
| 1000 | 8 | kividb | 6,636.4 | 881.5 | 3,059.9 | 6,128.2 | 40.6 / 140.3 | 5.2 |
| 1000 | 8 | reddb-query | 2,648.9 | 2,754.2 | 5,602.1 | 7,083.6 | 93.6 / 125.1 | 77.8 |
| 1000 | 8 | reddb-rest | 3,990.1 | 1,694.9 | 4,406.1 | 6,294.7 | 81.5 / 120.7 | 71.1 |
| 1000 | 8 | redis | 7,872.6 | 865.2 | 2,190.5 | 3,005.3 | 80.0 / 143.8 | 7.3 |
| 1000 | 32 | kividb | 8,646.1 | 3,088.2 | 8,737.7 | 12,258.2 | 41.7 / 153.1 | 6.3 |
| 1000 | 32 | reddb-query | 1,092.6 | 24,049.6 | 70,866.2 | 99,238.1 | 70.1 / 129.6 | 81.0 |
| 1000 | 32 | reddb-rest | 4,167.7 | 6,268.9 | 18,501.5 | 25,857.3 | 87.7 / 123.4 | 72.1 |
| 1000 | 32 | redis | 2,865.6 | 3,652.8 | 42,738.3 | 64,123.9 | 81.5 / 154.3 | 7.5 |
| 10000 | 1 | kividb | 2,759.5 | 175.9 | 242.8 | 8,471.5 | 51.3 / 60.7 | 6.3 |
| 10000 | 1 | reddb-query | 1,947.3 | 502.7 | 555.6 | 613.3 | 74.6 / 45.5 | 97.3 |
| 10000 | 1 | reddb-rest | 2,470.6 | 391.2 | 455.6 | 515.4 | 63.6 / 46.3 | 89.2 |
| 10000 | 1 | redis | 4,887.7 | 197.0 | 232.7 | 259.4 | 59.0 / 40.8 | 9.1 |
| 10000 | 8 | kividb | 5,294.3 | 1,063.5 | 3,804.3 | 8,691.9 | 41.9 / 140.2 | 6.7 |
| 10000 | 8 | reddb-query | 2,966.3 | 2,508.3 | 4,875.5 | 6,318.7 | 100.1 / 125.2 | 104.9 |
| 10000 | 8 | reddb-rest | 4,033.8 | 1,665.4 | 4,410.5 | 6,085.2 | 82.6 / 121.2 | 89.7 |
| 10000 | 8 | redis | 8,233.1 | 818.4 | 2,111.9 | 2,953.7 | 81.2 / 143.3 | 9.2 |
| 10000 | 32 | kividb | 9,564.6 | 2,818.3 | 7,898.4 | 10,929.9 | 42.1 / 153.5 | 7.9 |
| 10000 | 32 | reddb-query | 3,410.3 | 8,546.3 | 18,581.8 | 24,433.3 | 107.2 / 130.7 | 107.3 |
| 10000 | 32 | reddb-rest | 4,358.7 | 5,855.7 | 17,383.8 | 25,145.4 | 89.7 / 124.4 | 90.3 |
| 10000 | 32 | redis | 8,156.4 | 3,309.3 | 9,115.1 | 12,740.3 | 87.2 / 154.0 | 9.7 |

## RESP GET, pipeline 16 — batch completion latency

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 49,716.0 | 273.3 | 396.8 | 903.3 | 34.6 / 69.1 | 5.4 |
| 1000 | 1 | reddb-query | unsupported | — | — | — | — | — |
| 1000 | 1 | reddb-rest | unsupported | — | — | — | — | — |
| 1000 | 1 | redis | 54,752.6 | 276.7 | 346.4 | 405.5 | 53.3 / 50.9 | 7.4 |
| 1000 | 8 | kividb | 75,976.6 | 1,412.6 | 3,741.8 | 5,282.3 | 30.4 / 127.9 | 5.7 |
| 1000 | 8 | reddb-query | unsupported | — | — | — | — | — |
| 1000 | 8 | reddb-rest | unsupported | — | — | — | — | — |
| 1000 | 8 | redis | 28,996.2 | 1,593.4 | 15,945.3 | 32,765.7 | 59.8 / 131.3 | 7.7 |
| 1000 | 32 | kividb | 92,558.9 | 4,679.9 | 12,231.6 | 16,853.2 | 34.6 / 137.3 | 6.6 |
| 1000 | 32 | reddb-query | unsupported | — | — | — | — | — |
| 1000 | 32 | reddb-rest | unsupported | — | — | — | — | — |
| 1000 | 32 | redis | 25,960.0 | 14,981.9 | 56,781.3 | 78,817.8 | 53.8 / 137.6 | 8.0 |
| 10000 | 1 | kividb | 25,299.6 | 288.0 | 440.4 | 9,794.8 | 40.5 / 70.8 | 6.7 |
| 10000 | 1 | reddb-query | unsupported | — | — | — | — | — |
| 10000 | 1 | reddb-rest | unsupported | — | — | — | — | — |
| 10000 | 1 | redis | 59,030.8 | 258.4 | 305.9 | 347.7 | 54.0 / 51.9 | 9.4 |
| 10000 | 8 | kividb | 71,869.2 | 1,483.9 | 3,952.0 | 5,582.9 | 34.7 / 129.0 | 7.2 |
| 10000 | 8 | reddb-query | unsupported | — | — | — | — | — |
| 10000 | 8 | reddb-rest | unsupported | — | — | — | — | — |
| 10000 | 8 | redis | 73,933.5 | 1,453.1 | 3,767.9 | 5,112.4 | 66.5 / 128.8 | 9.2 |
| 10000 | 32 | kividb | 101,130.3 | 4,293.6 | 11,133.0 | 15,293.2 | 35.1 / 138.1 | 8.1 |
| 10000 | 32 | reddb-query | unsupported | — | — | — | — | — |
| 10000 | 32 | reddb-rest | unsupported | — | — | — | — | — |
| 10000 | 32 | redis | 81,669.7 | 5,295.3 | 14,256.1 | 19,787.9 | 76.4 / 136.6 | 9.5 |

## Durable overwrites — KiviDB includes SET + WAITAOF

| Keys | Connections | Product path | Ops/s | p50 µs | p95 µs | p99 µs | Server/client CPU % | Server MiB |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: |
| 1000 | 1 | kividb | 222.0 | 4,378.9 | 4,528.4 | 6,477.0 | 7.0 / 4.1 | 7.5 |
| 1000 | 1 | reddb-rest (grouped-matched) | 144.1 | 4,754.7 | 15,296.2 | 16,579.2 | 10.6 / 4.9 | 104.5 |
| 1000 | 1 | reddb-rest (grouped) | 148.5 | 4,671.9 | 14,652.4 | 16,641.5 | 8.8 / 4.0 | 76.3 |
| 1000 | 1 | reddb-rest | 91.7 | 5,087.5 | 16,133.4 | 74,723.5 | 7.6 / 3.6 | 96.5 |
| 1000 | 1 | redis | 174.1 | 4,983.8 | 10,395.4 | 30,976.1 | 5.7 / 3.3 | 9.0 |
| 1000 | 8 | kividb | 2,199.5 | 3,424.2 | 5,383.5 | 5,969.5 | 12.1 / 47.9 | 9.2 |
| 1000 | 8 | reddb-rest (grouped-matched) | 205.3 | 8,666.4 | 141,285.7 | 731,882.0 | 19.7 / 8.6 | 119.0 |
| 1000 | 8 | reddb-rest (grouped) | 519.3 | 9,139.0 | 29,114.8 | 103,440.3 | 38.2 / 33.7 | 79.0 |
| 1000 | 8 | reddb-rest | 242.8 | 31,272.6 | 39,049.4 | 60,444.7 | 20.0 / 14.1 | 96.6 |
| 1000 | 8 | redis | 2,529.2 | 2,912.1 | 4,425.4 | 7,876.6 | 15.9 / 28.5 | 9.3 |

## Qualification

HTTP/JSON and RESP are different product paths. Python threads and the two-core client budget can limit throughput. Pipeline-16 latency refers to a complete batch; it is not directly comparable with a single-request percentile. Server order is fixed and the host is shared. These measurements cannot establish maximum engine throughput or ADR 0076 leadership.

See [the experiment contract](../../kv/COMPARE.md), `manifest.json`, `summary.json`, the raw cell JSON and resource JSONL files for setup, unsupported cells, failures and recovery checks.
