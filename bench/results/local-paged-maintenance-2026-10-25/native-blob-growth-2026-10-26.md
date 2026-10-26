# Native blob allocation/write amplification, 2026-10-26

Historical Platform logs on frozen `8e97eaaa346578b04353321dbb2f15f29de98e7d`
separate the rejected first publication into 5.461 s of physical metadata,
29.758 s deriving 113 artifacts (419,621,489 bytes), 87.693 s publishing their
blobs and 0.132 s publishing the final native header/store. These are old-source
measurements, not a latency result for this correction.

The small reproduction writes 512 native blob pages with 32 cache slots, then
checks every byte and cold reopens the file. Test-only observation at the native
VFS records successful home-page writes and synchronization calls. Previously,
the writer allocated every page before filling any of them. Cache pressure
therefore wrote the complete blank generation before the payload generation.

| Same 512-page fixture | Native home-page writes | Blank home-page writes | Native VFS sync calls |
| --- | ---: | ---: | ---: |
| Before | 1,024 | 512 | 102 |
| After | 528 | 16 | 57 |

The unchanged failing I/O-count regression passes after the correction. These
are operation counts, not an extrapolated cloud speedup, a disk-capacity result
or final Auth/Platform acceptance.

## Failure ordering

The first one-page-lookahead implementation reduced I/O but failed a new fault
injection: a protected home write interrupted while growing an existing blob
could replace its old prefix before all new pages had been allocated. Recovery
then encountered an incomplete chain. That intermediate implementation was
rejected locally and was never released or deployed.

The final writer builds the new tail first, with one-page allocation lookahead,
and only then updates the previously published prefix. The injected home-write
failure during tail allocation now preserves the exact old payload after DWB
recovery. Successful growth/shrink, identical-byte skips, deferred retirement
and native source proofs retain their existing contracts. No allocator,
WAL barrier, DWB frame/sync, checksum or persistent format is changed.

This does not promise rollback of every partial in-place native publication.
Existing failed unpublished allocations can still leave conservative garbage;
source proofs continue to reject stale or corrupt derived artifacts. The new
test specifically protects the old prefix during expansion allocation.

Local validation includes eight native blob tests, four native-source tests,
36 native pager tests and the portable pager I/O conformance suite. Runtime
integration also verifies VACUUM/shrink and cold reopening in both paged
profiles. Full first-publication construction remains linear; its CPU cost,
the original physical-growth gate and same-final-artifact Auth/Platform
qualification are still required before rollout/downscale.
