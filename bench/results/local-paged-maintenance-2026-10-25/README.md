# Local paged WAL and concurrent read qualification, 2026-10-25

The synthetic table has 1,000 rows, an integer primary key, a bucket index and
512-byte payloads. Whole-table updates change only payload text. Every read must
retain count 1,000 and ID sum 499,500. These are shared-host diagnostics, not an
independent performance study or qualification of deployed production stores.

Released 1.26.8 retained 131,931,435 bytes of recovery WAL after 100 updates and
65 seconds idle. An explicit checkpoint drained logical WAL length to its 8-byte
header in 770 ms. Reserved disk blocks remain a separate measurement.

The proposed automatic worker on source `78aec6f25` drained WAL in three rounds.
The debug run was rejected: its third round returned HTTP 503 during a 33-second
checkpoint; earlier checkpoint times were 10.8, 23.4 and 23.8 seconds. The optimized
run measured 2.055, 4.690 and 7.114 seconds but failed its concurrent-read invariant.
A second instrumented optimized run captured count 89 and ID sum 46,616. Every
post-round full-row and bucket-index check still found all 1,000 rows. Crash/reopen
qualification was therefore not attempted. The optimized fixture used a separate
partition on the same host, so these numbers are not a paired speedup claim.

Released 1.26.8 also failed a periodic-checkpoint HTTP control with ten incomplete
concurrent reads, including count 999. Negative controls observed no mismatch in
3,280 queries without checkpoints or 6,268 queries with faster synchronous
checkpoint boundaries. Those finite negative samples do not exclude the race.
The released manager source and the failing candidate manager were byte-identical
(SHA-256 `bf5d60612e7aa6ef031066372c3f0908a29945d6aecbc8323e063809ac2c5e55`).

The minimized regression pauses a real snapshot scan after its growing row,
then attempts the actual sealed-to-growing relocation. The old path removes the
sealed preimage before waiting for growing's lock: the scan returns `[2]` rather
than `[1, 2]`. It fails in milliseconds without the HTTP transport, WAL pressure,
large datasets or automatic checkpoint worker.

The fix retains the growing topology read gate across sealed probes and publishes
relocation under its exclusive gate. It pins sealed membership against consolidation
and installs the preserved destination before removing its source. It changes no
stored format or WAL ordering. Bounded consumers remain outside storage guards.

The minimized test fails before the fix; all 38 manager tests and all 400 engine
tests pass afterward (two and three existing ignored tests, respectively).
Updated server regressions cover checkpoint policy, worker shutdown, commit
acknowledgement fencing, native shrink, checkpoint/GC provenance, statement
snapshots and the vacuum snapshot horizon. The locked server library check passes.

## Optimized candidate after relocation fix

These runs use source `0cfb3568b18befe9c45e74c6e440e7332a8374b3` and binary SHA-256
`75c393d3b5811aebb19705c011286b60c2f57ef428cb2c763ed4f54067925e42`.
They are local candidate results, not official released-artifact or production
qualification. The configured engine budget is 1 GiB on a shared host, without
a dedicated cloud cgroup. No explicit checkpoint requests were made.

| Workload | Concurrent reads / failures | Query p99 / max | Writer p99 / max | Checkpoints |
| --- | --- | --- | --- | --- |
| Three bursts of 100 whole-table updates | 913 / 0 | 148.120 / 205.029 ms | 459.651 / 1013.693 ms | 1.708 / 3.850 / 7.291 s |
| 180 updates, paced 0.5 s apart | 776 / 0 | 67.092 / 87.797 ms | 2826.060 / 4225.061 ms | 1.783 / 3.160 / 4.386 s |

Both workloads verify every row, payload and bucket-index result after each
round, then survive SIGKILL and cold reopen with an acknowledged uncheckpointed
write. The continuous workload records three writer operations overlapping
checkpoints; its final marker was verified in the stopped process's WAL before
the crash. Maximum allocated footprints were 510,246,912 and 230,170,624 bytes,
respectively. Readers pass, but the 4.225 s writer maximum remains material.

A separate drain/reuse run continues the burst fixture for three more 100-update
bursts, draining between bursts. Automatic maintenance reduces physical history
to 1,119 entities and logical WAL to 8 bytes. The main file is 472,219,648 bytes
after **each** of the three drained bursts: zero additional main-file growth.
Rows, payloads and index checks pass every round. The reused fixture now contains
payload cycle 600; it no longer represents the earlier cycle-300 crash fixture.
The WAL can retain allocated disk blocks despite its 8-byte logical length.
The main-file plateau is reuse of an existing high-water mark, not physical
shrinking or a general disk-space bound; metadata/resident memory are separate
measurements.

## Remaining gates

The draft PR skipped its automatic code CI lane; documentation-shim checks do not
qualify code. An explicit full code CI run and the signed RC workflow later passed
on source `a2d71f940`; the diagnostic events added afterward still need validation.
The actual native-store results below still block stable publication and fleet
rollout.

