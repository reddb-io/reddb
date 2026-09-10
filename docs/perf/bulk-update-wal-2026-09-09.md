# Bulk UPDATE WAL diagnosis (#2319)

This is shared-host diagnostic evidence, not a competitive performance claim
under ADR 0076. Other projects were compiling during the investigation.

## Workload and storage

The JavaScript SDK connects through the `red` subprocess using `memory://`.
In this revision, that URI creates an **ephemeral persistent file**, including
its embedded WAL. It does not select a RAM-only engine. The memory budget was
64 MiB. The fixture seeds 2,500 `(id, payload)` rows in batches of 500, then
updates every payload using a parameterized statement. Readback checks every
expected key and value, including duplicate/missing/unexpected keys.

The initial shortened diagnostic used four statements per process (the first
is warmup), with Rust 1.97.1 debug binaries. Milliseconds, in execution order:

| Source | Warmup | Statement 1 | Statement 2 | Statement 3 |
| --- | ---: | ---: | ---: | ---: |
| Before memory governance (`d07b677c1`) | 4447.123 | 4458.660 | 4945.785 | 5292.612 |
| Merged memory governance (`f40d2244a`) | 4364.997 | 4571.041 | 5959.631 | 4923.904 |

These runs confirm slow bulk UPDATEs, but do not attribute a regression to
memory admission. They are not independent controlled comparisons.

## Phase diagnosis

Temporary timers on `f40d2244a` separated selection, preparation, admission,
publication, secondary indexes and CDC. The four instrumented statements took
4.550, 6.899, 7.076 and 7.300 seconds end to end. Publication accounted for
4.438, 6.726, 6.883 and 7.089 seconds. Admission took only 11–19 milliseconds
per statement (it is included in preparation, not added to it).

A second probe split publication into old-version update, new-version insert,
indexing, serialization and finalization. Most publication time was in
`finish_paged_write`, called once per versioned row. The four end-to-end times
were 13.898, 12.745, 17.151 and 14.614 seconds; finalization alone accounted for
12.016, 10.863, 15.175 and 12.786 seconds. The much slower host during this run
is another reason not to use these numbers as an accepted latency comparison.

The embedded append writes WAL frames, synchronizes them, publishes the next
superblock boundary, and synchronizes again. A sequential 2,500-row UPDATE
therefore waits for 2,500 durable appends. The group commit coordinator cannot
combine this embedded path's sequential per-row waits.

Cost sketch: for N rows and chunk size C = 2,048, two synchronization calls per
row cost approximately `2 * N * sync_latency`; batching the same WAL records
per chunk reduces this component to `2 * ceil(N / C) * sync_latency`, plus
any required WAL growth/checkpoint work. WAL payload bytes still have to be
encoded and written. Acknowledged writes must remain durable.

The full diagnostic script retains seven independent processes per binary,
alternating order, the original warmups/iterations, per-statement samples and
per-run p50/p95/p99. With only eight bulk statements, p95 and p99 are both the
maximum observed statement; they are descriptive, not precise tail estimates.
Do not treat correlated statements as independent runs for confidence bounds.

## Change and correctness contract

`persist_update_chunk` reuses the existing deferred-WAL wrapper for a chunk
when no enclosing capture exists. An explicit transaction or event-enabled
statement keeps ownership of its existing capture. An ordinary autocommit
chunk appends its captured records durably before returning to index/CDC
maintenance. The wrapper also appends completed writes if a later item fails,
as the existing event-enabled autocommit path does.

All-target memory admission and reservation lifetimes are unchanged. Normal
UPDATE retains its 2,048-row publication chunks. This changes neither WAL
encoding nor the configured durability policy. It does not add general
statement rollback for I/O errors.

The regression fixture updates 2,050 rows and reads the durable superblock
generation. Before the change, it advanced 2,057 times (individual appends
plus WAL-growth checkpoints). The required bound is two chunk appends and at
most one growth checkpoint per chunk. The child then exits without running
destructors; the parent reopens the database and checks all 2,050 IDs and
payloads. No elapsed-time assertion is used for this structural regression.

The first batching candidate passed the publication-count bound but failed
recovery: the last two IDs retained `seed`. Inspection found that
`load_from_bytes_with_config` reconstructed a snapshot using `insert_auto`
while `embedded_wal_path` still targeted the live artifact. Loading old
snapshot images could therefore append them after newer durable WAL images,
including checkpointing the partially reconstructed store. Larger batches
made the snapshot/WAL boundary in this fixture expose that existing hazard.

Snapshot reconstruction now temporarily removes the embedded append path and
restores it before returning the store. A separate regression starts with a
snapshot plus a newer WAL action, checks that loading preserves the entire
artifact byte for byte, and verifies that a subsequent normal insert still
advances the durable boundary. The multi-chunk process-exit regression checks
the complete snapshot-plus-WAL recovery path.

Issue #2319 remains open for equivalent optimized builds and independent
run-level latency evidence on an uncontended host. Neither this diagnosis nor
a large reduction in sync count proves superiority to SQLite or SurrealDB.
