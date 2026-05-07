# null: Bench: execute #149 scaffold and fill all 8 workload cells

## Parent

#188

## What to build

Run the Blob Cache benchmark suite scaffolded in #149 against the canonical `make duel-official` config (#154). Fill every `TBD` cell across all 8 result tables in `docs/perf/blob-cache-bench-2026-05-06.md`. Replace `sess-canonical-pending` placeholder with the real session ID.

Steps:
1. Add the dev-deps flagged by #149 (`criterion` or `divan`, `redis` client).
2. Implement the bench harness in `crates/reddb-server/benches/blob_cache_*.rs` exercising BlobCache directly + Redis baseline via the docker setup in `bench/blob-cache/redis-setup.md`.
3. Run all 8 workloads. Capture results.
4. Update the report doc with measured numbers.
5. Produce the SIEVE-vs-W-TinyLFU comparison data point on the mixed-blob workload.

## Acceptance criteria

- [ ] All 8 result tables in the bench report have measured numbers (no `TBD`).
- [ ] Session ID slot replaced with a real session ID.
- [ ] Redis baseline numbers from a documented docker setup.
- [ ] SIEVE vs W-TinyLFU delta documented with decision criterion (≥5pp gap → file W-TinyLFU migration follow-up).
- [ ] Interpretation section concludes: ship as-is / ship with W-TinyLFU migration / more engine work needed.

## Blocked by

- https://github.com/reddb-io/reddb/issues/189

## Progress (2026-05-07)

Steps 1–2 implemented AFK:

- `crates/reddb-server/Cargo.toml`: added `criterion = "0.5"` + `tempfile = "3"`
  to `[dev-dependencies]`; added `[[bench]] name = "blob_cache_bench" harness = false`.
- `crates/reddb-server/benches/blob_cache_bench.rs` (NEW): criterion harness for
  all 8 workloads (w1–w8), RedDB side only. L1 scaled to 8 MiB so the suite runs
  on any host; relative numbers are host-invariant. Redis cells check
  `REDIS_NO_PERSIST_ADDR` / `REDIS_AOF_ADDR` env vars and can be wired in once
  Docker is available.
- `bench/blob-cache/redis-up.sh` (NEW): starts both Redis 7.4 variants per
  redis-setup.md; prints the env var exports needed before running the bench.
- `bench/blob-cache/redis-down.sh` (NEW): stops containers; `--wipe-aof` flag
  to also remove the AOF volume.

Remaining (requires Docker + human run):
- Step 3: run `REDIS_NO_PERSIST_ADDR=... REDIS_AOF_ADDR=... cargo bench -p reddb-server`
- Step 4: fill TBD cells in docs/perf/blob-cache-bench-2026-05-06.md
- Step 5: SIEVE vs W-TinyLFU comparison (w8 at WS=2.0×L1)

