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

The known structural sparse-checkpoint regression remains a release blocker until
its existing test passes. The new KV fixture also fails on Linux for a fresh
embedded file: after acknowledged CREATE/PUT and process kill, the contract is
absent on the first reopen. Its fresh-paged and embedded-on-paged variants pass.
The embedded save path updates the auxiliary contract blob in memory without
recording that change in the logical WAL. This is a new confirmed regression,
not proof that the private Windows incident has the same cause. Keep the failing
fixture enabled until durable contract publication is corrected. Encryption/vault, native indexes, long-lived readers,
metadata overflow and true ENOSPC recovery need their relevant regression suites
and workload-specific qualification; the new matrix alone does not certify them.

The operational mission remains separate and incomplete until there is evidence
for the exact released artifact: repeated Auth/Platform and Free STG workloads,
all-database version inventory and storage time series, Free idle shutdown/wake
latency, and source-to-BigQuery CPU/RAM/storage/traffic in/out, requested config
revision, RedDB version and RedRouter AI token/cost records. Missing samples are
not zero. OCI downscale requires workload headroom evidence and a validated
volume migration; these local tests do not authorize an in-place volume shrink.


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
