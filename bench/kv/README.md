# KV benchmark suite

For verified comparative diagnostics, use [COMPARE.md](COMPARE.md). The legacy
runner below remains a historical regression fixture: it opens new HTTP
connections while reusing RESP connections and does not validate returned values.
Its cross-engine ratios cannot establish comparative performance.

This suite pins the normal-KV operational benchmark contract from issue #253.
It compares RedDB and Redis on the same `PUT`/`GET`/`INCR` workload and measures
RedDB `WATCH` delivery lag.

## Prerequisites

- a RedDB server listening at `REDDB_URL` (default `http://127.0.0.1:5000`)
- a local Redis server at `REDIS_HOST:REDIS_PORT` (default `127.0.0.1:6379`)
- Python 3.10+

The benchmark intentionally excludes Config and Vault; it uses only a normal KV
collection named `kv_bench`.

## Run

```bash
bench/kv/run.sh
```

Useful knobs:

```bash
KV_BENCH_OPS=10000 \
KV_BENCH_WATCH_EVENTS=100000 \
REDDB_URL=http://127.0.0.1:5000 \
REDIS_HOST=127.0.0.1 \
REDIS_PORT=6379 \
bench/kv/run.sh
```

The single-sample result JSON is written to `bench/results/kv-latest.json`
unless `KV_BENCH_OUT` is set. The release gate runs three samples against
fresh RedDB files, preserves every raw result, and aggregates their medians and
coefficient of variation with `aggregate.py`.

## Regression gate

```bash
bench/kv/check-regression.py \
  bench/results/kv-release-baseline.json \
  bench/results/kv-latest.json
```

The default gate compares per-sample RedDB/Redis ratios and fails when median
normalized p99 regresses by more than 20%, normalized throughput regresses by
more than 20%, or either ratio has a coefficient of variation above 0.50. The
same-run Redis reference controls for shared-runner contention; it is not a
claim of cross-engine parity. Override the thresholds with
`KV_BENCH_P99_REGRESSION_THRESHOLD`,
`KV_BENCH_THROUGHPUT_REGRESSION_THRESHOLD`, and
`KV_BENCH_NORMALIZED_CV_MAX`.

Every WATCH sample must receive at least one change event per requested write
with zero drops. An update can legitimately emit both delete and update events
for its version transition. The median `watch_delivery_lag.p99_ms` must stay below
`KV_BENCH_WATCH_P99_TARGET_MS` (default `10`).