ADR 0038 records a proposed transitional exception: this worker schedules full
atomic checkpoints. It does not satisfy incremental per-tick maintenance or
provide a hard storage cap. These finite tests do not establish universal closure.

## Signed container and native stores

The canonical RC workflow published `1.26.8-rc.363` from
`a2d71f9405e0e45276d7ed9025b32ba15b619e19`. The RC tag source, binary attestation,
container signature and equality of the container/public binary bytes were
independently verified. Image digest:
`sha256:45ef40952864417c24b4131324e2a19b2e4e4a4e23f7b872ac353a80c12f0046`.
The explicit full code CI passed all 33 jobs on that source. Earlier draft checks
and documentation-shim greens did not qualify the code.

The native Auth copy ran with its live 2 GiB cgroup / 1 GiB engine budget and one
CPU. All 37 original collections, earlier acknowledgements, 1,000 workload rows
and index checks survived; 60 whole-table updates and 310 concurrent aggregates
had no read failures. Query p99/max were 20.21/25.03 ms, writer max 6.659 s.
The WAL drained automatically, the 14,675,329,024-byte main file did not grow,
and the last acknowledged uncheckpointed write survived SIGKILL and cold reopen.
Peak memory was 1,873,731,584 bytes, without OOM. However, actual background
checkpoints took 6.851 and **10.151 s**: strict acceptance against the retained
10 s checkpoint limit **fails**. Raw harness flags covered functional and
explicit-checkpoint tests; separate acceptance evaluates background durations.

The authenticated historical native Platform copy ran with its live 4 GiB
cgroup / 2 GiB engine budget and one CPU, on an existing idle worker. It opened,
verified all 170 original collection cardinalities and preserved the original
acknowledgement, then exhausted its 4 GiB cgroup during the first operator
DDL/write sequence, before any measured checkpoint completed. The engine exited
137 and the client lost its connection; cgroup events recorded OOM kills.
The failing copy remains frozen. This does not yet attribute the failure to the
automatic worker, metadata publication or cache budgeting. A fresh original-copy
control on published 1.26.8 is required before assigning causality. No native
candidate acceptance or live rollout is claimed.

Targeted `reddb::checkpoint` debug events mark store publication, append-only
publication, physical metadata preparation, native artifact snapshots/payload
preparation and native header publication. Enable them for isolated diagnosis
with `RUST_LOG=info,reddb::checkpoint=debug` (and the same CLI log filter when
explicitly set). They report stage, duration and artifact sizes, without payloads
or credentials; they do not change publication order or fix the memory failure.

```bash
cargo test --locked -p reddb-io-engine --lib snapshot_scan_does_not_miss_a_sealed_entity_moving_into_growing
cargo test --locked -p reddb-io-engine --lib
```

## Native publication memory regressions

The released 1.26.8 container also exhausted a 512 MiB cgroup with a 256 MiB
engine budget after acknowledging 60,000 synthetic JSON-text rows. Creating an
unrelated empty table triggers native secondary-artifact publication. Borrowed
row traversal, ownership transfer and direct summary counting remove whole-row
copies, but did not alone pass the constrained case. Building fulltext postings
one source row at a time removes the additional collection of text strings.

A separate allocator regression identified another publication spike: flushing
8,192 dirty 16 KiB pages (128 MiB) allocated 268,500,992 extra bytes. The pager
materialized the entire dirty cache and extended another page-image vector.
Taking compact cache IDs and staging at most 256 page images per batch reduced
the measured extra peak to 5,261,560 bytes. Each batch retains its WAL-before-home
barrier and double-write protection; the header follows all batches. The test
checks every page after cold reopen. Failed resident batches remain dirty.

All 400 engine unit tests, 17 portable integration tests, native artifact shrink,
fulltext allocation/cold-reopen and freelist dirty-eviction regressions passed.
Tests use an owned temporary directory because the host's quota-enabled `/tmp`
returned EDQUOT; the failed attempt is retained separately from its same-source
successful retry. These are candidate measurements, not fleet acceptance.

On source `6ace5e4fd` plus the pager/text-memory patch, both debug and optimized
local binaries acknowledge all 60,000 rows, create the unrelated table and
complete an explicit checkpoint without OOM. Optimized checkpoint time is
1.889 s. The optimized binary still exhausts memory while rebuilding native
artifacts during graceful shutdown; debug completes without an OOM. This is a
remaining acceptance failure. No stable publication or live rollout is admitted.

## Allocator arena isolation and constrained acceptance

A small extracted-routine diagnostic repeats the same 60,000-row fulltext
publication from two retained worker threads and the main thread, reserving a
modeled 280 MiB resident baseline. Default glibc arenas fail with an OOM kill.
Changing only `MALLOC_ARENA_MAX=1` passes all three publications. This diagnostic
uses a modeled baseline and is not an engine acceptance test. Singleton-posting
and compact-buffer experiments did not pass the modeled test and are not part
of this patch.

