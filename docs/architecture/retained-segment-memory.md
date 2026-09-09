# Segment memory across consolidation and reader lifetimes

Memory admission must not treat a segment swap as immediate reclamation. Exact
vector cursors can still own the old sources, while paced consolidation also
owns an unpublished copy. Both allocations remain part of the segment arena.

## Accounting contract

Collection resident usage includes growing and sealed segments, retired sources
still held by readers, and the in-flight merged segment. Tombstones, the retained
source registry, and consolidation ID/source buffer capacities are included in
the estimate. The shared arena sampler uses this resident number, matching the
collection statistics instead of dropping tombstone memory at the admission seam.
Entity counts and query membership still describe active storage only.

Retirement registers weak references only when another owner holds the source.
Sampling and subsequent swaps prune expired entries. Multiple readers of one
source charge it once; ending one reader does not release that charge while
another reader remains. The registry never keeps payloads alive. Its retained
vector capacity remains charged even after the last source expires.

Sampling acquires locks in consolidation, growing, sealed, retired order. A
consolidation tick retains its guard through publication, preventing the merged
allocation from disappearing between the unpublished and active inventories.
Retirement registration happens under the sealed publication guard, preventing
an active-to-retired accounting gap. Consumer callbacks remain outside these
locks. Sampling can wait for a maintenance tick or publication to finish.

The existing `consolidation.bytes_reclaimed` counter reports the logical
source-to-merged footprint reduction. It is not immediate physical reclamation
while readers hold the sources. Admission refreshes live usage instead of taking
that counter as available capacity.

After pressure reclamation, admission still requires used bytes plus estimated
growth to fit the budget. Merely bringing current usage below the ceiling does
not authorize an operation that would cross it again. Rejections retain the
existing didactic budget error; a later operation can succeed after readers exit.

## Cost and limits

Cost sketch: a sample is O(S + R + C) in active segments, retained sources, and
consolidation source descriptors; it visits no entity payloads. Retirement adds
one weak handle per retained source, with amortized vector growth at publication.
It adds no per-candidate cursor allocation and no WAL or network work. Buffer
capacity estimates are shared storage accounting, not per-reader copies.

This closes lifetime-accounting and post-reclamation admission gaps, not a hard
process RSS bound. Existing entity/allocator estimates remain approximate. Query
result buffers, collection removal with outstanding handles, and maintenance
headroom before allocation still need their own admission work.
[Concurrent growth reservations](concurrent-memory-reservations.md) now protect
the existing runtime admission paths. Sampling an unpublished copy after a paced
tick is not reserving its peak allocation in advance.

## Verification

Deterministic tests hold two real scan cursors across consolidation and release
them separately, check the unpublished copy between paced ticks, and reject a
growth request that cannot fit even after successful reclamation. Validation
also exercises existing pressure, memory-statistics, storage, vector/MVCC/RLS,
graph, SQL and no-malloc regressions.
