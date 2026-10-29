# Platform warm recurrence diagnostic: RC376

Source: `1caae8d13f2d97c75f0b018e4b64e10e00069bbf`.
Image: `ghcr.io/reddb-io/reddb@sha256:01853a04559e099293c5f79390e1c38cc13efffaa813d48f3ced02d91f14e63d`.
Binary SHA-256: `0dcc20e46e3bac24a9d1d6a7689bea3ed6cbc2085e1e20f7703e6be9d95c38a3`.
The source tag, binary attestation, image signature and image/binary equality
were independently verified. Full code CI passed all 33 jobs on that source;
all six Release Candidate jobs also passed.

**Decision: rejected for rollout.** The first requested checkpoint took
15.724 s, exceeding the unchanged 10 s criterion. A separate small cold-open
overlap probe also exposes unnecessary cache eviction/reconstruction. The
favorable five-cycle recurrence result below does not override either issue
or establish full Auth/Platform qualification.

## Input and method

A new sparse copy of the previously frozen historical Platform input was
created on the existing qualification worker's temporary disk. All 153 files
were independently rehashed against the read-only source, with inode/size/
mtime/allocation invariants checked. The source remained frozen. This is a
**warm artifact** diagnostic: the input already contained native caches from
RC375. It does not qualify first materialization from the original legacy
input, nor does temporary boot-disk storage qualify every live storage path.

The engine retained one CPU, a 2 GiB engine budget and a 4 GiB container limit.
The reporter issued a login alongside each requested checkpoint. After initial
publication, it created a single-row table with a 512-byte payload and unique
ID index, then ran five rounds of six fixed-size updates and WAL drain. It
verified the visible payload/value and an indexed query plan each round.
This is not the representative larger repeated-load or pinned-snapshot gate.

The original acknowledged canary value was preserved. Eight login requests
completed without failure; the largest client-observed login time was 1.104 s.
The parallel requests establish concurrent issuance, not a precise server-side
phase barrier. The separate deterministic native HTTP test supplies that
causal evidence. The engine retained its ordinary 30-second HTTP deadline.

## Results

| Cycle | Requested checkpoint | Main allocated bytes after drain | WAL logical bytes | WAL allocated bytes |
| --- | ---: | ---: | ---: | ---: |
| 1 | 3.681 s | 5,915,131,904 | 8 | 16,777,216 |
| 2 | 4.702 s | 5,915,131,904 | 8 | 16,777,216 |
| 3 | 3.721 s | 5,915,131,904 | 8 | 16,777,216 |
| 4 | 3.853 s | 5,915,131,904 | 8 | 16,777,216 |
| 5 | 3.664 s | 5,915,131,904 | 8 | 16,777,216 |

All 30 updates succeeded; the largest observed update time was 5 ms. Across
the fixture setup and five cycles, the main file added 57,344 allocated bytes
relative to the post-first-publication baseline. There was no further main
allocation growth between the five drained observations. WAL returned to its
8-byte logical header while retaining a stable 16 MiB allocation reservation.

| Observation | Main logical bytes | Main allocated bytes |
| --- | ---: | ---: |
| After opening | 17,714,823,168 | 5,914,206,208 |
| After first requested publication | 17,714,823,168 | 5,915,074,560 |
| After fixture and repeated cycles | 17,714,823,168 | 5,915,131,904 |
| After graceful shutdown | 17,714,823,168 | 5,915,168,768 |

First publication added 868,352 allocated bytes; fixture/cycles added 57,344;
shutdown added 36,864. The total after-open-to-shutdown change was 962,560
bytes. The main logical length never changed. These are main-file and WAL
observations, not an exhaustive accounting of every auxiliary file or a claim
that the pre-existing multi-gigabyte footprint has been reclaimed.

## Remaining first-checkpoint cost

| First requested checkpoint stage | Time |
| --- | ---: |
| Native artifact preparation before publication | 9.411 s |
| Initial primary store publication | 0.013 s |
| Physical metadata preparation | 5.865 s |
| Final native header/store publication | 0.165 s |
| Complete client-observed checkpoint | 15.724 s |

The first preparation retained 111 artifacts and constructed two, with
56,363,508 logical payload bytes. It includes cold source-proof admission and
changed-source processing; it is not exclusively index construction. Later
preparations took approximately 2.88–3.94 s, with metadata preparation around
0.71–0.88 s and final native publication generally tens of milliseconds.
The client operation and stage durations have different boundaries and are
not an additive precise CPU profile.

The first latency violation is retained in the rejected diagnostic receipt;
subsequent successful RPC cycles were collected only to distinguish initial
cost from recurrence. They provide no exception to the original acceptance
criterion. Observed readiness wait was 82.063 s on this historical copy, which
also needs separate cold-start analysis rather than a fleet wake-time claim.

## Cold-open overlap finding

A separate six-row native probe linked the frozen runtime source. Its serial
control retained five artifact references before and after checkpoint. With
one preparation paused and a second checkpoint issued after cold open,
references changed `5 -> 0 -> 5`; all six authoritative rows remained correct.
The second checkpoint skipped cold proof admission and evicted the caches,
forcing the first to reconstruct them. This proves avoidable work in that
case, not the cause of every historical storage increase.

The next correction must serialize ordinary preparation before publication,
while callers already holding the row/WAL fence must never wait on a
preparation that could need that fence. Keep the positive audited-login,
changed-source rejection and crash/reuse gates alongside the new cold test.

## Instrumentation and operational state

The earlier temporary warm attempt had completed its clone and opened the
engine, then stopped before workload operations because the reporter demanded
16 GiB free again after copying approximately 5.9 GiB. The new reporter keeps
16 GiB for initial admission and 8 GiB as its post-clone runtime reserve.
Four small tests preserve the 7 GiB main-file ceiling, recovery-WAL ceiling
and original 64 MiB growth limit. This is a diagnostic guard correction,
not an engine growth fix.

No new persistent volume was provisioned. The temporary memory admission used
the already merged IaC patch; it was restored to the original 10 GiB request
quota after the diagnostic terminated. The live writer remained outside this
experiment. Private receipts are under `platform-preparation-prep1354` in the
existing qualification directory. Full payload/index coverage, a long held
snapshot, process-kill recovery and the same final artifact in Auth remain
required, followed by the original fleet, downscale, BigQuery and free gates.
