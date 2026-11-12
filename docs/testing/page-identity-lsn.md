# Page identity and durable LSN authority

Run locally:

```console
make test-page-identity
```

The functional runtime tests checkpoint two tables, a UNIQUE HASH index and a BTREE index. They
replace a collection's leaf with another table's complete leaf, or stamp `u64::MAX` as the page
LSN and recompute its checksum. Previously a cold SQL scan served the foreign table's values,
or accepted the impossible LSN. Both faults must now fail with a named integrity error while
preserving the original data file and WAL bytes.

Checksummed reads compare the embedded page ID with the requested address before caching bytes.
Ordinary page writes check identity before capturing undo or changing the cache. The encrypted
host checks plaintext identity after authentication and before encryption; ciphertext is never
interpreted as a plaintext page header.

For nonzero LSNs, the shared pager asks its host for durable authority. The default host accepts
only LSNs at or below the completed physical checkpoint. The native host can also use the
attached WAL writer's durable boundary. This check does not flush the WAL or advance a boundary.
A cold page beyond the checkpoint without an available WAL authority is refused. The existing
WAL-first regression additionally reopens a real stamped page with its durable WAL attached and
requires the read to succeed. LSN-zero pages retain the existing DWB contract.

The positive recovery journey stages a complete current DWB image before damaging its home page.
Cold reopen must replay that trusted image, return exact HASH/BTREE/scan results, verify persisted
indexes, checkpoint, and survive another cold reopen. Without that recovery evidence, the
authoritative page is refused rather than recreated.

## Remaining generation-proof gap

Identity and LSN validation do not detect a previous valid image of the same reused physical page
when its LSN is zero. A functional counterexample reproduces an older value after a confirmed
MVCC update: checkpoint until the allocator reuses a recorded collection-page address, replace
the current page with that address's old checksummed image, then read at cold boot.

The current resume catalog stores root addresses, counts and index definitions; it does not seal
every expected page image independently. A stamp read from the suspect page itself is not an
independent generation proof. This case remains unfinished under scenario 2, with its rejecting
regression retained on the follow-up branch. Resolving it requires generation evidence anchored
outside the suspect page, including interior/leaf and overflow ownership; an extra local CRC
comparison alone must not be presented as complete lost-write detection.

Reference fault patterns: [MariaDB's future-LSN test recalculates the page CRC](https://github.com/MariaDB/server/blob/11.8/mysql-test/suite/innodb/t/corrupted_during_recovery.test).
[PostgreSQL's amcheck documentation](https://www.postgresql.org/docs/current/amcheck.html) distinguishes
logical corruption from physical checksum failures. This qualification follows the explicit
misdirected/lost-write fault model in [ADR 0074](../../.red/adr/0074-storage-fault-model.md).

Resource sketch: identity adds one integer comparison after a disk read; nonzero LSNs additionally
consult a durable boundary. Neither path scans the database or allocates a page inventory.

## Independent catalog publication

The follow-up writes catalog generations to newly allocated pages. Both superblock copies carry
the new root address and a digest over the complete physical catalog chain, including continuation
links and page identities. The required catalog-root capability is fenced before dependent bytes.
Page 1 remains the bootstrap for legacy files; a capability-aware reader follows the sealed root.

All new homes cross their durability barrier before publication. Both fallback headers forget the
old catalog before its pages are released. Recovery also fences both headers before releasing an
older catalog after a crash between the two header writes. The SQL crash journey covers publication
before either header, after both, and after only one followed by another recovery interruption and
damage to the newest header. Cold HASH/BTREE reads, exact rows, UNIQUE enforcement, new writes and
another checkpoint/reopen are the required outcomes.

Offline reconstruction uses the same catalog publisher and validates the source catalog digest.
Recovered output with unknown identity high-water marks retains its read-only fence through an
explicit required capability; changing catalog formats cannot silently re-enable writers. See
[ADR 0079](../../.red/adr/0079-fenced-legacy-recovery-output.md).

Resource sketch: publication adds two sealed-header writes and their existing fsync barriers. It
hashes the bounded catalog chain while encoding/reading it; it does not scan collection data at boot.

## Generation proof implementation in progress

The follow-up adds an authenticated page map to the shared B-tree. Its persisted byte contract is
owned by `reddb-file::page_proof`; `BTree::new_verified` and `with_verified_root` route point reads,
cursors and overflow through the same membership and full-image digest check. The independent
`PageProofRoot` must be published with the data root. A page's own CRC or local generation stamp
does not supply that authority.

The map has four fixed radix levels over the physical page ID. An update retains four compact
references, then writes bottom-up. The first change after a seal copies the affected proof path;
later changes use that generation's private nodes. A host must enable deferred reclamation so the
previous published paths cannot be reused before publication. A failed mutation or rejected read
fences that tree against further access and sealing; it cannot bless the suspect image.

The engine journey creates an interior tree, changes an existing row, appends across leaves and
stores an incompressible overflow value. It cold-opens the sealed pair, checks exact values through
point reads before a complete ordered cursor, then substitutes valid-CRC old leaf/interior images
and modified overflow/proof-node images. Every case must reject the read and preserve the source.
A complete known-current DWB image must restore the expected page and survive another cold reopen.
The test also checks that an unchanged page remains readable through the prior proof path after a
copy-on-first-mutation update. This is a proof-path preservation check, not MVCC over mutable pages.

Deleting the last row retires the proof map through the same deferred fence before publishing a
zero root. The lifecycle regression uses an overflow value, cold reopens the populated tree, deletes
it, publishes and releases its complete ownership, then reopens empty and creates the tree again.
Repeated cycles must reuse pages without growing the file. A zero data root cannot restore a
nonempty proof descriptor.

```console
cargo test --locked -p reddb-io-engine --test portable page_proofs_cover -- --test-threads=1
```

The SQL runtime still constructs legacy trees. The reproduced SQL counterexample therefore remains
unfixed until integration completes. Required publication work remains:

- Persist and restore the exact proof descriptor for every live data, key and secondary tree,
  together with retained retired trees; empty trees must not orphan their proof nodes.
- Fence the tree-proof required reader capability in both superblocks before dependent bytes, while
  preserving legacy compatibility and bounded startup/preparation.
- Resolve interrupted proof/catalog publication through the existing WAL ownership and before-image
  protocol. Validate the recovery source before consuming evidence; an old complete DWB frame must
  not acquire authority from its own CRC.
- Make the existing SQL rejection test pass, retain positive HASH/BTREE/scan and UNIQUE behavior,
  and qualify crash, I/O-failure, retirement and reopen boundaries before merge.

Resource sketch for the new map: at most four proof-page reads plus the requested data page, all
using the existing bounded pager cache; no resident page-to-hash inventory. The four compact path
references are under 200 bytes. A populated tree initially needs four 16-KiB proof nodes. Dense leaf
nodes store 32 bytes per protected page, before sparse-node and copy-on-write overhead. Full-image
hashing covers up to 80 KiB per verified read; cold start does not scan data to rebuild proofs.
