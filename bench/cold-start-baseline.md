# Cold-Start Baseline

Generated: 2026-04-26T01:20:59Z

Host: `Linux 6.17.0-22-generic x86_64`
Toolchain: `rustc 1.95.0 (59807616e 2026-04-14)`

Source: `examples/cold_start_bench.rs` driven by `scripts/cold-start-bench.sh`.
Iterations per cell: 20 (+ 2 warmup discarded).

## PLAN.md B1 targets

- `warm`: open P95 < **2000 ms** (data dir present, fresh process).
- `cold_remote`: open P95 < **10000 ms** (empty data dir, restore-from-remote, 1 GB DB).

## Scenario: `warm` — data dir present, fresh process

| size MB | open p50 ms | open p95 ms | open p99 ms | total p50 ms | total p95 ms | total p99 ms | restore p50 ms | restore p95 ms |
|--------:|-----------:|-----------:|-----------:|------------:|------------:|------------:|--------------:|--------------:|
