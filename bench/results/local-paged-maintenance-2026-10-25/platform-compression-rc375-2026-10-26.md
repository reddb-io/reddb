# Historical Platform compression experiment: RC375

Artifact source: `dd5e81d475ac6e6438947f2e289278ad52eee0cc`.
Image: `ghcr.io/reddb-io/reddb@sha256:2d9298f8e46f12fc4e0ef00d46eff7ab7707fa85ed53c1f5e5e94f62ba9290d5`.
The image signature, binary attestation and image/binary equality were verified.
All 33 full-CI jobs and all three Browser Foundation jobs passed on this source.

**Decision: rejected for rollout.** Initial native materialization now fits
the physical-growth gate, but the automatic checkpoint still took 50.782 s
and a concurrent login returned HTTP 503. The repeated-write, held-snapshot,
confirmed-write recovery and same-artifact Auth requirements remain open.

## Input and scope

A new isolated sparse restore used the same authenticated source manifest
and `.rdb` SHA-256 as the preceding historical Platform experiment. The restored
disk files were independently rehashed. Limits remained one engine CPU, a
2 GiB engine budget and a 4 GiB container limit, on the same qualification
worker-pool configuration and an existing OCI block-storage class. Both live
internal databases remained unchanged and ready.

This historical copy contains 516,917 physical records and 416,945,064 encoded
payload bytes after shutdown. Those are historical physical records and encoded
values, not a current fleet measurement or a count of customer-visible rows.
The largest sources include observability/monitoring exports, usage records
and control events. Their primary trees include many overflow pages; their
logical reachable page size is 2,563,440,640 bytes.

## Physical attribution after initial publication and graceful shutdown

No test canary writes were reached. The first automatic checkpoint and a final
shutdown checkpoint completed. A separate read-only job validated reachable
page checksums, all native artifact logical checksums, compressed frames and
the source-proof blob against the frozen post-shutdown file.

| Quantity | Bytes |
| --- | ---: |
| Logical native artifact payload, 113 artifacts | 419,621,183 |
| Stored artifact payload, 14 compressed and 99 raw | 82,162,689 |
| Chain capacity, 5,110 pages of 16 KiB | 83,722,240 |
| Allocated bytes attributed to artifact pages by FIEMAP | 83,697,664 |
| Net main-file allocated growth | 22,175,744 |

The descriptor lengths/checksums continue to describe logical bytes. Page
counts describe actual physical chains. The physical allocation change is:

| Category | Allocated-byte change |
| --- | ---: |
| Artifact pages | +83,562,496 |
| Pages previously classified free | -63,037,440 |
| Metadata, including source proofs | +90,112 |
| Double-write zone | +786,432 |
| Native header pages | +81,920 |
| Primary, retired, deferred and unattributed regions | 0 |
| Change in stat-minus-FIEMAP accounting | +692,224 |
| **Total** | **+22,175,744** |

The free-page decrease is a change in ownership to live pages, not bytes
released to the filesystem. The pre-existing 4,147,073,024 allocated bytes in
the unattributed region did not grow and are not declared reclaimable.
This observation fits the unchanged 64 MiB main-allocation gate for this
initial-publication/shutdown scope; it does not prove repeated-load stability.

## Time and availability

| First automatic checkpoint stage | Time |
| --- | ---: |
| Initial local store publication | 0.136 s |
| Append-only publication | 0.003 s |
| Physical metadata preparation | 5.963 s |
| Derive 113 native artifacts | 32.834 s |
| Blob publication call | 1.328 s |
| Final store publication/retirement | 10.514 s |
| Complete automatic checkpoint | 50.782 s |

The blob publication call includes encoding and pager admission; later durable
page work is also present in final store publication. Do not call 1.328 s the
complete durable index publication. Frozen RC373 previously spent 29.758 s
deriving approximately 419.6 MB and 87.693 s in its blob publication call.
These stage measurements explain the remaining bottleneck; they do not
qualify a fleet latency guarantee.

The HTTP listener log appeared approximately 141 s after the container's
second-resolution start timestamp. This includes cold loading and is separate
from a later diagnostic pause. Login returned 503 around its 30 s handler
deadline during the 50.782 s automatic checkpoint. Source inspection shows a
shared publication barrier across the checkpoint and auth persistence paths;
the later [small concurrency reproduction](checkpoint-auth-contention-2026-10-26.md)
confirms that this fence alone can produce the same HTTP deadline failure,
without CPU-intensive construction or a request queue. It does not exclude
additional historical latency contributors. Increasing the timeout is not an
accepted correction.

## Instrumentation limitation

The original reporter reused an old transition loop with a missing extent
cursor advance and paused the isolated engine after opening. A subprocess
regression times out before the correction and passes afterward, including
extent clipping beyond EOF. Only that collector was replaced; the same engine
and database file continued. Its long pause is recorded separately and grants
no latency approval. The automatic checkpoint started after resumption, and
the subsequent login failure stopped the planned five write cycles.

The rejected frozen database remains preserved. Temporary warm-copy attempts
stopped at filesystem capacity/allocation guards before starting another
engine. They are not RedDB growth observations. A separate synthetic copy
probe on the same XFS filesystem reproduced append allocation exceeding a
1.5 GiB ceiling before completing a file with 256 MiB of planned nonzero
content. Presizing a 4 GiB logical file first held its peak allocated footprint
to 269,516,800 bytes, including allocation bookkeeping, with the expected
SHA-256. Seeding a final extent was unnecessary. The corrected copier now
presizes before sparse writes; no engine change or relaxed growth gate is
inferred from that diagnostic fix.

An extracted 166,866-row synthetic construction probe retained identical
75,131,328-byte frames: baseline build/total 4.023/4.902 s, borrowed-key B-tree
3.878/4.760 s, randomized fast-hash build plus canonical ordering 1.610/3.606 s.
It uses Rust 1.99.1 and the existing optimized file codec, not the RC375 build
toolchain. It is an algorithm probe only; no engine patch or historical speedup
is approved from it.

Private evidence is retained under `platform-compression-r1334`,
`platform-warm-r1342`, `platform-warm-r1344` and `native-build-probe-r1343` in
the existing qualification directory. Fleet upgrades, observation/downscale,
partitioned all-org usage reporting and the free sleep/wake simulation remain
required after engine acceptance.
