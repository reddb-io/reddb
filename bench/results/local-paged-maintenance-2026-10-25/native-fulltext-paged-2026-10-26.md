# Native-pager part publication experiment

Runtime source remains `689438210c4e72c1c29eee9c5f1444bea40b82ac`.
The experiment runs on evidence checkout `b5ab05ca1`, whose engine sources
are unchanged. It links the existing native Pager, BTree, PageStore,
TransactionManager and Checkpointer. The node/manifest representation is
experimental, stored in fresh dedicated files; it is **not a native full-text
runtime integration or a managed RedDB database format**.

Source: [native-fulltext-paged-probe.rs](native-fulltext-paged-probe.rs).
Observations/hashes: [native-fulltext-paged-2026-10-26.json](native-fulltext-paged-2026-10-26.json).
Measured executable SHA-256:
`98a43c35c65bdc0e4bcee724f1e898db1ef00e3ffa8bd2831f396c94f8221444`.

**Decision:** packed part updates, held logical roots and native page-WAL
recovery work in this component experiment. Do not integrate this particular
representation into the runtime yet: its initial footprint is high, and its
retirement queue is volatile. Preserve these results as a durability/work
oracle while qualifying the already CI-approved runtime separately.

## Method and publication

Immutable logical term/posting nodes are records in the existing B-tree, so
they share physical 16 KiB pages rather than each consuming a page. A reverse
document-to-term map is stored alongside those nodes. The experimental
manifest binds the B-tree root, logical root, document count, canonical frame
length/CRC and next logical node identity.

A PageStore overlay buffers changed existing pages. Allocation reservations
are flushed while changed old pages remain private; then a native page
transaction commits node records, reverse records, B-tree structure and the
manifest together. The existing TransactionManager makes its page images and
commit durable in WAL before applying them to the pager. Native pager flushing
retains its double-write path, and the existing Checkpointer drains recovery
WAL. Physical frees follow committed publication.

Last-reference drops enqueue logical node identities. Each publication drains
at most 4,096 identities, deleting only records actually persisted by that
experiment. Held roots retain their nodes, so they cannot be deleted early.
This demonstrates a lifetime-driven queue in a single-process component test;
it is not yet durable retirement inventory or production reclamation policy.

Each of three fresh inputs has 256, 2,048 or 8,192 documents. Twelve rounds
overwrite the same six documents with fixed-length values. The first logical
root stays pinned through round three; its complete frame is read from disk
and compared before release. After every round, the current disk frame must
equal independent full derivation byte for byte and match its CRC. All three
fixtures also cold-reopen with their reverse mappings intact.

## Space and update work

| Documents | Initial main allocated | Main after round 12 | Main growth over 12 rounds | Six-document transaction pages |
| --- | ---: | ---: | ---: | --- |
| 256 | 1,540,096 bytes | 1,572,864 bytes | 32,768 bytes | 3–5 |
| 2,048 | 4,456,448 bytes | 4,489,216 bytes | 32,768 bytes | 3–5 |
| 8,192 | 14,467,072 bytes | 14,499,840 bytes | 32,768 bytes | 3–5 |

Each input stabilizes in its later rounds. In the largest input, rounds 7–12
all end at exactly 14,499,840 allocated bytes. These are observed finite
plateaus, not a general hard space bound. Main logical lengths equal allocated
lengths in these synthetic files.

Typical later updates in the largest input persist 19 new node records plus
six inverse records, 2,327 key/value bytes, and delete 19 obsolete node records.
They consume three page images (49,152 bytes) in the transaction. Releasing the
initial reader makes 43 additional old records deletable and touches 20 page
images in its separate reclamation transaction. Smaller inputs release 33 and
39 old records. The preselected 32-page component ceiling passes for every
six-document update and reader-release publication.

**Transaction page images are not total physical write traffic.** WAL framing,
compression, double-write staging/clearing, allocation reservation, flushing
and the explicit drain add work beyond those counts. This experiment does not
measure syscall/block-level I/O amplification. Drained page WAL contains a
37-byte checkpoint marker while retaining a stable 16 MiB allocation.

Most reclamation here reuses cells inside live B-tree pages. Only the smallest
input frees one complete physical page during its measured rounds; the other
two free none. Thus the plateau must not be described as a general native
freelist or orphan-page reclamation result.

The initial 8,192-document projection contains 65,542 logical node records,
860,160 inverse-value bytes and 6,144,500 key/value bytes, but occupies
14,467,072 main-file bytes. Its canonical legacy text frame is approximately
0.85 MB and the previous SQL diagnostic compresses a different approximately
1 MiB frame into four pages. Those are different inputs/layouts and not a
paired storage comparison, but they expose substantial initial overhead in
this representation. Bulk packing/chunking, compact identities and actual
compression need investigation before adopting it; stable recurrence alone
does not make this cost acceptable.

## Recovery and preserved copies

The completed 8,192-document fixture is stopped, copied to three fresh
directories and byte-checked unchanged after the child processes. Each child
stages round 13 and exits without destructors at a declared boundary:

| Boundary | Child exit | Recovered result | Page images replayed |
| --- | ---: | --- | ---: |
| Before commit, only Begin made durable | 72 | Round 12 frame and inverse map | 0 |
| After native commit, before normal flush | 73 | Round 13 frame and inverse map | 3 |
| After commit plus one modeled complete home-page write | 74 | Round 13 frame and inverse map | 3 |

The third case deliberately writes one correctly checksummed committed page
image to its home offset and synchronizes it, then exits. It models incomplete
multi-page application; it does not inject a torn sector or intercept the
production double-write routine. The original stopped fixture and all fault
copies remain preserved. All 49 emitted observations pass their assertions.

## Limits and next gate

The inverse map is durable here, but cold update setup materializes the tree;
it is not a bounded lazy node loader. Source provenance, collection-wide
BLAKE3/metadata scans, active source transactions, query integration, budget
admission, format ownership/versioning and downgrade behavior are not changed.
Uncommitted allocation reservations and unreachable node records still need
a durable bounded recovery/reclamation protocol. A process-local drop queue
cannot prove space reuse across repeated crashes. Reference-drop cascades for
large retained roots also need a bounded-work mechanism.

The executable is an optimized driver linked to the existing unoptimized
native library, under Rust 1.95.0 and one 8 GiB RAM / 4 GiB swap / two-CPU
scope. It is not a production latency comparison. The checked-in source differs
from the measured private source only by a repository-relative baseline module
include; both hashes are recorded.

The RC377 build uses the already 33/33-CI-approved runtime commit and excludes
this experiment. Its historical Auth/Platform qualification remains required
on the same final signed artifact, preserving the 10-second/64 MiB gates,
complete payload/index checks, repeated load/drain, held transactions, recovery
and shutdown. Fleet upgrades/observation, historical cleanup, OCI downscale,
partitioned BigQuery usage and the STG free load/suspend/wake mission remain
open.

Private execution receipts are `native-fulltext-paged-receipt-prep1373.private.json`,
`native-fulltext-paged-results-prep1373.private.jsonl`, and their build/run logs
under the existing qualification directory. No active database was used.
