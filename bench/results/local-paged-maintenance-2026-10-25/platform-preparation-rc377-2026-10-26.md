# Platform warm recurrence diagnostic: RC377

Runtime source: `689438210c4e72c1c29eee9c5f1444bea40b82ac`.
Image: `ghcr.io/reddb-io/reddb@sha256:6b19bbfe62d454f5a97aa8a6f9e69fa88103914fc104ad03a8278e7a4710ded2`.
Binary SHA-256: `cc8ae4b4020109a174a81400630bda55462a5855c11e629e7ecf65d7b6c8b8d7`.
Full CI run `40361168504` passed all 33 jobs; Release Candidate run
`40368554063` passed all six jobs. Tag/source, binary attestation, image
signature and image/binary equality were independently verified.

**Decision: rejected for rollout and volume reduction.** The first requested
checkpoint took 13.828 seconds, exceeding the unchanged 10-second limit.
The stable five-cycle allocation result does not override this failure or
qualify historical Auth on this artifact.

## Controlled comparison

Diagnostic `stg-platform-preparation-prep1378` used a new sparse, rehashed
copy of the same frozen historical input as RC376, on existing temporary
worker storage. The original and rejected copies were preserved. Limits
remained one engine CPU, a 2 GiB engine budget and 4 GiB container memory.
The source already contained RC375 native caches: this is a warm-artifact
comparison, not original legacy materialization or live-PVC qualification.

The fixture and workload were unchanged: one indexed row with a 512-byte
payload, five rounds of six fixed-size updates, checkpoint and WAL drain.
Every round checked contents and an indexed query plan. The original
acknowledged canary survived. All eight issued logins succeeded; the maximum
observed login took 829 ms. Concurrent issuance is not a server-side phase
barrier. Readiness wait was 80.062 seconds.

## Latency attribution

| First requested checkpoint stage | RC376 | RC377 |
| --- | ---: | ---: |
| Native artifact preparation | 9.411 s | 9.776 s |
| Initial primary publication | 0.013 s | 0.013 s |
| Physical metadata preparation | 5.865 s | 3.613 s |
| Final native header/store publication | 0.165 s | 0.168 s |
| Complete client-observed checkpoint | 15.724 s | 13.828 s |

RC377 retained 111 native artifacts and constructed two, with 56,363,520
logical payload bytes. The complete request includes other stages, including
blob publication; the table is not an additive CPU profile. This single
comparison supports lower observed metadata time, not a statistical speedup
claim. Native preparation still dominates the failing first checkpoint.

The five subsequent checkpoints took 3.434 / 3.284 / 3.397 / 3.343 / 3.314
seconds. All 30 updates succeeded, with a maximum observed time of 5 ms.

## Logical length and physical allocation

| Observation | Main logical bytes | Main allocated bytes |
| --- | ---: | ---: |
| After opening | 17,714,823,168 | 5,914,206,208 |
| After first requested publication | 17,714,823,168 | 5,915,074,560 |
| After fixture and each of five drained cycles | 17,714,823,168 | 5,915,131,904 |
| After graceful shutdown | 17,714,823,168 | 5,915,156,480 |

First publication added 868,352 allocated bytes; fixture/cycles added 57,344;
shutdown added 24,576. Total after-open-to-shutdown change was 950,272 bytes.
There was no further main allocation growth between the five drained cycles.
WAL returned to eight logical bytes with a stable 16,777,216-byte allocation
reservation. Graceful shutdown completed in 3.461 seconds with WAL flush and
final checkpoint reported complete.

These measurements distinguish main allocation, main logical length and WAL.
They do not explain the pre-existing 5.9 GB allocated footprint, establish
physical write traffic, classify every auxiliary file, or demonstrate that
all workloads now have bounded growth.

## Next decision and operational state

The next latency experiment must separate source-proof scans from changed
artifact derivation/serialization inside native preparation. Keep the source,
input, limits and storage fixed, and use a small reproduction before another
complete candidate. Streaming fingerprints did not remove the dominant native
preparation cost. The paged-tree prototype remains unadopted because its
initial footprint and durable retirement protocol are not qualified.

Full payload/index coverage, representative repeated load, a held long
snapshot followed by reuse, acknowledged-write crash recovery, and the same
final artifact on Auth remain mandatory. No live database was used in this
diagnostic. No persistent volume was added. The owned restoration driver
confirmed the original 10 GiB request-memory quota at 17:54:09 UTC on
2026-10-26. The existing live stores were not upgraded to RC377.

The independent STG Auth expired-interaction fix is rio-lair PR #790,
delivered at source `00e6e0213c40893df1813fdd8812b7eb970d64ce`. Its browser
recovery and API availability results do not qualify this engine candidate.
