# Canonical query response memory — 2026-10-27

Complete canonical replies retained both the executor result and a JSON value
tree for every returned record. A synthetic database containing 50,000 rows,
an integer ID and twelve 32-byte text fields exhausted a 512 MiB server during
`SELECT *`. The main file was 50,495,488 bytes. The configured engine budget
was 256 MiB and the logical pools reported about 121 MiB before the request.

## Attribution

Failure control: signed `1.26.8-rc.382`, commit
`734ffa081093a048792f44a8470abeb891874761`, image
`ghcr.io/reddb-io/reddb@sha256:6e6192124b723dd56a67a182570bd786daa0ab78ae0c13126110cbc2270c8f90`.
Every control used a separate copy of the same stopped synthetic database,
one server CPU, 512 MiB memory with swap disabled, and a 256 MiB engine budget.
The following controls did not load an allocator profiler:

| Request | Result on RC382 |
|---|---|
| `SELECT * FROM probe ORDER BY id` | Server OOM; connection closed |
| `SELECT * FROM probe` | Server OOM; connection closed |
| Same request with result caching disabled | Server OOM; connection closed |
| `SELECT id FROM probe ORDER BY id` | 50,000 rows returned |
| `SELECT * FROM probe ORDER BY id LIMIT 100` | 100 rows returned |
| Same complete SQL through the RedWire summary reply | Execution completed; subsequent COUNT remained 50,000 |

The complete response path was necessary for this failure. Sorting was not.
Disabling result caching did not fix it; ordinary SELECTs above five rows are
not admitted to the result cache. No engine cache change is included here.

## Native regression

`canonical_envelope_peak_memory_tracks_encoded_output` exercises the actual
canonical frame renderer with 2,048 records and twelve text fields. Its
thread-local allocator measures additional live allocations while rendering;
the fixture itself is outside the measured scope. These are allocation peaks,
not process RSS. Before and after use the same test profile and flags.

| Measurement | Before | After |
|---|---:|---:|
| Encoded payload bytes | 1,127,665 | 1,127,665 |
| Additional peak allocation bytes | 8,465,026 | 1,829,505 |

The test fails before the change and passes after it, with byte-identical
payloads. The renderer now writes records individually, sharing the existing
record conversion and envelope metadata. HTTP preserves its byte cap and
RedWire stops at its existing 16 MiB frame limit, including the 16-byte header.
Oversized replies produce errors rather than successful partial documents.
The 28 presentation tests cover payload types, descriptors, ASK envelopes,
exact byte limits and early rejection of an oversized frame. Locked library
and test checking also passes.

## Local complete-process checks

The fix was checked with a locally built development binary on the same
container base, not a published release artifact. This is diagnostic behavior
evidence; its timings are not a release-performance comparison against RC382.
The server remained at one CPU, 512 MiB and a 256 MiB budget. The external
test client had separate memory headroom to parse the complete response.

| Request with the local fix | Outcome | Server peak MiB |
|---|---|---:|
| RedWire, ordered 20,000-row reply below the frame limit | Every returned ID and all twelve text fields matched | 405.4 |
| RedWire, complete 50,000-row request | Existing frame limit rejected the reply; subsequent COUNT was 50,000 | 390.4 |
| HTTP, complete 50,000-row reply | HTTP 200; every ID and all twelve text fields matched | 407.0 |
| HTTP, same query with a 10,000-byte result limit | HTTP 413 with `payload:result_bytes:10001:10000`; subsequent COUNT was 50,000 | 348.3 |

No server OOM occurred in these four checks. The 50,000-row JSON reply exceeds
the existing RedWire frame size; the test does not raise that limit or silently
truncate rows. The HTTP case verifies the complete dataset. The signed final
artifact must repeat these checks before rollout.

## Remaining memory work

A separate 10,000-row allocator diagnostic returned 25,112,576 anonymous bytes
to the OS through a controlled `malloc_trim(0)`, with identical live arena
allocation before and after. Payload hashes and acknowledged-write recovery
also passed. Only the adjacent before/after trim samples support that number;
the larger drop between coarse workload stages also included natural allocator
reclamation.

This demonstrates retained free heap as one contributor, not the explanation
for every resident byte. The executor result and output buffer still reside in
memory. Complete intermediate-query accounting, large individual values,
concurrency, high resident record costs and cold-open latency need their own
qualification. These checks do not approve fleet rollout or OCI downscale.