The actual optimized server then repeats the original workload under the same
512 MiB cgroup, 256 MiB engine budget and one CPU. Its only runtime difference
is `MALLOC_ARENA_MAX=1`: all 60,000 rows are acknowledged, unrelated DDL and an
explicit checkpoint pass, and graceful shutdown exits zero without OOM.
After cold reopen, every original row payload is checked. Five further
checkpoints take 1.786, 1.438, 1.495, 1.324 and 1.363 seconds; the initial explicit
checkpoint takes 1.448 seconds. Allocated main-file bytes remain 131,293,184 in
all five observations. An acknowledged uncheckpointed write survives SIGKILL
and a second cold reopen, followed by another exit-zero graceful shutdown.

The image defaults now bound glibc arenas to one; operators can override that
allocator setting. This permits cross-thread reuse of freed workspaces and is
not a universal process memory cap. The constrained acceptance uses the frozen
optimized binary from the storage-memory patch, before integration of the
subsequent upstream query-sharing commits. The integrated signed RC still needs
its own artifact and native-store acceptance. No stable publication or fleet
rollout has occurred on the strength of these local tests.

## Repeated native-index construction and publication

The integrated, signed `1.26.8-rc.366` passes all 33 full-CI jobs and the
60,000-row constrained image test, including payload validation, crash recovery
and exit-zero shutdown. The original native Platform copy still fails: its
automatic checkpoint takes 87.326 seconds and the operator checkpoint returns
HTTP 503 after the 30-second handler deadline. There is no OOM. All 170 original
collection cardinalities and the earlier acknowledged value remain valid.
The failed copy is retained, staging's temporary memory request quota is restored
to 10 GiB, and no live database is upgraded on this evidence.

A larger synthetic image reproduction uses 300,000 JSON-text rows, one CPU,
a 4 GiB cgroup and a 2 GiB engine budget. Unrelated empty-table DDL takes
23.866 seconds; an explicit checkpoint takes 12.281 seconds. Debug events show
238,102,522 native artifact bytes reconstructed twice for the DDL, taking
10.182 and 11.136 seconds, while the later store-publication phase takes
52 milliseconds. These local data files live on tmpfs: this isolates repeated
derivation cost and does not qualify OCI disk performance.

Publication now retains a successfully published collection's page references
only while its manager identity, segment revisions and exact published artifact
references match. This cache contains no payload copies. It advances only after
durable publication; mutations, collection replacement and changed references
require reconstruction. Cold processes conservatively reconstruct their first
publication: no unverified persisted source proof is assumed.

The blob writer also compares actual encoded page content, including next-page
links and cleared tails. Identical pages do not re-enter the WAL or dirty-page
queue. A regression first reproduced three unnecessarily dirty pages; with the
fix an unchanged three-page blob leaves zero dirty pages and changing one byte
dirties exactly one page, preserving the changed payload after cold reopen.

On storage source `5ad97818e`, all 21 targeted server unit tests and `cargo check`
pass. The optimized clone of the same 300,000-row fixture takes 9.080 seconds
for the first cold checkpoint, then 33.612, 37.482, 34.730, 35.464 and 33.662 ms
for five further checkpoints. Unrelated DDL takes 40.823 ms. Main allocated bytes
stay exactly 650,526,720; all-file allocation stays 668,467,200. Every payload is
independently checked, an uncheckpointed ACK survives SIGKILL and a second cold
open, and graceful shutdown exits zero without OOM. These are local tmpfs
measurements, not acceptance of the original OCI Platform copy.

## Bounded full-text updates

Signed candidate `1.26.8-rc.369`, source `4072816ca`, passes all 33 full-CI
jobs and the 60,000-row / 512 MiB constrained image check, with crash recovery,
two cold opens, exact payloads and exit-zero shutdown. Main-file allocation
remains 131,211,264 bytes across five checkpoints. Its fresh historical Auth
restore nevertheless fails the strict gate: maximum writer latency is
10.570 seconds and automatic checkpoints take 10.735 and 8.049 seconds.
All 37 collections and concurrent reads remain correct, without OOM. The
failed fixture is preserved and is not reused for subsequent mutations.

A native profile finds 166,872 records in `red.control_events`, six more than
the original restore. Its full-text frame is 42,995,659 bytes. The changed
collection still requires complete full-text derivation even for these six
events. The proposed journal-based path removes changed IDs from published
postings and tokenizes only their current rows. Both paths sort postings by
entity ID, preserving the existing frame format. This still decodes and
encodes the entire bounded index; it is not an in-place index-page update or
a hard per-checkpoint time limit.

Admission requires the same manager, complete mutation journals, exact published
references and a volatile proof that all counted documents had postings.
Nonempty documents with no tokens require full derivation on the next change.
JSON/blob additions, other models/artifacts and oversized inputs also fall back.
The encoded source is capped at one eighth of the engine budget or 64 MiB,
whichever is smaller; changed text is capped at one eighth of that limit or
8 MiB. Source-row estimates are checked before payload copies. Cache proofs
still advance only after durable publication; the first cold publication is
conservative. Native Auth and Platform acceptance remain open.
