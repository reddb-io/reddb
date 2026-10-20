# RQL model validation, read projection and write diagnosis — 2026-10-20

RQL point commands previously built a complete logical catalog snapshot just
to check the target collection's model. `snapshot_store_with_declarations`
calls `collection_observations` for every collection, visiting every stored
entity even when a declared model is authoritative. An indexed KV lookup was
therefore preceded by work proportional to the entire database.

The runtime now borrows the target's cached contract and validates its declared
model directly when the physical collection exists. Undeclared legacy
collections and orphan contracts retain the existing snapshot fallback. Vault
access validation still runs first, model mismatch still precedes authorization,
and subsequent access-policy and MVCC checks remain in place. A changed contract
is observed through the existing invalidated contract cache. The optimization
also applies to other point commands dispatched through this model validator;
only KV GET is timed here.

In the repeated local capture, 10,000-key RQL KV GET with one connection
changed from 1,302 to 1,940 queries/s (+49%), with median round p99 from
1.069 to 0.609 ms (-43%). Higher-concurrency improvements were smaller, and
the eight-connection p99 worsened slightly. SQL showed no consistent gain.

RQL KV GET additionally projects the value, physical ID and timestamps under
the indexed storage read guard, avoiding an owned copy of unreturned row data.
Query response selection borrows unfiltered record slices, including empty
filter lists. Filtered responses retain their owned selection; resumable
streams clone before sorting so the source result is not mutated. The nine
KV response fields, tags and missing-key envelope are preserved.

Plain SQL SELECT does not enter the model-validation branch changed here.
Its only production change in this patch is borrowing the unfiltered response
selection. The SQL controls therefore help expose whole-experiment variation;
they do not establish a SQL executor improvement. Further SQL optimization
requires profiling execution and materialization separately from presentation.

Cost sketch: warm declared-model validation changes from a database-wide
catalog/row walk with allocation to two target map lookups and Arc increments.
The legacy fallback remains expensive. KV projection copies returned data,
not unrelated fields. Unfiltered response selection becomes a borrowed slice
with no row clones; this does not remove SQL execution, sorting, JSON encoding,
HTTP or returned-value allocation costs. No WAL, durability or file format
change is included.

## Experiments and limits

These are shared-host RedDB before/after diagnostics, not a KiviDB/Redis
comparison or a competitive leadership claim. The host is a Ryzen 5 3600 with
a Docker/WSL2 Linux VM configured with 12 CPUs and 20 GiB. Each isolated server
has CPUs 0–3, four CPU quota, 2 GiB memory limit, a 1 GiB engine budget, four
workers, `--http-max-handlers 64`, strict durability and disabled result cache.
The persistent HTTP Python 3.12 client has CPUs 4–5, two CPU quota and 1 GiB.
Container image IDs, actual environment, commands and mounts are in manifests.
The unrelated host workload is uncontrolled. Builds and syscall profiling did
not run concurrently with timed query captures.

Both versions use equivalent separate volumes with 10,000 KV keys, 100-byte
deterministic values, and a `query_perf` table with 10,000 rows, two selected
columns and an unused 128-character column. Setup creates a hash index on
`seq`; retained EXPLAIN output selects `index_seek` for the SQL point query.
This planner output does not prove the complete executor avoids other work.
The separate runtime probe's `rows_scanned: 0`/`exec_time_us: 0` counters are
incomplete telemetry and are not treated as evidence of zero work.

Each experiment uses before/after/after/before process order, with five
two-second rounds per cell and ten warmup requests per connection. KV and SQL
point reads use 1/8/32 connections; SQL queries returning 100/1,000 ordered rows
use one. Every returned value, row count and order is checked. After each of
four process runs, SIGKILL/reopen is followed by full 10,000-row SQL and
10,000-key KV verification. A process crash does not test a power failure.

Reported aggregates are medians of five round statistics within each process,
then the median of two process samples per version. All raw rounds, process
ranges, p50/p95/p99, errors, CPU, working set and I/O endpoints are retained.
Two process samples are insufficient for a useful leadership confidence
interval. Python decoding, validation and the GIL can bound the observed
throughput; these figures are not server capacity estimates. Resource samples
cover the whole cell, including warmups and verification.

Optimized builds use Rust 1.95.0 and
`cargo build --release --locked --bin red -j 4`:

