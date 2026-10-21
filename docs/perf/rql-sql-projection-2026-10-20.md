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

## Durable-write diagnosis before the architectural follow-up

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
The initial read-path change deliberately did not modify that protocol.

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

## Architectural follow-up

Source revision `fae39eb6d07c32f8bc734973f10a9349a8e56a45` follows the merged
read-path work. The changes address correctness boundaries before measuring
speed. Evidence is retained separately in
[kv-architecture-2026-10-20](../../bench/results/kv-architecture-2026-10-20/).

Catalog cache rebuilds load metadata outside the lock and publish only if the
generation has not changed. Concurrent replacement, invalidation and removal
therefore cannot be overwritten by a stale rebuild. Existing readers retain
immutable Arc snapshots. Runtime and application callers now select validated
typed KV resolvers; they cannot supply arbitrary callbacks under segment locks.

Single-file, unversioned autocommit KV replacement stages the new entity, metadata and old
entity deletion under the publication lock, captures their WAL actions with a
non-nesting RAII guard, and commits them as one embedded publication. PUT, INCR,
CAS and DELETE in `KvAtomicOps` use the same per-key serialization. Watch events and secondary
index maintenance follow the storage commit. A returned publication error
restores the previous in-memory row and metadata; uncertain disk failures still
have an unknown durable outcome and never produce a successful acknowledgment.
Versioned KV, Vault, paged backends and explicit transactions retain their existing write paths.
The low-level helper still preserves Strict page flushing when used with a pager;
the runtime rejects that combination before selecting this optimized path.

Publication-fenced grouped KV writes enqueue before releasing the publication lock and wait
outside it. A synchronous elected caller drains bounded FIFO batches; there is
no permanent worker thread. Each batch retains both the WAL data sync and the
alternate-superblock sync. A checkpoint drains queued publications before
replacing their WAL with its snapshot. Batch errors reach every waiter, and an
unwinding writer releases waiting callers with errors. A failed keyed write
retains its queue ticket through rollback so checkpoint fencing still observes
the failed publication. Artifact readers take shared file locks to wait for
concurrent publication rather than failing Windows mandatory byte-range locks.
The store retains its coordinator for the database session. An uncertain I/O
failure or interrupted staging fences later writes and checkpoints until close
and reopen reloads the durable outcome. Dropping the last queue ticket cannot
silently clear that failure. A fence concurrent with disk publication also
prevents a successful acknowledgment.

Raw mutable row access now removes only that row's indexed key and tracks its
ID for conservative rechecking. It preserves all unaffected indexed candidates;
there is no whole-segment scan fallback. The extra candidate set is included in
resident-memory accounting. Segment fragmentation and MVCC history remain real
costs: lookup is O(segments + key versions + raw-mutated candidates), not O(1)
independent of history. The ignored
`key_lookup_fragmentation_history_diagnostic` exercises 1/16/64 segments and
1/100/1000 versions while checking the returned version and resident bytes.

SQL CPU profiling of the previous binary found 22.07% of samples in entity
cloning and 16.29% in value cloning. The caller was time-series hydration on
ordinary table rows before predicate evaluation. Hydration now borrows ordinary
rows and clones only when an interned time-series point needs resolved tags.
The regression also exposed an empty hash index after reopening: positional
rows had no attached schema during rebuild. Rebuilding and indexed batch reads
now attach the restored collection schema. Hash misses retain the scan fallback:
SQL numeric equality can match values with different physical index encodings.
Tests verify hit values and zero scanned segments before and after reopening,
and equality between numeric representations. KV tag indexes are also restored
from persisted metadata rather than disappearing on reopen.

Deterministic regressions cover stale catalog publication, logical replacement
failure/rollback and metadata recovery, bounded group membership, error fanout,
panic cleanup, checkpoint fencing, crashes at all three WAL publication points,
schema recovery, mutable key replacement and borrowed hydration. Existing
allocation, MVCC, moderation and memory-admission guards remain enabled.

### Checked architectural measurements

