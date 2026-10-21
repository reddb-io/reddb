# Checked RQL and SQL read paths

`read_paths.py` measures persistent HTTP `/query` requests with result caching
disabled. It validates every returned value, row count and ordering before
counting a completed request. SQL point reads use a hash index; setup retains
the actual `EXPLAIN` response. The other SQL cases return 100 or 1,000 ordered
rows with two selected fields. KV values are deterministic 100-byte strings.

The default dataset has 10,000 KV keys and 10,000 SQL rows. Prepare the KV
collection `kv_diag_10000` with `bench/kv/compare.py --prepare-only`, then run:

```sh
python3 bench/query/read_paths.py --host SERVER --setup --out setup.json
python3 bench/query/read_paths.py --host SERVER --case kv --workers 1 --out kv.json
python3 bench/query/read_paths.py --host SERVER --case sql-point --workers 8 --out point.json
python3 bench/query/read_paths.py --host SERVER --case sql-1000 --out rows.json
python3 bench/query/read_paths.py --host SERVER --verify-only --out verification.json
python3 -m unittest discover -s bench/query -p test_read_paths.py -v
```

Use equivalent engine, dataset, durability and cache settings on both sides.
The SQL table is named `query_perf`; setup deliberately fails if it already
exists. Verification checks all 10,000 rows. An invalid response persists an
invalid artifact and exits with an error. Five two-second rounds reuse one
process; they are not five independent engine runs. Request completion latency
includes Python JSON decoding and validation, and the Python/GIL client can
limit throughput.

`local_matrix.py` drives pre-created Docker containers. Mount this repository's
`bench` directory at `/bench:ro` in the client and the chosen output directory
at `/results`. Provide before/after server names, a Python client container,
engine revisions and binary SHA256s. It runs before/after/after/before, with
1/8/32 connections for KV and SQL point reads, and one for multirow reads.
After each process run, it kills/reopens the server and verifies the entire
SQL and KV datasets. It saves container identities, limits and resource traces.

```sh
python3 bench/query/local_matrix.py --client CLIENT --before BEFORE --after AFTER \
  --before-revision BEFORE_SHA --after-revision AFTER_SHA \
  --before-sha256 BEFORE_BINARY_HASH --after-sha256 AFTER_BINARY_HASH --out RESULTS
python3 bench/query/summarize.py RESULTS
```

`--prepared-data` checks existing SQL and KV datasets before sampling rather
than creating SQL data again. Use a fresh output directory for a new experiment.
`--resume` preserves completed cells and the original failure manifest; it
refuses changed engine identities, changed measured workload or replacement of
an invalid cell. It was introduced to repair a missing verifier port in the
first capture, whose error and original measurements remain available.

Summaries retain all process samples and unfavorable cells. They aggregate
round medians within a process, then two process medians per version. No
confidence interval or competitive capacity claim is inferred from this small
shared-host sample. Resource statistics cover warmup and verification too.
Normalize text artifacts to LF before generating hashes; CI verifies the
committed bytes and exercises response-validation and syscall-parser guards.

`write_profile.py` is a separate syscall diagnostic. Its server needs `strace`,
`SYS_PTRACE`, `seccomp=unconfined` and a writable `/results` mount. It attaches
only to the isolated server PID 1, captures `fsync`/`fdatasync`, checks every
write/readback and verifies persisted values after process termination. The
GET control should issue no syncs. `--stack-traces` records eight writes with
userspace stacks. Interrupted calls are joined by thread; failed or incomplete
traces invalidate the profile. Traced timings include instrumentation overhead
and are not throughput results. Supply `--engine-revision` and `--binary-sha256`
for the exact binary being profiled; identity is never inferred from a prior run.

The dated report in `docs/perf/rql-sql-projection-2026-10-20.md` records the
experiments, engine hashes, limits, negative results and remaining write work.
