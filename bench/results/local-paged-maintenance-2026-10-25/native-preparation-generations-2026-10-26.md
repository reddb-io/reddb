# Optimistic native preparation before checkpoint publication

Date: 2026-10-26. This candidate follows the frozen login contention
reproduction at `e5048b52e933d94c6872e8a8dc48ce3e90c09a3d`.
No signed container or historical Auth/Platform acceptance is established
by the local results in this report. RC375 remains rejected.

## Question and correction

Can native artifact construction run before taking the row/WAL publication
fence, while rejecting bytes derived from a source that changes before
publication?

The affirmative HTTP regression failed against the prior engine: the real
login ledger could not complete during a five-second pause at native
preparation. This is the same path that previously exhausted the ordinary
30-second HTTP deadline, reduced without changing production configuration.

The candidate stages native payloads before `flush_local_only()` acquires
`vault_publication_lock`. It retains weak manager identities, segment mutation
generations and canonical source digests. Under the publication fence, after
the primary flush, every staged collection is checked against its current
manager, generations and retained physical references. Unchanged proofs can
reuse their published chains; replaced managers, changed generations,
unproved sources and invalid references cannot publish staged bytes.

This is optimistic generation validation, **not an immutable snapshot**.
When validation fails, the disposable persisted native cache for that source
is deferred; its authoritative rows, WAL and declared query indexes still
checkpoint. A later stable checkpoint builds its current cache. There is no
unbounded retry loop or conservative full rebuild under the final fence.
Continuously changing sources may repeatedly defer these caches; their query
behavior and cold costs remain part of historical qualification.

One independent reentrant admission gate bounds concurrent preparation.
Publishers only try that gate, including callers already holding publication;
they never wait for preparation while holding the store fence. If admission
is busy, they retain only proven unchanged caches. Proof-map mutex guards are
released before any store-fence acquisition, avoiding a new inverse lock order.

Existing physical chains are read while protected against retirement/reuse.
Their current descriptors are checked before copying payloads. Fulltext frame
decode, delta application, canonical serialization, full construction and
source hashing run outside the checkpoint's long publication section. Payload
copy/decompression and final page persistence still take existing protection;
this does not establish bounded end-to-end latency or per-page incremental work.

The row/WAL fence, force-sync ordering, DWB protection, final header publication
and detached-page retirement retain their prior order. No file format changes
are introduced by this correction.

## Focused evidence

The same affirmative HTTP test first failed against the prior engine in
6.87 s. The initial corrected run passed: HTTP status `200 -> 200 -> 200`,
with concurrent login completing around 966 ms and all three authentication
events recovered after cold reopen. Another integration run measured about
434 ms; these shared-host observations are causal diagnostics, not a paired
throughput or latency comparison.

The fixture now uses six source rows, a unique ID index and a secondary
payload index. It pauses preparation, completes an audited HTTP login and
changes a source row before publication. The stale native cache is absent
after publication; table payloads and secondary equality remain correct.
The next stable checkpoint rebuilds the cache, and cold reopen preserves
the updated indexed row and all three distinct, complete login events.
The HTTP test keeps the server's ordinary 30-second deadline.

All 23 tests in `storage::unified::devx::reddb::` passed locally under
Rust 1.95.0, including:

- A changed source generation rejects staged payloads, summaries and reuse
  proofs, followed by a current-source byte comparison after checkpoint.
- A changed retained-page descriptor rejects reuse.
- An overlapping preparation does not block a publisher that already owns
  the row/WAL fence; a later checkpoint restores the deferred cache.
- Cold reuse, legacy/corrupt-payload fallback and six source-proof crash
  subprocess phases, plus existing native frame/delta regressions.

All 30 tests across nine relevant integration suites also passed: audited HTTP,
concurrent/open transactions and crash recovery, GC provenance, idle pressure,
sealed preimages, sparse updates, native allocation and native shrink/cold
reopen. The 64 MiB text-corpus allocation test measured 566,726 additional live
bytes for unrelated DDL and 597,468 for republishing an unchanged checkpoint;
the unchanged bound remains 16 MiB. This does not measure peak first-build
memory on the historical Platform input.

The tightened HTTP check also passed, requiring receipt of the complete 200
response before releasing preparation, not only completion of the ledger
call: ledger entry 408.008 ms, complete HTTP response 414.867 ms. Its six-row
mutation, indexed lookups, deferred-cache rebuild and recovered audit content
checks passed in 2.59 s overall. The locked server/test check and workspace
formatting check passed. Local executions use one scope at a time, capped at
8 GiB RAM and 4 GiB swap.

## Remaining acceptance

The initial historical artifact materialization fit the unchanged 64 MiB
allocation gate only after compression; it still took 50.782 s overall.
Removing derivation from the publication fence does not itself reduce that
total below the required 10 s. The existing final store publication was
10.514 s. These are RC375 measurements, not measurements of this candidate.

The next immutable artifact must pass full code CI, then the same historical
Auth and Platform inputs under their intended budgets. Check complete payloads,
all applicable indexes, repeated writes/checkpoints, pinned snapshots and
reclamation after release, acknowledged-write crash recovery, shutdown and
logical versus allocated main/WAL/auxiliary growth. Keep rejected inputs frozen.

The original mission remains open: fleet release/upgrade including free,
observation and safe volume reduction, partitioned BigQuery resource/config/
engine-version/AI usage coverage, and Filipe's STG free load/sleep/wake tests.
