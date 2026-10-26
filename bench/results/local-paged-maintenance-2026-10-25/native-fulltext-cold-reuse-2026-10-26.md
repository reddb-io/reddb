# Updated columnar rows: cold reuse scaling

Runtime source: `bae760f43730ac419832fe6723f07f0b7290af18`.
Harness: [native-fulltext-cold-reuse.rs](native-fulltext-cold-reuse.rs).
Observations and hashes:
[native-fulltext-cold-reuse-2026-10-26.json](native-fulltext-cold-reuse-2026-10-26.json).

**Decision:** the SQL row-model correction eliminates the reproduced cold
table-cache reconstruction in all three synthetic input sizes. This is not
historical Auth/Platform qualification, a general growth bound, or permission
to roll out or reduce volumes.

## Method and results

The generator, sizes, six edited rows / 324 new payload bytes per round,
three rounds, 512 MiB engine budget and full-frame reference derivation match
the preceding [scaling diagnostic](native-fulltext-scaling-2026-10-26.md).
Every fixture is new. Autovacuum remains disabled, deliberately retaining
18 physical historical versions; this does not demonstrate their reclamation.

The new harness strengthens cold recovery: after checking recovered live
count and the latest payload, it compares the existing persisted frame to
the reopened source **before another checkpoint**. It then requires the cold
checkpoint to retain the exact previous table artifact descriptor. Both
checks pass in all three sizes, alongside every warm frame/content check.

| Rows | Main allocation before rounds | Main allocation after round 3 | First cold checkpoint | Cold prepared bytes |
| --- | ---: | ---: | ---: | ---: |
| 256 | 1,474,560 | 1,490,944 | 43.2 ms | 9,731 |
| 2,048 | 1,916,928 | 1,933,312 | 49.3 ms | 9,731 |
| 8,192 | 3,538,944 | 3,538,944 | 84.4 ms | 9,731 |

Each cold preparation constructs one boot-configuration artifact and retains
four. The unchanged table text frame is retained, not reconstructed. The
preceding runtime instead constructed two and retained three, preparing
43,717 / 272,349 / 1,058,781 bytes in its cold checkpoints. Exact retained
descriptors establish the avoided table publication; timings are single
shared-host observations and are not a statistical production speedup claim.

Compilation/execution used Rust 1.95.0, an optimized small harness and the
unoptimized server library, in one 8 GiB RAM / 4 GiB swap / two-CPU scope.
Source and library hashes and clean runtime source state are retained in the
private receipt. All 15 warm/cold observations passed. No active or historical
database was opened or modified by this synthetic run.

## Remaining work

Changed warm rounds still prepare whole logical table frames: 32,938 /
261,570 / 1,048,002 bytes in round one with the same six edits. Stable main
allocation and successful cold reuse do not make these updates proportional
to changed data. Cold source admission still hashes the collection and
validates the retained payloads; its work is not constant either.

The prior RC377 historical first checkpoint remains a rejected 13.828-second
observation on its own source. This source has not yet passed a signed
historical Auth/Platform comparison. Complete payload/index checks, repeated
load/drain, held snapshots and reuse, acknowledged-write crash recovery,
shutdown and the original latency/storage gates remain required before fleet
updates and volume reduction. Warehouse/resource/AI coverage and the free
load/suspend/wakeup experiment remain open.
