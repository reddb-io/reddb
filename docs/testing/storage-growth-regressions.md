# Storage growth and recovery regressions

A fixed logical dataset must not acquire a new permanent page allocation on each
checkpoint or recovery attempt. A successful open is insufficient: payloads,
parent traversal, leaf traversal, collection models, allocator uniqueness and
space accounting must agree after recovery and after a further cold open.

Run on Linux (the process-exit ownership hooks are test-only):

```sh
CARGO_BUILD_JOBS=2 make test-storage-apocalypse
make test-dst-storage
```

The `Storage recovery and growth` workflow runs both on storage PRs, daily and
through `workflow_dispatch`, plus a Windows job for the unclean KV restart
fixtures. It retains logs even when a test fails. These tests
are not ignored and have no `continue-on-error`. Repository branch protection
must separately require the workflow if it is to be a mandatory merge gate.

## Failure-to-test map

| Identified failure | Executable regression / oracle |
| --- | --- |
| Replacement allocations leak before catalog publication | `interrupted_checkpoint_reclaims_unpublished_pages`, `interrupted_checkpoint_with_wal_reclaims_unpublished_pages`, `interrupted_replay_reclaims_unpublished_pages`, `interrupted_replay_reclaims_durable_unpublished_pages`: 12 interrupted attempts and bounded file growth |
| Retired pages forgotten between publication and freeing | `interrupted_retired_reclamation_preserves_reuse`: compare actual reuse before enqueue, after enqueue and after sync; reject duplicate allocation |
| Recovery itself crashes | `ownership_recovery_survives_interruption_and_retry`; `checkpoint_and_recovery_crash_matrix`: crash recovery again before allocator sync, after sync and after journal Finish |
| Repeated crashes with overflow and multiple trees | `checkpoint_and_recovery_crash_matrix`: 2 durability modes × 4 checkpoint boundaries × 3 recovery boundaries × 8 generations; 144 fixed rows across 3 collections, four cache slots, random-looking and compressible payloads; verify every payload on two cold opens per generation |
| Parent routes disagree with leaf links | New crash matrix compares the full ordered leaf range with individual B-tree root lookups, including encoded values; existing `btree_mixed_width_split` and portable B-tree fixtures exercise splits |
| Freed pages are not reused, or reused twice | `multi_trunk_recovery_reuses_every_page_without_growing`: more than two freelist trunks, both superblock fallback choices, 3 reuse generations; exact allocation-ID set, no appended pages, live sentinel and reused-page markers survive |
| Delete/refill or overflow-to-inline churn keeps allocating | `delete_refill_and_overflow_resize_plateau`: 16 cycles in Strict and Grouped, 3 collections, empty checkpoint/reopen, same-key short/large replacements, full payload verification |
| WAL growth hidden by stable main-file size | New crash/churn tests measure main-file length, WAL length and allocated filesystem blocks separately after recovery/checkpoint; all generations after warmup must stay within a fixed allowance |
| Recovery chooses stale allocator state from the other superblock | `recovery_reclaims_pages_idempotently_across_superblock_fallbacks`, new multi-trunk test; portable recovery tests also interrupt allocator synchronization |
| Dirty eviction, torn writes, partial DWB and sync failure | Portable pager tests (`pager_multiframe_checkpoint_io_and_crash_recovery`, `pager_recovery_failure_boundaries`, `pager_dirty_eviction_recovers_a_torn_home_write`); `pager_allocation_recovery`, `pager_freelist_flush_recovery`, `pager_flush_transaction`; `make test-dst-storage` |
| Pending or aborted transactions enter checkpoint, or snapshots lose versions | `checkpoint_open_transaction`, `checkpoint_concurrent_transaction`, `checkpoint_gc_provenance`, `vacuum_snapshot_horizon` |
| Checkpoint falls back to whole-tree work for sparse changes | `checkpoint_sparse_update`, including `same_collection_pending_pressure_keeps_checkpoint_incremental`; keep the existing performance assertion, do not waive it to make this lane green |
| Structural sparse checkpoint overwrites published pages before its catalog commits | `overflow_checkpoint_undo_survives_interrupted_recovery_and_publication`: overflow payloads, four cache slots, interrupted undo twice, post-publication crash, full payload checks and three further checkpoints/reopens |
| Undo WAL append fails, or an image is corrupt/incomplete | `checkpoint_before_image_failure_prevents_cache_and_disk_overwrite` fences both write APIs before cache changes; the overflow undo fixture rejects invalid ownership, page size/checksum, duplicate images and truncated WAL before restoring any page |
| Native KV model becomes table after an unclean restart (#2536) | `kv_model_and_value_survive_kill_without_bootstrap_checkpoint`: kill after acknowledged CREATE/PUT, verify model and value on 3 reopens; fresh embedded, fresh paged and embedded options opening a paged file |
| Document contracts and native indexes disappear on replay | `paged_checkpoint_rebuild`, including `document_indexes_survive_reused_checkpoints_and_uncheckpointed_wal_recovery` and `legacy_paged_document_definition_survives_exit_without_checkpoint` |

## Interpreting space checks

The crash matrix uses four warmup generations, then allows at most two database
pages of main-file or WAL growth. The delete/refill test compares equivalent
short and large phases after cycles 6/7. The physical-block allowance is 128 KiB
because filesystem extent allocation differs from logical page allocation.
Neither check demands shrinking the file after a delete: previously allocated
pages may remain in the file, but subsequent equivalent work must reuse them.
The multi-trunk test is stricter: every recovered page ID must be returned exactly
once, the live sentinel must never be returned, and the file must not grow at all.

The payload generator and expected model do not read expected values back from
the database. A subprocess must exit at the requested failpoint with its exact
exit code; a normal exit or panic is a test failure. The KV test uses an ACK marker
and parent-side process kill, with a 30-second setup deadline.

## Limits and remaining qualification

This is coverage of named failure modes, not proof of every possible page state.
Process exit/kill preserves the kernel page cache. SimVfs and portable pager tests
supply separate power-loss/torn-write models; they do not imply every native
checkpoint syscall has been swept under every physical fault combination.
The KV fixture exercises the contract in #2536, not the private Windows Hunter
RedWire workload. A passing Linux run does not qualify that Windows incident.

The structural sparse-checkpoint and acknowledged KV fixtures remain required
release gates. Structural projection now uses the checkpoint undo protocol below;
embedded auxiliary contracts are durably appended to the logical WAL. These
fixes must pass on the final artifact. Encryption/vault, native indexes, long-lived readers,
metadata overflow and true ENOSPC recovery need their relevant regression suites
and workload-specific qualification; the new matrix alone does not certify them.

The operational mission remains separate and incomplete until there is evidence
for the exact released artifact: repeated Auth/Platform and Free STG workloads,
all-database version inventory and storage time series, Free idle shutdown/wake
latency, and source-to-BigQuery CPU/RAM/storage/traffic in/out, requested config
revision, RedDB version and RedRouter AI token/cost records. Missing samples are
not zero. OCI downscale requires workload headroom evidence and a validated
volume migration; these local tests do not authorize an in-place volume shrink.

## Structural checkpoints and recovery memory

Paged format `0x0001_0003` requires checkpoint undo recovery. Both superblock
copies are fenced to this version before the first ownership epoch; older
readers reject it. Rollback to an older binary requires restoring the complete
pre-upgrade backup, including its WAL and metadata.

Sparse overflow updates and splits no longer copy the entire collection tree.
The first overwrite of each existing allocatable page synchronously appends its
before-image using the existing `FullPageImage` WAL record, with transaction 0
and the ownership epoch. Newly reserved pages need no undo. The callback runs
before inserting the replacement in cache because eviction can write immediately.
Append failure fences all further page writes. The catalog remains the commit
boundary and is never itself an undo target.

Before following collection roots, recovery validates the journal and every
image, then reads the publication epoch directly from catalog metadata. An
unpublished epoch is restored in a second streaming pass, one page at a time,
and synced before its reservations are reclaimed. A published epoch is not
undone. Allocator recovery preserves every page still named by the catalog's
deferred-free ledger, including a page reserved and retired in the same epoch.
An interrupted restoration repeats safely from the same durable images.

Cost sketch: N first-touched existing pages add N × 16 KiB of WAL and up to N
synchronous appends per checkpoint; recovery retains ownership records, page-ID
sets and one image at a time, instead of N image payloads. This exchanges WAL I/O for bounded recovery
payload memory and avoids whole-tree copying for a sparse structural change.
It does not eliminate full replacement for pinned readers or unknown mutations.

## Memory regressions during recovery and observation

`crates/reddb-engine/tests/recovery_memory.rs` checks named-row recovery against
bulk-load resident accounting, 32 native-record reopen cycles, dynamic fields,
a bounded cache of row shapes, captured-cursor immutability and compiled column
projection. Shared row names never declare a collection schema. Native records
keep their names on the next checkpoint through the existing named-row encoding.
This is reader compatible, but changes compact positional rows to self-describing
named records when a schema is available; validate mixed-version replication
before qualifying a release.

The native runtime tests cover storage-only paged reopening before runtime
initialization, repeated runtime reopening, filters/projection against a different
dynamic row shape and aliased RedWire columns. The scoped-stats allocation test
observes all six model profilers while an unrelated collection grows. It measures
allocated bytes as well as allocation count, since worker-thread cloning can
leave only a few large allocations on the calling thread.

System collections in the `red_` and `red.` namespaces retain their named-row
representation: bootstrap and catalog readers still access those maps directly.
Both early and late recovery compaction must preserve it. A storage-only cold
open checks the durable bootstrap marker before runtime initialization, then
repeated runtime opens check the selected preset and declared column schema.
Compacting user data must never cause an existing vault to bootstrap again.

Document statistics, inferred columns and document counts borrow stored fields.
Document statistics decode one body at a time and reuse the planner's bounded
column samples, scanning each column instead of retaining every document's
payload. The allocation regression adds 8 MiB of binary fields while requiring
the same observation results and no corresponding allocation growth.
`make test-memory-regressions` runs these gates in the storage CI even when an
earlier storage or power-cut step fails.

Metadata publication also admits optional native search-artifact snapshots
before cloning source rows. Disabled capabilities do not build artifacts or copy
source rows: in particular, full-text-disabled table collections must not acquire
full-text snapshots during unrelated DDL. The input allowance is budget / 256 (at most 2 MiB)
per collection; encoded artifacts retained by one publication share budget / 128
(at most 8 MiB). JSON path expansion uses a bounded explicit stack and accounts
for retained paths and scalar values. Larger derived snapshots are omitted and
reported as `deferred-memory-budget`; stale artifact blobs are retired through
the existing publication path. Canonical records and live runtime indexes remain
the source of queries. This limits persisted derived acceleration: it does not
implement a spill-to-disk builder for arbitrarily large native artifacts.
The regression checks DDL with unrelated binary payloads, cold reopen after
deferral, correct canonical reads, and publication after VACUUM makes the
collection fit again. Native artifact shrink/reopen tests still verify the
published path for collections that fit.

Embedded single-file contract changes append auxiliary metadata to the logical
WAL before acknowledging DDL. Publication holds the same store fence across
cache edits, contract-map serialization and WAL append. Recovery applies the
latest auxiliary blob before hydrating the contract cache; an empty blob clears
it. This adds store-action tag 7. Older readers reject that tag, so upgrade
readers before opening a WAL containing these contract edits. Existing native
record and snapshot encodings are unchanged. The process-kill KV regression
covers fresh embedded, fresh paged and embedded options reopening a paged file.

Run the focused gates with:

```sh
cargo test -p reddb-io-engine --test recovery_memory
cargo test -p reddb-io-server --lib recovery
cargo test -p reddb-io-server --lib collection_stats_do_not_clone_unrelated_database_rows
cargo test -p reddb-io-server --lib trusted_wire_column_checks_recovered_row_shape_and_keeps_alias
```

Resident accounting is not process RSS. STG qualification must also record
container RSS/working set and memory limit, engine budget, exact image digest,
Pod UID, OOM/restart state, original collection models/counts and acknowledged
payloads through cold open and concurrent observation. A failed constrained-memory
run remains a failed gate, even when the unit tests pass. Never run broad
profiling diagnostics on a serving database while qualifying their memory bounds;
use an isolated clone first.

## Local artifact qualification under real memory limits

On a Linux host with a working user systemd manager and cgroup v2, run one case
at a time against the exact downloaded release or candidate binary:

```sh
python3 scripts/qualify-local-memory.py --binary /absolute/path/to/red \
  --output /var/tmp/reddb-qualification-unique --memory-mib 512
```

The output directory must not exist. It contains synthetic credentials, private
engine logs, samples and a machine-readable result; preserve it on failure and
do not publish its raw contents. The harness runs only its own transient services,
binds all listeners to loopback, enforces zero swap, at most one CPU, a 30-minute
service lifetime and a 2-GiB external allocated-disk guard. The client and sampler
run outside the database cgroup. It does not need or use cloud credentials.

The default scenario seeds 1,024 table rows (inline, overflow and compressible)
and 32 large documents, then checks every payload through twelve indexed
update/delete/refill cycles, index drop/recreation, 24 concurrent reads, eight
SIGKILLs after acknowledged writes, and a clean reopen. KV model/value recovery
and vault certificate reuse are exercised too. It records boot time, memory
peaks/events and main-file/WAL/whole-family storage separately. A namespaced
container and a nested host service must both resolve their actual memory cap;
`--probe-only` makes that boot assertion a short regression.

Storage comparisons use four-cycle bursts followed by ten seconds for paced
history reclamation and two publication checkpoints. A burst can temporarily
outpace background MVCC reclamation; this is distinct from permanent allocation
leaks. `--settle-seconds 0` tests sustained input without that drain and can fail
the growth bound even while a drained workload passes. Such a failure remains
visible and does not qualify sustained throughput. `--drain-seconds 10` between
each cycle and `--vacuum` are diagnostic controls, recorded in the receipt;
explicit VACUUM results must not be reported as automatic reclamation evidence.
`--budget-mib 384` distinguishes an explicit 384-MiB budget from auto-detection
inside a 512-MiB kernel limit. A passing explicit-budget run cannot qualify the
automatic resolver. Use `--memory-mib 256` for an adversarial smaller slot and
`--cpu-percent 25` for the Nano CPU constraint; neither changes the dataset.

This harness fails on budget mismatch, lost/duplicate data, OOM, excessive growth
or failed recovery. It is a local runtime qualification tool, not a substitute
for the existing crash-boundary tests or measurements on OCI storage. It does
not qualify a sustained production workload, authenticated application sessions,
all possible page states, or power-loss behavior.

The allocator also has direct crash regressions for a WAL barrier with no active
page-ownership journal: `unjournaled_trunk_allocation_preserves_unconsumed_free_pages_after_crash`
reserves one page from a persisted trunk, discards volatile state, then requires
every untouched ID to be reused exactly once without file growth.
`unjournaled_trunk_rewrite_io_failures_preserve_remaining_ownership` interrupts
reads/writes/syncs around the protected trunk rewrite. Recovery may retain the
single uncertain reservation, but cannot lose the trunk remainder or return a
successfully reserved ID again. Outside an ownership epoch, this path pays one
protected page write per reused ID; journaled checkpoint/replay allocation keeps
its batched path. No file format change is introduced.

## Maintenance cost under sustained load

Standalone, writable runtimes using the embedded deployment profile run table
history reclamation after readiness. The worker waits 200 ms between iterations,
scans at most 4,096 physical positions and reclaims at most 256 versions per tick.
It skips a busy publication fence without advancing the cursor and respects
active snapshots and aborted transactions. Failures back off for five seconds.
These are work bounds, not a hard wall-clock deadline: a WAL flush or slow disk
can still lengthen a tick. No full-file rewrite or arbitrary orphan-page repair
is performed by this worker.

The operator controls are:

```sql
SET CONFIG runtime.mvcc.autovacuum.enabled = true;
SET CONFIG runtime.mvcc.autovacuum.scan_rows_per_tick = 4096;
SET CONFIG runtime.mvcc.autovacuum.reclaim_rows_per_tick = 256;
```

Budgets clamp to 1–4,096 scanned positions and 1–256 reclaimed versions.
Disabling reclamation is a diagnostic control, not a storage-growth remedy.
Long-lived snapshots can legitimately retain history. Reusing freed pages and
reducing the physical file size are separate operations.

Measure the cost against an identical control on the exact candidate artifact:

```sh
python3 scripts/qualify-maintenance-cost.py --binary /absolute/path/to/red \
  --output /var/tmp/reddb-maintenance-unique --memory-mib 512
```

The harness creates four independent stores in OFF/ON/ON/OFF order, each with
256 overflow-payload rows, four concurrent writers/readers, and 1,000 updates
per writer at a target of 160 total requests/s. Every read checks the exact
payload and acknowledged revision. Each case finishes with checkpoint,
SIGKILL and recovery verification. It records per-request p50/p95/p99/max,
scheduled completion latency (including queue delay), achieved throughput,
CPU/throttling and I/O counter deltas, cgroup memory peaks, and main/WAL/family
storage. I/O is reported as null when that controller is not delegated to the
user service; no host controller settings are changed. It never invokes manual
VACUUM or pauses the measured workload for GC.
The checkpoint and recovery stages are outside the measured load interval.

Use the same private-output precautions as the memory harness. Run cases
sequentially on an otherwise quiet host. An error, OOM, incorrect payload or
failed recovery fails the command. Timing is reported without a universal
pass/fail percentage: compare the full ranges, queue delay, resource limits
and storage state against the application's latency and throughput budgets.
At a fixed offered rate, matching throughput proves that rate was sustained,
not equal maximum capacity. Repeat with higher offered rates and on OCI disk
before extrapolating. This small hot-row workload does not qualify whole-file
compaction, every index/model, long snapshots, or a large historical STG store.
