# Six-edit native full-text scaling diagnostic

Engine source: `689438210c4e72c1c29eee9c5f1444bea40b82ac`.
Standalone harness: [native-fulltext-scaling.rs](native-fulltext-scaling.rs).
Observations: [native-fulltext-scaling-2026-10-26.json](native-fulltext-scaling-2026-10-26.json).
Harness SHA-256: `c310f18618766727fb21dc8357d2707d0a4f63f42b1052cea7363c1f89daa060`.
Executable SHA-256: `9053d23e2e46fa2cd5966a454fb3c13149c3e04c0973031186e82c8b2eb00ca1`.

**Decision:** the existing delta path bounds retokenization but still prepares
a replacement whose logical size scales with the complete index. Stable
physical allocation does not establish bounded CPU/memory work. These small
synthetic results do not qualify the rejected historical Platform candidate,
explain its pre-existing gigabytes, or authorize rollout/downscale.

## Method

Each fresh synthetic paged store has 256, 2,048 or 8,192 table rows and an
explicit unique ID index. A fixed batch generator seeds the data, then seals
the baseline source. Three rounds each edit six rows and supply exactly
324 bytes of new payload text. A checkpoint follows each round.

The harness records checkpoint stage events, pager accesses, free/total pages,
artifact descriptors, and logical length versus filesystem allocated blocks
for every fixture file. Independently rebuilding the full text frame from all
physical source versions must produce identical bytes after every checkpoint.
Live row counts and ID-based payload reads must also pass. After graceful
shutdown/reopen, count and updated payload are checked before another
checkpoint; the first cold checkpoint must then publish an exactly
source-derived frame.

The engine uses a 512 MiB budget. Compilation/execution ran in one local
8 GiB RAM / 4 GiB swap / two-CPU scope, with the existing unoptimized server
library and Rust 1.95.0. The small harness itself is optimized. Library hashes,
source hashes, build/run results and timestamps are retained in the private
receipt. This is a shared-host diagnostic, not an optimized production latency
comparison or a signed release artifact. Autovacuum is disabled to isolate the
publication path, so the 18 newly created historical versions are intentionally
retained; this does not test their reclamation.

## Observations

| Baseline rows | New payload per round | First round prepared text bytes | Main allocated before rounds | Main allocated after round 3 | Round checkpoint times |
| --- | ---: | ---: | ---: | ---: | --- |
| 256 | 324 bytes | 32,938 | 1,474,560 | 1,490,944 | 49.8 / 51.6 / 65.3 ms |
| 2,048 | 324 bytes | 261,570 | 1,916,928 | 1,933,312 | 86.3 / 82.3 / 64.6 ms |
| 8,192 | 324 bytes | 1,048,002 | 3,538,944 | 3,538,944 | 166.0 / 137.4 / 140.2 ms |

The 32-fold row increase makes the first changed frame approximately 31.8
times larger despite the same six edits. In each changed round, stage events
report exactly one prepared artifact with a payload length equal to the entire
text frame and four retained artifacts. The existing implementation reads and
decodes the preceding frame, traverses its terms, encodes the replacement and
recomputes the changed-source digest. The prior native unit test separately
checks that retokenization is restricted to the mutation journal.

**Prepared logical bytes are not measured physical write traffic.** For
8,192 rows, compression places the approximately 1 MiB frame into four
16 KiB pages. Total checkpoint pager accesses were 132 / 127 / 127, and main
allocation stayed exactly 3,538,944 bytes across baseline and all three rounds.
For the smaller inputs, one additional 16 KiB main-file page was allocated.
Those finite samples show reuse, not a general future storage bound.

Every observed WAL had an eight-byte logical header with a stable 16,777,216
allocated bytes. The metadata sidecar grew by about 1.5 KiB logically across
the three rounds while retaining 45,056 allocated bytes; the result-cache
file stayed at 1,097,728 allocated bytes. Manifest/empty collection/audit files
are included in the JSON inventory. The reported freelist count was zero;
retired-page attribution and an exhaustive live-page census are not supplied
by this experiment.

All 12 baseline/round frame comparisons, nine edited-payload checks, row counts
and three cold-recovered payload checks passed. First cold checkpoints took
53.1 / 91.2 / 247.9 ms and each restored full frame/source equality. This does
not replace process-kill recovery or the long-snapshot/reuse gate.

## Preserved rejected attempt and cold normalization

The preceding `prep1367` fixture completed all four 256-row warm frame checks
and recovered its live count/payload, but its direct read of the old persisted
frame before cold publication differed from a derivation of the reopened raw
source. Both frames contained 274 physical documents and 517 terms; 12 terms
had different postings/frequencies. The named-row codec persists named values
instead of the additional positional column representation, while raw native
text derivation currently observes both. Consequently, that raw source can
change representation across reopen and cannot inherit its old proof blindly.

The rejected fixture and both decoded comparison inputs were preserved. The
final diagnostic explicitly verifies user data before cold publication, then
requires exact derived equality after source admission/reconstruction. It
does not treat the earlier mismatch as a successful cold-cache reuse test.
Eliminating this representation-driven cache rebuild is a separate possible
improvement, requiring a small reproduction and compatible provenance checks.
Two earlier harness builds failed at imports before opening any fixture.

## Next decision

The experiment rejects the hypothesis that the current native text delta has
work proportional only to the changed rows. It also provides no evidence of
unbounded main-file growth in these three small rounds. The next index change
must address the full-frame boundary, using independently updatable parts and
durable publication/retirement proofs, rather than merely speeding up another
whole-frame pass. A minimized regression must enforce the chosen work bound
and compare its result to full derivation before a larger candidate is built.

Full historical Auth/Platform qualification on the same final artifact remains
open, including complete payload/index behavior, repeated load/drain, a held
snapshot and reuse after release, acknowledged-write crash recovery, shutdown
and the original 10-second/physical-growth gates. Fleet upgrades/observation,
volume downscale, partitioned BigQuery coverage for all orgs/databases and the
STG free load/suspend/wake experiment remain open.

Private receipts: `fulltext-scaling-receipt-prep1368.private.json`,
`fulltext-scaling-trace-prep1368.private.log`,
`fulltext-scaling-summary-prep1368.private.json` and
`fulltext-scaling-cold-diff-prep1367.private.json` under the existing
qualification directory. No active database or cloud workload was modified.
