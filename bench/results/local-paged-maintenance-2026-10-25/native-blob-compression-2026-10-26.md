# Native full-text blob compression, 2026-10-26

Question: can lossless physical compression reduce native full-text page
materialization without changing logical index bytes, recovery or page reuse?

The local answer is yes. The original regression produced a 1,574,427-byte
canonical text frame occupying 97 raw pages. With the correction it occupies
one physical page; public reads and cold reopen return the exact original
bytes, logical length and CRC32. This is a deliberately compressible synthetic
fixture, not the historical Platform corpus or a fleet storage forecast.

`reddb-file` owns the `RDBZ` first-page contract. The payload is one Zstd level-1
frame; remaining pages retain `RDBL`. Only full-text payloads bearing the `RDFT`
magic are eligible. Small, oversized and insufficiently compressible inputs
stay raw. The logical descriptor length and checksum remain unchanged, while
its page count records actual physical pages. All artifact admission paths
validate that count independently of logical length.

Readers reject over-budget output before decoder allocation, impose an 8 MiB
window, require one frame with the declared content size, check exact output
length and CRC32, and reject trailing frames. Compressed-chain accumulation is
bounded by decoded length. Runtime workspace uses one eighth of the resolved
engine budget; direct stores default to 64 MiB. This is a per-blob workspace
limit, not a claim that all engine allocations are globally accounted here.

| Local requirement | Evidence/result |
| --- | --- |
| Exact bytes and decoded budget | Three codec tests, including shared fixture |
| Independent reader | Node native Zstd verifies shared page, SHA-256, CRC32 and budget rejection |
| Raw/compressed transitions | Same root; raw fallback and corrupt descriptor rejection pass |
| Failure during compressed expansion | Injected protected home-write failure preserves prior artifact after reopen |
| Cycle safety | Readers and retirement traversal reject cyclic chains |
| Page reuse and retirement | Twelve blob tests; two runtime VACUUM/shrink integrations pass |
| Cold source proof/recovery | Four tests, with six crash subprocess phases including compressed WAL and primary-only publication |
| Snapshot/delta contents | Four native snapshot and two delta-admission tests pass |
| Unchanged-corpus memory | DDL/checkpoint temporary peaks 565,999/586,036 bytes, below the unchanged 16 MiB gate |
| File contract and portable storage | 337 file tests, 54 layout-authority tests and 23 portable tests pass |

The runtime shrink fixture now uses 8,192 rows because compression packed the
old 2,048-row fixture into one page, eliminating its physical tail-retirement
coverage. The original multiple-page/shrink assertions remain unchanged.

The read-only diagnostic profiler was extended separately and preserves the
old profiler. A mixed raw/compressed fixture checks logical/stored/page bytes,
CRC, page-count mismatch, cycles, memory limits and unchanged source mtime.
This avoids interpreting logical index bytes as actual physical allocation.

## Durability and compatibility review

Tail-before-prefix allocation, publication locking, WAL-before-pages,
double-write barriers, final native-header publication and deferred retirement
are unchanged. Compression occurs before page mutation. It introduces no
sidecar. Existing conservative garbage after interrupted unpublished allocation
is still possible; this is not an orphan-page vacuum.

Older engines cannot read `RDBZ`. Rollback requires a compatible engine or a
verified pre-upgrade copy; do not launch an old writer against upgraded files.
Lowering the memory budget can also reject an existing compressed artifact
whose decoded size exceeds the new workspace and must be qualified explicitly.

## Decision and remaining gate

Keep the correction for local qualification. Do not infer historical acceptance
from the synthetic page reduction. Frozen RC373 spent 29.758 seconds deriving
113 artifacts and 87.693 seconds publishing their blobs; compression targets
physical publication and does not remove whole-index construction, decoding
or serialization. Their CPU and memory costs remain proportional to the frame.

The same final signed artifact still must pass Auth and Platform, including
the original strict 10-second and 64 MiB physical-growth gates, repeated writes,
checkpoint/drain, held snapshot, confirmed-write crash recovery and shutdown.
No fleet upgrade or database-volume downscale is justified by these local
results. Telemetry and the free-database simulation remain subsequent work.

Private local evidence: `native-compressed-blob-r1325` under the existing
qualification directory, including the failing-before log, final test logs,
shared fixture producer and independent profiler receipt. No customer payloads
or provider credentials are included in this report.
