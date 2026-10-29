# Cold checkpoint preparation admission

Date: 2026-10-26. This correction follows the frozen RC376 runtime source
`1caae8d13f2d97c75f0b018e4b64e10e00069bbf`. It does not transfer RC376's full CI
or artifact qualification to the corrected source. RC376 remains rejected for
its 15.724 s first Platform checkpoint and the cold-overlap case below.

## Reproduction and scope

A six-row native probe used the frozen RC376 source library. Its serial control
retained five native cache references before and after checkpoint. After a
cold open, the first checkpoint was paused before source-proof admission. A
second checkpoint completed without preparation admission and published no
caches because its in-memory proof map was empty. References changed
`5 -> 0 -> 5`, with all six authoritative rows still present.

The original fallback prevented a preparation/publication lock inversion, but
applied the same fallback to ordinary concurrent checkpoints. This unnecessarily
discarded cold persisted caches and forced reconstruction. The probe proves
avoidable work; it does not attribute every historical storage increase to
this case or demonstrate loss of row data.

The new native regression fails before the correction at its requirement that
an ordinary second checkpoint wait outside publication for the admitted
preparation. This isolates the case without a large corpus, CPU saturation,
network queue or active database file.

## Correction and invariants

An ordinary checkpoint takes the preparation gate before acquiring the
row/WAL publication fence. A competing checkpoint therefore waits before
publication while the first admits cold proofs and publishes reusable caches.

A nested caller that already owns the publication fence only tries the
preparation gate. It retains the existing conservative fallback rather than
waiting for a preparation that might need its fence. The implementation checks
ownership by the current thread, not merely whether some thread holds the
reentrant mutex.

The gate is acquired before the publication guard and released after it.
Authentication and row/WAL mutations do not acquire preparation admission.
Source generation validation, protected native-chain reads, force-sync,
double-write barriers and final publication/retirement order are unchanged.
No persistent format or timeout changes are introduced.

The native regression checks that:

- the second ordinary checkpoint waits while preparation is deliberately
  paused;
- the row/WAL publication mutex remains available during that wait;
- all native reference fields remain identical during and after both
  checkpoints, comparing descriptor order canonically;
- all six source rows remain and persisted artifact bytes match a full
  current-source derivation;
- the separate nested-publication test still completes without waiting under
  the row/WAL fence.

The focused corrected test passed in 5.64 s, including its intentional
five-second admission pause. This is not a measured five-second checkpoint.
The full 24-test native suite passed in 19.24 s, and the 30 relevant
integration tests passed again, including the affirmative audited
HTTP case, concurrent/open transaction crash recovery, GC provenance,
idle pressure, sealed preimages, sparse updates, allocation and shrink/reopen.
Locked checking and workspace formatting also passed. Executions retained
the one-scope-at-a-time 8 GiB RAM/4 GiB swap limits.

## Remaining work

The correction prevents this cold-checkpoint overlap from discarding caches.
Nested publication can still defer unproved persisted caches; continuously
changing sources and their cold costs require final qualification. This is
not a complete incremental native-index architecture.

The [RC376 warm Platform diagnostic](platform-preparation-rc376-2026-10-26.md)
found 57,344 additional main-file allocated bytes across fixture setup and
five drained cycles, with no further growth between cycles, but its first
checkpoint exceeded the unchanged 10 s limit. The next performance experiment
must address its measured 9.411 s preparation and 5.865 s metadata costs.

Qualify the same final artifact in Auth and Platform, including complete
payload/index behavior, representative repeated load and history drain, a
long snapshot and reuse after release, acknowledged-write crash recovery and
shutdown. The full fleet, downscale, partitioned BigQuery resource/config/
engine/AI coverage and STG free load/sleep/wake requirements remain open.
