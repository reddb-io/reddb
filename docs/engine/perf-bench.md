# Performance bench — reproduction guide

How to run the mini-duel benchmark (`benches/bench_definitive_dual.py`)
that compares RedDB against Postgres across 28 scenarios in a local
Docker Compose environment, and how to interpret the output.

The guide accompanies the perf-parity push planned in
`docs/spec-performance-parity-2026-04-17.md` and `tasks/plan.md`.
Every perf-focused commit records its delta against the baseline
captured with this setup.

> **Note — numbers below are the 04-17 snapshot.** Several 04-20/04-21
> perf landings are not yet re-measured here: streaming bulk wire
> protocol (~3× typed_insert), columnar pre-validated insert, CDC
> split lock, lock-free WAL append queue, batched bulk WAL actions,
> B-tree right-sibling hop on sorted bulk insert, parallel row
> serialize with batched id reservation, and wire encode column-index
> caching. Re-run the harness against `main` to refresh — follow-up.

## Prerequisites

- Docker ≥ 24 with Buildx (`docker buildx version`)
- `python3` ≥ 3.10
- Local Postgres client libs (for the harness's `psycopg` import): on
  Debian-flavoured hosts, `apt install libpq-dev`; on macOS,
  `brew install libpq`
- Idle machine — bench is sensitive to CPU / IO contention. Close
  other Docker workloads.

## Quick start

```bash
# 1. Build the RedDB image (≈ 7-9 min first time; incremental rebuilds
#    hit the Cargo dependency cache layer).
docker build -f Dockerfile -t reddb:latest .

# 2. Bring up containers. Bench expects the RedDB binary on the host
#    at `target/release/red` — see "Bench binary vs image" below.
cargo build --release --bin red

# 3. Run the 28-scenario mini-duel. Numbers land in stdout + a JSON
#    dump next to the script.
python3 benches/bench_definitive_dual.py --output benches/latest.json
```

A clean run produces two blocks: Report A (standard types) and Report B
(optimised types). Each block lists 19 scenarios per side (RedDB
"standard" run1 + Postgres for comparison). Failures print the exact
error line. A trailing summary compares gaps.

## Tuning surface

The RedDB image ships with "opinionated but safe" defaults. Every
Tier A config matrix key self-heals on first boot — see
[`config-matrix.md`](./config-matrix.md) when it lands. Operators
override knobs via:

### Environment variables

Map: `REDDB_<matrix_key_with_dots_as_underscores_uppercase>`.

```bash
# Durability: sync (default) vs async
docker run -e REDDB_DURABILITY_MODE=async reddb:latest server

# Fast-swap per-collection locking off (emergency rollback to
# the pre-Phase-1 global-mutex behaviour)
docker run -e REDDB_CONCURRENCY_LOCKING_ENABLED=false reddb:latest

# Tune group-commit flush cadence
docker run -e REDDB_STORAGE_WAL_MAX_INTERVAL_MS=5 reddb:latest
```

Env vars are in-memory only — re-read every boot, never written to
`red_config`. Restart without the env var to revert.

### Mounted config file

Drop a JSON object at `/etc/reddb/config.json` (override path via
`REDDB_CONFIG_FILE=…`). Keys land in `red_config` with
write-if-absent semantics, so a later `SET CONFIG` by the user always
wins and the matrix's Tier A self-heal isn't overwritten.

```json
{
  "durability": { "mode": "sync" },
  "storage": {
    "wal": { "max_interval_ms": 10, "min_batch_size": 4 },
    "bgwriter": { "delay_ms": 200 },
    "bulk_insert": { "max_buffered_rows": 2000 }
  }
}
```

```bash
docker run -v $(pwd)/my-config.json:/etc/reddb/config.json reddb:latest
```

Missing file = silent no-op. Malformed file logs a warning and is
ignored; boot never fails on a bad overlay file.

### SET CONFIG at runtime

The classic path — writes directly into `red_config`, persists across
restarts, lowest priority vs env but higher than matrix default:

```sql
SET CONFIG storage.bulk_insert.max_buffered_rows = 2000;
SHOW CONFIG storage.bulk_insert.max_buffered_rows;
```

## Precedence

Highest wins:

1. `REDDB_<KEY>` env var (in-memory, per-boot)
2. `/etc/reddb/config.json` file overlay (persisted via
   write-if-absent)
3. `SET CONFIG` persisted value
4. Matrix default (Tier A self-heals on boot, Tier B in-memory default)

## Baseline reference

Contaminated baseline (pre-`b1d22e3` red_stats fix, 28 scenarios, one
run on a quiet developer workstation):

| Scenario | PG ops/s | RedDB ops/s | Gap |
|----------|---------:|------------:|----:|
| insert_bulk | 86,782 | 8,474 | 10.2× |
| insert_sequential | 1,443 | 336 | 4.3× |
| bulk_update | 45,596 | 943 | 48× |
| select_range | 114 | 9 | 12.7× |
| select_complex | 1,085 | 55 | 19.7× |
| concurrent | 6,523 | 98 | 66× |

Read as "PG is Nx faster than RedDB on this scenario at this point in
time". Target after the full perf-parity push is **all scenarios at
≤ 1.5×**. See the spec for the reasoning.

7/28 scenarios failed on `grpc BulkInsertBinary` before `b1d22e3`
landed — that fix's origin is what triggered this guide being written.

## Diagnosing a new regression

1. **Is it a functional failure?** If the bench script prints
   `grpc BulkInsertBinary: code=Internal` or equivalent, the scenario
   never ran — the "ops/sec" is noise. Capture the gRPC error first.
   Common culprit: stale state in `red_stats` from a prior run; drop
   the container's `/data` volume and retry.

2. **Is the regression in ops/s or latency?** p50 moves in lockstep
   with ops/s for most scenarios; if they diverge, you're looking at
   tail-latency drift (batching, background flush, bgwriter cadence).
   Dig into `SHOW CONFIG storage.bgwriter.*` and
   `storage.wal.max_interval_ms`.

3. **Bisect the recent commits.** Every perf-focused commit in the
   parity push records its delta in the message. A sudden
   regression will correlate with a commit; revert to confirm.

4. **Is the host contended?** `docker stats` during the bench —
   another container pegging disk or CPU poisons the numbers.
   Re-run in isolation before filing a regression.

## Bench binary vs image

The current bench script launches the server from the host path
`target/release/red`, not from the Docker image. That keeps the bench
fast to iterate on without a rebuild-and-`docker run` cycle. The
Docker image is the shipping artefact operators consume; the host
binary is the developer workflow.

Both paths read the same env vars and config file mechanism — a
config change verified on the host binary behaves identically in the
image. If you need to bench the image end-to-end (including any
container-specific overhead), point the harness's `REDDB` constant at
`docker exec reddb …` or adapt the script to speak gRPC over the
exposed port.

## Files

- `benches/bench_definitive_dual.py` — the harness.
- `Dockerfile` — the image. Perf-parity defaults land through the
  matrix (`src/runtime/config_matrix.rs`), not via explicit `ENV`
  lines in the Dockerfile.
- `docs/spec-performance-parity-2026-04-17.md` — why we care.
- `tasks/plan.md` — the phased attack.
