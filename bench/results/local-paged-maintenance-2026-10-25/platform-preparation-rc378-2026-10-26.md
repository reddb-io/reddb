# Platform warm recurrence diagnostic: RC378

Runtime source: `bae760f43730ac419832fe6723f07f0b7290af18`.
Image: `ghcr.io/reddb-io/reddb@sha256:c2ff72d6fd9ade0687c57f7e9a80c2e7fb46bcfa0e91c8ce46031f9fad071942`.
Binary SHA-256: `1d3089b05b55fc85e343f1f0ca022379a0675e0b47f662db0fa86d11e8e14d84`.
Full CI `40382040196` passed 33 jobs; Release Candidate `40384361495`
passed six. Tag/source, binary attestation, image signature and image/binary
equality were independently checked before the experiment.

**Decision: rejected for fleet rollout and volume reduction.** The first
requested checkpoint took 13.580 seconds, exceeding the unchanged 10-second
limit. No active database received this artifact. Auth qualification on this
exact artifact remains outstanding.

## Controlled experiment and attribution

The existing Job `stg-platform-preparation-prep1395`, UID
`89da89d6-22ab-47a4-886f-fe34baacf8de`, used a new sparse, hash-verified copy
of the same frozen historical input as RC376/RC377. The source already has
warm native artifacts; this does not measure original legacy materialization.
The engine retained one CPU, a 2 GiB engine budget and 4 GiB container limit.
The frozen source WAL was eight bytes. Cloning/hashing 153 files took 741.070
seconds. Opening/readiness took 78.064 seconds before the measured requests.

| First requested checkpoint | RC377 | RC378 |
| --- | ---: | ---: |
| Native preparation | 9.776 s | 9.629 s |
| Physical metadata preparation | 3.613 s | 3.502 s |
| Complete client-observed request | 13.828 s | 13.580 s |

RC378 prepared two artifacts, retained 111 and prepared 56,363,508 logical
bytes. These are single observations, not a statistical speedup claim. The
prepared size is compatible with the previously profiled 56,352,779-byte
`red.control_events` text frame plus `red_config`; the current trace does not
individually attribute hashing, retained-payload validation and construction.

All 30 fixed-size updates passed (maximum 10 ms). Five further checkpoints
took 3.385 / 3.260 / 4.209 / 3.437 / 3.305 seconds. All eight issued logins
passed (maximum 994 ms). Every cycle verified its 512-byte payload and
`index_seek` query plan. The original acknowledged value `1267869` survived.
Graceful shutdown took 3.596 seconds, with WAL flush/final checkpoint complete.

## Main-file and WAL accounting

| Observation | Main logical bytes | Main allocated bytes | WAL logical bytes |
| --- | ---: | ---: | ---: |
| After open | 17,714,823,168 | 5,914,206,208 | 887 |
| First requested publication | 17,714,823,168 | 5,915,074,560 | 8 |
| Fixture and each of five drained cycles | 17,714,823,168 | 5,915,131,904 | 8 |

The first publication added 868,352 allocated bytes; fixture/cycles added
57,344 bytes (56 KiB). No further main-file growth occurred between the five
drained cycles. WAL allocation remained 16,777,216 bytes. This does not explain
the existing 5.9 GB physical allocation or prove boundedness for every workload.

## Preservation and operational cleanup

After the engine exited, the frozen result was compressed/encrypted into the
separate internal staging R2 bucket `reddb-internal-backups-stg`, archive
`b4837286169a427680fdd8b60841d12f`. All 154 archived files, totaling
17,715,403,147 logical bytes, passed remote object reread and reconstructed
file-hash verification. Source manifest SHA-256:
`3d0cb77cf34cf7747ab40e1c96d41fa2ba78f37b9da033e0cafe9b207a949deb`.
This is remote reconstruction/hash validation, not an additional physical
restore-to-disk rehearsal. Earlier rejected inputs remain preserved.

The original observer rejected Kubernetes' omission of empty policy lists;
its resumed observer retained the same Job/Pod/NetworkPolicy identities and
admitted DNS/HTTPS only after the engine terminated. The Job completed at
20:07 UTC. Quota restoration initially stopped at its live-Platform readiness
guard during the separate staging OOM recovery rollout. After Platform was
Ready, the original guarded restore succeeded; independent readback confirmed
`requests.memory=10Gi` at 20:09:27 UTC. No exception remains for this Job.

## Next discriminating experiment

Cold native-source admission is lazy. A real `RuntimeLedger` audit insertion
before the first checkpoint may invalidate the persisted proof, forcing a
whole-frame rebuild. This is a hypothesis, not a reproduced finding yet.
Use a small ledger fixture, compare warm one-event publication against cold
reopen plus one `AuthLogin`, check delta row counts, exact frame contents and
event recovery. Measure opening plus first publication together so moving
work to startup cannot be mistaken for reduced work. Do not build another
full candidate before this experiment distinguishes the mechanisms.

Held-snapshot reclamation, acknowledged-write crash recovery, representative
load, unchanged durability ordering and the exact final artifact on both
Auth/Platform remain required. Fleet upgrades, OCI downscale, metering gaps
and the free-database exercise remain pending.
