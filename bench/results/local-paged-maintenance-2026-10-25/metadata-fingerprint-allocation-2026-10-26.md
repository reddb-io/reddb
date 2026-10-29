# Metadata fingerprint rendering allocation

Base source: `42aeb7ca5ad779e053f128c3a3aa1dd76b5394f9`.
That base completed full CI successfully in run `40356172394`. Its result
does not qualify the additional implementation in this report or an unsigned
artifact for rollout. RC376 remains rejected for its first 15.724 s Platform
checkpoint; this experiment does not replace that performance gate.

## Question and isolated reproduction

`physical_collection_roots()` fingerprints raw entity fields through their
existing Rust `Debug` representation. Previously, each call first allocated
the complete rendered field with `format!`, then hashed that temporary string.
This adds transient memory proportional to the largest rendered value during
cold or invalidated metadata scans.

The real-engine regression `tests/metadata_fingerprint_memory.rs` inserts a
4 MiB text field into an isolated paged store and publishes its baseline.
It invalidates only the memoized physical root with the existing manager API;
row revisions and native artifact admission remain unchanged. The next
checkpoint therefore measures metadata fingerprint recomputation without
confounding it with full native artifact construction.

| Implementation | Additional peak live allocation | Result |
| --- | ---: | --- |
| Previous rendered-string helper | 8,446,092 bytes | Fails the preselected 2 MiB ceiling |
| Streaming helper | 547,516 bytes | Passes, with identical persisted root |

The test was executed and failed before the production helper changed. The
corrected execution also reopens the store and verifies the complete 4 MiB
payload. Allocation accounting uses requested live bytes through a single-test
global allocator; it is not a process RSS measurement or a Platform speedup.

## Change and compatibility

An infallible `std::fmt::Write` adapter feeds each formatting chunk directly
into the existing FNV state. It retains the same seed, byte ordering, `Debug`
representation and running state across fields. No persisted format, hash
algorithm, native source-proof check, WAL ordering, publication fence or page
retirement rule changes.

Two unit tests compare the exact previous rendered-string fingerprint against
the streaming helper for native positional and named rows, empty and large
fields, Unicode/escaping, numeric extremes, floating-point special values,
multiple formatting chunks and successive fields. The 26-test native suite
passes, including cold admission, concurrent preparation, stale generation and
physical-reference rejection, and crash-recovery subprocesses. All 31 tests
across ten integration suites pass: audited HTTP during paused preparation,
concurrent/open transaction crash recovery, GC provenance, idle pressure,
sealed preimages, sparse updates, native allocation/shrink and metadata
allocation/cold recovery. Workspace formatting and locked server/test checking
also pass. Local compiler/test executions use Rust 1.95.0 with one scope at
a time, capped at 8 GiB RAM, 4 GiB swap and two CPUs.

A standalone optimized formatting probe produced identical hashes in all
12 samples and eliminated wrapper allocations. Median old/stream time ratios
ranged approximately 1.09–1.56 for the sampled values. This is algorithm-level
evidence only: neither it nor the memory regression establishes the historical
Platform first-checkpoint latency.

## Decision and next experiment

Keep the allocation improvement separate from the whole-frame text work.
The delta path still reads/decodes the old complete frame, traverses its terms,
serializes the complete replacement and recomputes a changed-source digest.
The next isolated experiment keeps six edits constant while varying the source
size, recording prepared logical bytes separately from allocated file blocks
and checking complete derived-frame equality. A large stable footprint still
needs attribution and reclamation evidence.

The same final artifact must pass the unchanged Auth and Platform acceptance
gates before fleet rollout or downscale. Full payload/index coverage, repeated
load/drain, held-snapshot retention and reuse after release, acknowledged-write
crash recovery and shutdown remain open, as do fleet observation, partitioned
BigQuery coverage and the STG free load/suspend/wake simulation.

Private execution logs: `metadata-fingerprint-red-prep1361.private.log`,
`metadata-fingerprint-green-prep1362.private.log`, and
`metadata-native-prep1363.private.log`, `metadata-integrations-prep1364.private.log`
and `metadata-check-prep1364.private.log` under the existing qualification
directory. These logs and synthetic fixtures contain no production changes.
