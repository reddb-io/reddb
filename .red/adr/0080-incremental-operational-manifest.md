# ADR 0080: Incremental operational manifest publication and revalidation

Status: accepted for implementation

The version-one operational manifest rewrote all collection and segment metadata
for every change. Retention also buffered that JSON before decoding it. For N
entries and K one-entry changes this caused O(N × K) serialized I/O; version two
writes O(K) changed records plus a constant-size, fsynced publication head.

`manifest.json` version two contains an epoch, committed journal length,
generation and checksum. `manifest.records` contains length-prefixed,
CRC32-checked change records, ending each transaction with its generation.
Collection entries, segment entries, retirement high-water marks and fork origin
remain owned by the file layer. Retention/state changes have separate records so
they do not rewrite a segment's bloom filter or checksum list.

Publication appends changes, syncs the journal and its directory, writes and
syncs `manifest.json.next`, atomically replaces `manifest.json`, then syncs the
directory. The caller continues to fence all manifest writers. Readers ignore
bytes past the published length. A retry overwrites only that unpublished tail.
Committed corruption or truncation fails recovery before artifact deletion.
The pending-drop publication still precedes file removal, and the final
publication follows segment-directory synchronization.

Recovery parses individual records rather than one aggregate JSON document.
The retained collection/segment index is still proportional to live metadata;
the change does not promise constant-memory catalogs. Warm handles replay only
the newly committed suffix. The store shares this baseline across DDL, flush,
recovery and retention instead of constructing an uncached handle per operation.
Maintenance retains its handle and caps replay I/O
per tick. It completes a pinned prefix before refreshing a newer head, so a
concurrent publisher does not restart every partial cold read. It must observe
a current head before publishing a proof or deleting anything.

Historical JSON is validated with its original checksum rules and accepted
unchanged on read-only recovery. Its first mutation writes the initial journal
and atomically publishes a version-two head. Reading/validating that legacy JSON
is a one-time compatibility exception; subsequent mutations never serialize the
whole manifest. Older binaries reject version two: a downgrade requires a
pre-upgrade backup. Offline copies validate ownership through the file-layer
reader and copy the complete operational directory, including the journal.

The journal retains committed history; this change does not introduce journal
garbage collection or periodic whole-manifest rewriting. Cold recovery performs
linear replay of that history. Journal compaction, if added, must use a separately
published epoch and preserve crash safety and bounded maintenance work.

Segment revalidation reads at most 1 MiB from disk and processes at most 1 MiB
of decompressed row JSON per tick. A bounded pipe feeds the header parser across
ticks; it retains only validation metadata, ignoring bloom/key strings without
buffering the raw header. Dropping an unfinished reader closes and joins its
parser. Hexadecimal row fields decode incrementally into a single binary row.
The runtime accepts the native B-tree value ceiling (256 MiB, twice that in JSON
hex plus framing), rather than the former 16 MiB JSON ceiling. Zstd uses its
standard 128 MiB streaming window ceiling instead of 8 MiB. Native version
validation retains an explicit 1 GiB allocation-accounting ceiling for the
encoded value and decoded representation, replacing the arbitrary 64 MiB cap.
Inputs exceeding these resource ceilings still fail closed. A completed proof
requires every frame/checksum, identity, canonical native encoding and timestamp
interpretation to agree; partial work never authorizes retention.

Regression coverage includes legacy migration, unchanged cold-boot files,
bounded suffix reads, unpublished tails, committed corruption/truncation,
publication faults in both retirement phases, interrupted header parsing, a
header and row above 16 MiB, a Zstd window above 8 MiB, and a native decode above
64 MiB across cold boot. Linux CI must validate recovery before merge.