| Version | Runtime source revision | Binary SHA256 |
| --- | --- | --- |
| REST-index baseline | `5ad0766eda49f7472bf85ee91524a17b3ec10673` | `2ac44ed0bd18f5bb8c3244724cd9a6825dd0bda712896dd201c8fafc1c7653e2` |
| Projection only | `00bb6996ad6addfe8beffd95756b83398af3d92f` | `f119570781ec7d2f68e18b80ba1836ad13a1b5846eb765b10126093b2d801c1f` |
| Projection and catalog fast path | `8eb7f554bf6e537755cc3341872dac278f8391e7` | `44804df62e0f003ed5948f914a0159ae378e7d00af254e6a88cbb46cb6b91718` |

Subsequent test-only corrections do not alter runtime source. The original
baseline already includes the prior indexed REST and KV projection APIs; it
is not an unmodified released server. The later catalog experiment reuses the
same verified read-only datasets and independently repeats both versions.

## Initial projection-only result

Avoiding copies alone did **not** produce an HTTP throughput improvement.
Keeping this negative result led to finding the catalog scan. All values below
are from the original preserved capture, rather than substituted reruns.

| Query | Connections | Before queries/s | Projection queries/s | Before p99 ms | Projection p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| KV point | 1 | 1,289.93 | 1,286.23 | 1.089 | 1.241 |
| KV point | 8 | 3,095.17 | 3,092.78 | 5.404 | 5.439 |
| KV point | 32 | 3,373.75 | 3,359.31 | 24.754 | 24.950 |
| SQL point | 1 | 364.42 | 362.34 | 3.809 | 3.986 |
| SQL point | 8 | 1,147.43 | 1,114.71 | 14.326 | 15.569 |
| SQL point | 32 | 1,369.68 | 1,351.83 | 65.214 | 65.138 |
| SQL 100 rows | 1 | 48.71 | 47.69 | 27.346 | 25.928 |
| SQL 1,000 rows | 1 | 32.19 | 32.33 | 37.003 | 34.076 |

The wrapper initially omitted the required `--port` on the post-crash KV
verifier. The first eight measured cells and SQL recovery had succeeded. The
repair retained those measured files, original manifest/error and telemetry,
then resumed verification and subsequent runs. Repeated repair verification
is unmeasured. Final status is `completed-with-orchestration-repair`, with all
32 measured cells valid and all eight full-state recovery checks successful.
The measured `read_paths.py` workload did not change during capture.

Raw evidence: [projection-only summary](../../bench/results/query-projection-2026-10-20/summary.json)
and [original failure manifest](../../bench/results/query-projection-2026-10-20/initial-manifest.json).

## Catalog fast-path result

The second experiment completed with `valid-diagnostic` status: 32 valid
measured cells, four process restarts and eight full-state recovery checks,
without an orchestration repair. Both versions were measured again.

| Query | Connections | Before queries/s | After queries/s | Before p99 ms | After p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| KV point | 1 | 1,302.42 | 1,940.13 | 1.069 | 0.609 |
| KV point | 8 | 3,047.33 | 3,289.18 | 5.506 | 5.639 |
| KV point | 32 | 3,347.34 | 3,604.63 | 24.888 | 23.138 |
| SQL point | 1 | 353.60 | 344.12 | 3.873 | 3.824 |
| SQL point | 8 | 1,107.95 | 1,085.31 | 15.063 | 15.466 |
| SQL point | 32 | 1,344.76 | 1,341.35 | 65.846 | 66.433 |
| SQL 100 rows | 1 | 48.91 | 44.38 | 23.775 | 31.892 |
| SQL 1,000 rows | 1 | 32.12 | 32.11 | 35.120 | 35.754 |

KV throughput increased 49%/8%/8% at 1/8/32 connections, while the primary
p99 metric improved 43% at one connection, worsened 2.4% at eight, and improved
7% at 32. This is a local observed difference with two process samples, not
a competitive-lead claim or a guarantee of the same gain on another host.
The serial KV process medians were 1,297/1,308 queries/s before and
1,953/1,927 after, supporting the point-read improvement within this capture.

SQL is not presented as faster. In particular, the 100-row query's aggregate
throughput fell 9% and p99 worsened 34%. Its two after-process medians were
40.06 and 48.69 queries/s versus 48.22 and 49.59 before; the worse sample is
retained. Other SQL throughput differences ranged from approximately -3% to
zero. Presentation allocation savings alone are insufficient to demonstrate
an end-to-end SQL improvement here.

Raw evidence: [catalog fast-path summary](../../bench/results/query-projection-2026-10-20/catalog-fast/summary.json),
[manifest](../../bench/results/query-projection-2026-10-20/catalog-fast/manifest.json)
and [build provenance](../../bench/results/query-projection-2026-10-20/build-provenance.json).

## Allocation evidence

