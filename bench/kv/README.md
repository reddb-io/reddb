# KV benchmark suite

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

The result JSON is written to `bench/results/kv-latest.json` unless
`KV_BENCH_OUT` is set.

## Regression gate

```bash
bench/kv/check-regression.py \
  bench/results/kv-release-baseline.json \
  bench/results/kv-latest.json
```

The default gate fails when any RedDB `p99_us` is more than 20% above the
baseline. Override with `KV_BENCH_P99_REGRESSION_THRESHOLD`, for example `0.15`.
The WATCH target is also enforced: `watch_delivery_lag.p99_ms` must stay below
`KV_BENCH_WATCH_P99_TARGET_MS` (default `10`).
