# Incremental full-text checksum/tree feasibility

Runtime source remains `689438210c4e72c1c29eee9c5f1444bea40b82ac`;
the baseline evidence checkout is `eb9b2ca7a6cea9041fb299422fb01d0420e70ccc`.
Full CI `40361168504` completed successfully with all 33 jobs on that runtime
source. No runtime code, native page format or deployed workload changes in
this experiment.

Prototype: [native-fulltext-tree-probe.rs](native-fulltext-tree-probe.rs).
Measurements: [native-fulltext-tree-2026-10-26.json](native-fulltext-tree-2026-10-26.json).

## Concrete question

Can a bounded document delta update an exact legacy `RDFT` frame's length and
CRC without decoding, traversing or serializing the whole frame on each edit,
while keeping an older frame available to a reader?

**Algorithm result: yes. Production integration: not implemented.** The
standalone executable uses immutable, copy-on-write balanced trees: an outer
tree orders terms, and each term owns an inner tree of entity-ID/frequency
postings. Each node caches its subtree's canonical byte length and CRC.
CRC concatenation combines child summaries with the changed record. The
resulting root digest equals the checksum of the complete existing byte
format, without serializing unchanged subtrees during updates.

A reverse document-to-term map identifies postings to replace/remove. In this
prototype that map is volatile; its durable equivalent remains required.
Term tokenization calls the current source implementation. The full rebuild
oracle independently tokenizes all input documents and uses the existing
`reddb-file` frame encoder. It checks exact bytes, length and CRC after every
observed update, outside the timed/update-work window.

## Negative control and results

The executable directly compiles the frozen existing
`native_fulltext_delta.rs`. On 8,192 documents, adding six physical versions
prepares 852,706 bytes and deliberately fails an experimental 64 KiB
changed-value ceiling (exit 101). The prototype must pass that ceiling and
a 4,096 tree-node-visit ceiling, then match full derivation.
These experimental work bounds do not replace the mission's unchanged
historical 10-second and physical-growth gates.

For the first of three rounds adding six new physical versions:

| Baseline documents | Complete canonical frame | Changed reachable node values | Reachable new tree nodes | Tree-node visits |
| --- | ---: | ---: | ---: | ---: |
| 256 | 27,362 bytes | 1,378 bytes | 106 | 475 |
| 2,048 | 213,730 bytes | 1,618 bytes | 124 | 637 |
| 8,192 | 852,706 bytes | 1,778 bytes | 136 | 745 |
| 65,536 | 6,816,482 bytes | 2,018 bytes | 154 | 907 |

The largest dataset's three rounds alter 2,018 / 2,090 / 2,162 bytes of
reachable node values, visit 907 / 915 / 919 nodes and retain 154 / 160 / 166
new reachable nodes. Intermediate path-copy nodes also allocate: the largest
rounds construct 807 / 800 / 809 nodes before unreachable intermediates drop.
They are not hidden in the changed-value measurement. Structural envelope,
keys, page packing, inverse-map persistence, WAL and source-proof bytes are
not included in that measurement; it is **not physical write traffic or a
storage reduction claim**.

All four input sizes pass three rounds of exact frame/CRC comparison and
preserve a pinned initial root's exact bytes. Between 57 and 105 nodes remain
reachable only from the held old snapshot, depending on size. Its old root
can no longer be upgraded through a weak reference after the snapshot drops.
This proves in-memory reference lifetime, not native freelist reuse.

A further 200 seeded differential steps cover inserts, overwrites, deletes,
Unicode, duplicate terms, whitespace/empty values, documents with no eligible
tokens and empty deltas. Full byte/checksum equality and tree balance/count
invariants pass each step. The physical source projection and named versus
positional row normalization from the previous SQL experiment are outside
this string-input prototype.

The optimized standalone executable links the existing `reddb-file` codec and
`crc32fast` library under Rust 1.95.0. Execution retains the one-scope-at-a-time
8 GiB RAM / 4 GiB swap / two-CPU limits. These small local update timings omit
I/O, primary data checkpointing, metadata and source-proof generation, so they
cannot establish a RedDB checkpoint speedup. The portable checked-in include
path is compiled and exercised again with the same frozen baseline; both
executions retain separate receipts.

## Decision and integration requirements

The global checksum contract does not mathematically require a full frame
pass. Carry the concatenated length/checksum summaries into a paged projection
and update the affected term/posting paths. Keep full-frame assembly for
consumers that explicitly request the legacy frame.

The prototype's individual AVL nodes must **not** become individual 16 KiB
native pages: that would introduce severe space amplification. The production
design needs packed B-tree pages, bounded split/merge/overflow handling and
a durable reverse lookup. It must bind its root to the exact source and
published generation, protect readers' roots, and retire superseded pages
only after the relevant snapshot/publication horizon. Publication must retain
WAL-before-pages, partial-write protection, home sync and metadata ordering.
Fault-injected partial publication, acknowledged-write crash recovery and
on-disk reuse after reader release are required before rollout.

The existing collection-wide source BLAKE3 scan and physical metadata
fingerprint scan also remain linear. This prototype does not eliminate either
cost or qualify total checkpoint work as proportional to changed rows.
Cold admission must preserve source/content checks; a physical root ID or
volatile revision counter alone is insufficient proof for in-place updates.

Before another final candidate, the next implementation gate is a small
**persisted** part-update reproduction: bounded pages/bytes for six changes,
unchanged snapshot contents while pinned, reuse after release, and old/new
valid state after an interrupted publication. Reuse the proven byte/CRC oracle
and the existing pager/WAL paths rather than treating this in-memory prototype
as durable storage. The native wire/inspection frame stays compatible; a new
on-disk representation still requires versioning and downgrade/rebuild rules.

Same-artifact Auth/Platform qualification, attribution/reclamation of the
historical footprint, fleet upgrades/observation, safe volume downscale,
partitioned BigQuery resource/config/engine/AI coverage and the STG free
load/suspend/wake experiment remain open. RC376 remains rejected for its
15.724-second first historical Platform checkpoint.

Private receipts: `native-fulltext-tree-receipt-prep1370.private.json` and
`native-fulltext-tree-receipt-prep1371.private.json`, with matching red/green
logs and JSONL observations under the existing qualification directory.