The closed read matrix uses main `0eb3a89c` and candidate `2584e484`, not the
later session-fence revision. It contains two process samples per version in
ABBA order, each with five two-second rounds. All values and ordering were
checked, and both datasets were verified after killing and reopening each
process. Result caching was disabled; both servers used Strict durability,
four pinned CPUs, a 2 GiB cgroup limit and a 1 GiB engine memory budget.

| HTTP read | Connections | Main queries/s | Candidate queries/s | Main p99 ms | Candidate p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| KV | 1 | 1,931 | 1,949 | 0.634 | 0.611 |
| KV | 8 | 3,182 | 3,204 | 5.878 | 5.871 |
| KV | 32 | 3,608 | 3,537 | 23.095 | 23.628 |
| SQL point | 1 | 314 | 1,872 | 5.024 | 0.658 |
| SQL point | 8 | 955 | 3,226 | 18.349 | 5.771 |
| SQL point | 32 | 1,157 | 3,619 | 79.550 | 22.894 |
| SQL 100 rows | 1 | 45.40 | 49.91 | 26.316 | 22.393 |
| SQL 1,000 rows | 1 | 30.93 | 32.91 | 39.707 | 33.885 |

These are medians of the two process medians, without a confidence interval.
The main SQL point c1 process medians ranged from 262 to 366 queries/s; the
candidate ranged from 1,846 to 1,897. The slower second main process raises the
aggregate ratio to 5.96x. The earlier, separately retained exploratory matrix
at `90feffe7` measured 5.21x. Do not pool the revisions or discard the slower
baseline. The cause of that process variance is not established. KV c32 is
slightly worse. This Python/HTTP/shared-host diagnostic establishes neither
engine capacity nor competitive superiority over KiviDB.

The separate syscall capture at `2584e484` checked 64 overwrites per phase,
read back each value and verified persistence after killing the server:

| Mode | Connections | Main sync calls | Candidate sync calls |
| --- | ---: | ---: | ---: |
| Strict | 1 | 259 | 131 |
| Strict | 8 | 256 | 128 |
| Grouped (200 microseconds) | 1 | 259 | 131 |
| Grouped (200 microseconds) | 8 | 256 | 32 |

The c1 phases include three additional syncs from one WAL-full checkpoint.
The clean c8 phases show four sync calls per overwrite falling to two in
Strict mode, and 0.5 per overwrite with grouping. All syncs target the same
artifact and return success; GET controls issue zero. The eightfold grouped
reduction is in syscall count, not measured write throughput. Both durability
barriers remain. Raw traces and recovery responses are retained under `writes/`.
Repeating both candidate modes at final revision `fae39eb6` in fresh volumes
produced the same counts: 131 c1, 128 Strict c8 and 32 grouped c8, with zero
GET syncs and checked recovery. Those captures are under `writes-final/`;
the final binary SHA256 is
`3337efdfdd686fafca7d9d02fc82f53c18bc589824f391300946256c45ac1adf`.

The read binaries' SHA256 values are
`6afcf03b3cc7aa42d693f02bdf653f7ca1cda2b9e3e125940b8e4551c8626c56`
(main) and
`9b015e9f6d51cd8a3e93ff2af75fa2487cfc7ce4da9a94df842b30562a20d0dd`
(`2584e484`). Complete Git archives were compared byte-for-byte with the Linux
build source; changed files received fresh modification times before building.
An earlier mixed-source overlay and a cache-reused baseline were discarded
before these captures. The invalidated test log is preserved and does not
represent the current source. The CPU hotspot profile predates main and is
diagnostic evidence only, not the baseline timing binary.