Native Windows Rust 1.99.0, warmed in-memory runtime, result cache disabled,
thread-local counter, same diagnostic fixture. Execution and presentation are
separate counts, not nanosecond measurements.

| Operation | Baseline allocations | Projection only | With catalog fast path |
| --- | ---: | ---: | ---: |
| Full RQL execution, no extra nested row fields | 482 | 477 | 85 |
| Full RQL execution, 128 unreturned nested array elements | 611 | 477 | 85 |
| Build one-row JSON presentation | 167 | 164 | 164 |
| Select unfiltered 1/100/1,000 result rows | 3/201/2,001 | 0/0/0 | 0/0/0 |

Declared-model validation alone also allocates zero with either zero or 64
unrelated collections. Output data still allocates; these results do not mean
the complete request is allocation-free. Counts and command are retained in
[allocation-evidence.json](../../bench/results/query-projection-2026-10-20/allocation-evidence.json).

## Durable-write diagnosis

Separate `strace` profiles use the immutable baseline binary and a dedicated
database. The tracing container has `SYS_PTRACE` and `seccomp=unconfined` only
for instrumentation. Its timings include tracing overhead and do not measure
write capacity. Every write is checked through readback and process-crash
recovery. The `sync` profile explicitly records both durability environment
variables set to `sync`; the strict profile records `strict`.

| Mode | Workload | Completed operations | Sync syscalls | Syncs per overwrite |
| --- | --- | ---: | ---: | ---: |
| strict | GET control | 64 | 0 | — |
| strict | PUT, 1 connection | 64 | 256 | 4 |
| strict | PUT, 8 connections | 64 | 256 | 4 |
| sync | GET control | 64 | 0 | — |
| sync | PUT, 1 connection | 64 | 256 | 4 |
| sync | PUT, 8 connections | 64 | 256 | 4 |

All syncs target `/data/bench.rdb`; all return successfully. An additional
eight-write profile records 32 syncs with userspace stacks. Each pair reaches
`EmbeddedRdbArtifact::append_wal_payloads`: `File::sync_data` after WAL payload
write, then `File::sync_all` after publishing the alternate superblock.
Ordinary KV overwrite deletes and creates, resulting in two appends and four
barriers. The path mutex/file lock serializes append publication.

`finish_paged_write` dispatches embedded WAL actions before its durability-mode
match. The grouped commit coordinator therefore does not batch these embedded
appends, explaining why selecting `sync` leaves the count unchanged even with
eight concurrent writers. Both publication barriers currently protect recovery;
removing them would not be a valid performance fix.

Evidence: [strict profile](../../bench/results/query-projection-2026-10-20/write-profile.json),
[sync profile](../../bench/results/query-projection-2026-10-20/sync-profile/write-profile.json),
[userspace stacks](../../bench/results/query-projection-2026-10-20/trace-put-stacks.trace).
Source: [finish_paged_write](../../crates/reddb-server/src/storage/unified/store/commit.rs)
and [append_wal_payloads](../../crates/reddb-file/src/embedded.rs).

The next write change should batch a logical overwrite's WAL actions, then
implement embedded group commit with explicit durable acknowledgment floors.
Keep WAL/superblock ordering per batch and verify atomic failure behavior,
concurrent waiters and recovery with injected faults. Merely wrapping existing
delete/create calls in thread-local deferred capture is insufficient: nested
capture, rollback, mutation publication and watcher ordering need handling.
This PR deliberately does not modify that protocol.

## Validation and reproduction

Regression coverage includes declared model changes/mismatches (including
Mixed), legacy inference, missing/orphan collections, allocation scaling, all
nine KV response fields, tags/missing results, filters, transport JSON goldens
and sorting without mutation. Native targeted checks passed: 49 presentation
tests, 37 projection tests, four declared-model tests, REST index regressions,
the engine/presentation architecture boundary and `cargo check --locked`.

The first CI attempt exposed a misplaced presentation allocation test in the
storage layer. The test was moved to presentation, using a test-only exported
allocation counter. The architecture guard was preserved. A second fixture
correction asserts the existing `INVALID_OPERATION` prefix exactly; neither
repair changes production behavior.

Harness tests verify bad responses cannot count as successes, all multirow
values are checked, fixed-duration accounting and sync trace completion/failure
handling. The query and existing KV harness suites have 7 and 12 tests.
Artifact checksums are generated from LF-normalized files and verified in CI.
See [bench/query/README.md](../../bench/query/README.md) for workload, matrix,
verification and profiling commands. Source and evidence CI links are recorded
on the pull request.
