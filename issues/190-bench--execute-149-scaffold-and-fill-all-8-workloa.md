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