Native validation passed the nine grouping/fault/crash tests, ten keyed tests,
the targeted schema/index/cache regressions, `cargo check --locked`, formatting
and the 7+12 harness tests. Full Linux CI for `2584e484` passed in
[run 39509985752](https://github.com/reddb-io/reddb/actions/runs/39509985752).
The later session-fence revision is validated separately in
[run 39514945742](https://github.com/reddb-io/reddb/actions/runs/39514945742),
whose Rust source is identical to `fae39eb6`.
The native broad KV run had 46 passes and one failure: the Vault/paged
constructor hits Windows mandatory-lock error 33. The same test fails on clean
main `0eb3a89c` before any writes. Both logs are retained; this pre-existing
paged limitation remains open. The low-level Strict page-flush regression runs
on Unix, while runtime eligibility keeps paged writes on their existing path.

### Updated Redis / KiviDB comparison

The untraced repeat at `fae39eb6` is closed under
[kv-compare-final-2026-10-20](../../bench/results/kv-compare-final-2026-10-20/SUMMARY.md).
It uses the immutable final binary above, Redis 8.4.0 and KiviDB 1.1.0, pinned
image identities, fresh volumes, four server CPUs and two separate client CPUs.
Each engine runs alone. GET uses 1,000/10,000 keys, 100-byte values, five
two-second rounds, and 1/8/32 connections. Every value is checked. Each matrix
and write cell is verified after process termination and reopening. All 96
recorded setup/measurement/recovery steps completed without errors; 12 RESP
pipeline cells remain explicitly unsupported for the HTTP RedDB adapters.

For 10,000 keys without pipelining, the observed HTTP KV results are:

| Connections | RedDB GET/s | Redis GET/s | KiviDB GET/s | RedDB p99 ms | Redis p99 ms | KiviDB p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2,471 | 4,888 | 2,759 | 0.52 | 0.26 | 8.47 |
| 8 | 4,034 | 8,233 | 5,294 | 6.09 | 2.95 | 8.69 |
| 32 | 4,359 | 8,156 | 9,565 | 25.15 | 12.74 | 10.93 |

RedDB remains behind in throughput. Its observed p99 is lower than KiviDB at
one and eight connections, and higher at 32; it is higher than Redis in all
three cells. The separate KV-via-RQL adapter measured 1,947/2,966/3,410 GET/s
at those connection counts. Those are KV commands through RQL, not the SQL
point query whose speedup was measured above. The unfavorable 1,000-key RQL
c32 cell remains visible: 1,093 GET/s and 99.24 ms p99, with a round range of
989–2,841 GET/s. Redis also varied substantially in some cells. No independent
repetitions or dedicated-host confidence interval establish a leadership claim.

Durable overwrite uses 1,000 target keys and five rounds of 500 writes, with
readback after each round. Redis acknowledges AOF `always`; KiviDB operations
include `SET` plus successful `WAITAOF 1 0 5000`; both RedDB modes retain their
existing WAL and superblock syncs. For eight connections:

| Product path | Writes/s | p99 ms |
| --- | ---: | ---: |
| Redis | 2,529 | 7.88 |
| KiviDB with local persistence fence | 2,199 | 5.97 |
| RedDB Strict | 243 | 60.44 |
| RedDB grouped, matched background population | 205 | 731.88 |

The first grouped continuation used a fresh volume with only the 1,000 target
keys. It measured 519 writes/s and 103.44 ms p99 at c8. It is retained as the
separate `grouped` diagnostic and is unsuitable for attributing all differences
from Strict to grouping: the primary matrix already holds another 11,000 keys
in the two read collections. The `grouped-matched` continuation therefore
preloads and reopens/verifies those same background collections before running
the identical write cells on another fresh volume. That comparable population
produced the unfavorable 205 writes/s result above, with round throughput
107.6–277.8 writes/s. At c1, Strict/grouped-matched measured 91.7/144.1 writes/s.

Fewer sync calls have not established a sustained write throughput or tail
latency improvement on populated stores. CPU samples are low during these
write cells; they do not by themselves identify the waiting path. Full-WAL
checkpoint/retry behavior is a candidate for separate profiling, not a proven
cause of this result. The next write optimization needs a populated-store
regression workload and p99 evidence alongside sync counts. All unsuccessful
performance comparisons remain in the report; no engine changes were made
during these measurements.
