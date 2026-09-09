# Bounded candidate cursor for exact vector search

Exact vector search previously collected every visible physical ID before
hydrating batches. For 16,384 candidates the ID vector alone requested a 128 KiB
allocation, even with top-k three. The segment cursor fixes membership and append
boundaries once, then hydrates at most 256 physical positions per batch. Scoring,
metadata predicates, RLS and top-k payload copies run outside segment locks.

## Positions and read contract

Bulk-loaded flat slots are already append-only. HashMap entries now have a
shared append-only ID directory, maintained by individual insert, gapped or
HashMap bulk insertion, and consolidation adoption. Physical deletion and
unpublished-merge eviction leave cursor positions in place; lookup skips absent
or tombstoned items. Rebuilding a segment reconstructs the directory. Updates
retain physical identity and do not move positions. Existing recovery insertion
paths reconstruct this derived structure without a new disk/WAL record.

The cursor pins segment references and captures both growing and sealed under
topology guards, in growing-then-sealed order. Sealing holds the growing slot
guard through publication to the sealed list, preventing a capture gap or double
inclusion. Those guards are released before consuming batches. New appends after
the captured boundary are excluded; consolidation can retire sources without
invalidating their captured positions. Retired source payloads may remain alive
until the scan releases its references.

The existing explicit snapshot predicate applies before cloning; exact scoring
rechecks visibility and applies policy before ranking. No policy runs under the
segment guard. The cursor itself is a physical membership boundary, not a new
transaction snapshot or a change to MVCC retention. Internal callers without a
snapshot retain the existing moderation/xmax fallback.

Cooperative work checks cover segments and every physical position, including
hidden versions and tombstones, then scoring. Interrupted batches are discarded;
the runtime propagates the budget error rather than returning partial top-k.
This counts actual inspected positions more strictly than the former visible-ID
collector. Lock waits and atomic payload cloning remain non-interruptible.

## Cost and limits

Cost sketch: candidate work remains O(P) for P captured physical positions.
Additional cursor memory is O(S + B) scalar/buffer entries for S segments and
B ≤ 256, plus selected payloads; it no longer allocates O(N) IDs per query.
The batch buffer is reused and sized to the remaining positions, so empty/small
collections do not reserve a full batch. Payload bytes depend on item width.

The shared HashMap directory costs eight bytes per reserved ID slot, including
Vec capacity slack, accounted in segment resident bytes. Append work is amortized
O(1). Flat-only ingestion adds no directory entries. Deleted directory entries
remain until segment reclamation. This moves memory into shared storage for
HashMap-backed items; it does not remove the database's O(N) resident data.

Pinned retired segments can delay memory reclamation. Existing active-segment
accounting is not a complete peak-memory admission bound for those retained
references. Sealing now holds the growing topology guard through sealing and
publication; seal-heavy write latency needs its own measurements. No stronger
write-throughput, crash-resilience or competitor-parity claim is made here.

Tests exercise flat/HashMap growing and sealed collections, empty/singleton
inputs, gapped bulk IDs, deletion and structural updates, old snapshots,
interrupted batches, reentrant consumer reads/writes, sealing and consolidation
between batches, and rebuilding positions through adoption. Existing vector
MVCC/RLS and persistent reopen suites remain part of validation.

## Allocation probe

A standalone public `search_similar` probe uses two-dimensional vectors, exact
cosine scoring and top-k three, after catalog warmup. Each run returns three
results. Local debug-build calling-thread measurements are:

| Ingestion | Candidates | Parent allocated bytes | Cursor allocated bytes | Parent largest allocation | Cursor largest allocation |
|---|---:|---:|---:|---:|---:|
| Individual | 1,024 | 314,785 | 87,287 | 67,584 | 67,584 |
| Individual | 16,384 | 5,015,125 | 363,767 | 131,072 | 67,584 |
| Bulk | 1,024 | 314,551 | 87,287 | 67,584 | 67,584 |
| Bulk | 16,384 | 5,014,711 | 363,767 | 131,072 | 67,584 |

The total decreases because both the ID list and repeated per-batch container
allocations disappear. Dense values and collection strings still clone during
hydration, so cumulative allocations continue to grow with candidate count.
These measurements are not peak-memory, throughput or latency benchmarks and do
not include ingestion or the shared directory's resident-memory cost.
