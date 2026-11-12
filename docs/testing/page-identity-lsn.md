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

The original resume catalog stored root addresses, counts and index definitions without sealing
every expected page image independently. A stamp read from the suspect page itself is not an
independent generation proof. The follow-up now binds tree-image maps to the independent catalog
root; the original SQL counterexample passes. The qualification sections below cover recovery-source
authority, legacy retirement and offline rewrites.

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

## Generation proof mechanism

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

Native salvage of the existing multi-interior-level fixture exposed stack exhaustion while
rebuilding rows through the recovery visitor. Interior split propagation now ascends iteratively
with child addresses and the separator. It no longer retains a 16-KiB page frame for each recursive
parent call, and the split helper returns an address instead of another complete page. The unchanged
deep salvage journey must recover every readable row without increasing the worker's stack size.

```console
cargo test --locked -p reddb-io-engine --test portable page_proofs_cover -- --test-threads=1
```

The SQL runtime now builds verified checkpoint replacement trees and restores their descriptors at
cold boot, including data, key and secondary roots. The `RDTP` catalog extension precedes resume
counters and stores sorted data-root, proof-reference and generation tuples. Its required reader
capability is fenced in both superblocks before proof-dependent pages. Once enabled, the published
base is immutable between checkpoints: embedded writes use their existing resident/WAL path instead
of changing a sealed base in place. Legacy trees are replaced at checkpoint rather than scanning the
database to create proofs during ordinary startup.

Required publication work remains:

- Qualify live/retired proof descriptors, empty trees, legacy preparation and offline rewrites.
- Resolve interrupted proof/catalog publication through the existing WAL ownership and before-image
  protocol. Validate the recovery source before consuming evidence; an old complete DWB frame must
  not acquire authority from its own CRC.
- Retain positive HASH/BTREE/scan and UNIQUE behavior and qualify crash, I/O-failure, retirement
  and reopen boundaries before merge. The original reused-page SQL test passed locally after the
  runtime integration (one selected test, 13.31 seconds); this is not the full scenario qualification.

Resource sketch for the new map: at most four proof-page reads plus the requested data page, all
using the existing bounded pager cache; no resident page-to-hash inventory. The four compact path
references are under 200 bytes. A populated tree initially needs four 16-KiB proof nodes. Dense leaf
nodes store 32 bytes per protected page, before sparse-node and copy-on-write overhead. Full-image
hashing covers up to 80 KiB per verified read; cold start does not scan data to rebuild proofs.

## Local qualification snapshot (2026-11-12)

At `d32a13a3d`, all 51 portable engine tests passed, including the empty-generation lifecycle and
catalog publication I/O matrix. All 54 offline-compaction tests also passed: 53 in the main round,
then the separately selected WAL-over-256-MiB streaming test. This includes native deep-tree salvage,
recovery-only write fencing, persisted indexes, schema, vault, audit and multimodel reconstruction.
`cargo check --locked -p reddb-io-server -j1`, formatting and `git diff --check` passed.

The native page-integrity round at `f398373c3` ran all seven tests without excluding the SQL
counterexample: six passed, including the three catalog publication crash phases; the reused-data-page
test failed by returning `committed-1`. The interrupted DWB recovery I/O matrix passed separately.
The subsequent split-stack change does not integrate runtime tree proofs. These results qualify the
catalog and engine prerequisites, not closure of the remaining SQL generation defect.

## Native integration and offline recovery qualification

At `4f1228b59`, native collection, key and secondary trees restore their independently sealed proof
descriptors. The original SQL reused-page regression passed. The eight-test runtime page-integrity
round subsequently passed, including catalog publication crashes, DWB repair and the resident native
API journey. The resident journey commits an insertion and deletion, kills the process before
checkpoint, verifies that the sealed base image stayed unchanged, and requires exact rows, another
write, checkpoint and cold reopen. Both subprocess phases have a 60-second progress limit. This is
the native resident `UnifiedStore` API; it does not qualify the separate embedded packaging format.

The expanded 48-table catalog crash journey exposed another stack overflow. Proof descent now copies
only the selected compact reference out of its page-reading helper instead of retaining 16-KiB pages
in the caller's stack frame. The unchanged expanded journey passed in 78.04 seconds.

Offline compaction and recovery now restore the source's catalog proofs and build fresh proofs for
relocated output trees. A discarded index's sealed map can enumerate its pages independently of the
damaged index images. Every map node still has to pass its digest, prefix, level and generation checks;
page aliases and out-of-range ownership fail. This read-only inventory cannot clear a failed mutation
fence or authorize sealing a modified tree. Authoritative collection records are verified as before.

All 56 offline integration tests passed across three selections: 52 in the main round (including
the WAL-over-256-MiB journey), three physical-WAL tests, and the new valid-CRC index recovery journey.
HASH and BTREE images with a valid CRC but the wrong content must be detected and rebuilt from verified
rows, preserve the source bytes, return exact indexed values and survive a second cold reopen.
Corrupting the independent proof map as well must prevent publication.

Physical-redo fixtures now seal the expected replacement generation independently, leave its home
page at the previous image, and require committed WAL to repair it while preserving XID/identity floors
and secondary-index parity. A separate negative test supplies a committed, CRC-valid WAL image without
matching published tree authority: recovery must reject it and retain the source and WAL unchanged.
Torn-tail fixtures retain their final catalog page in DWB/WAL instead of relocating data behind the
proof map's back.

After these ownership changes, all 51 portable engine integration cases passed in 187.48 seconds.
`cargo check --locked -p reddb-io-server -j1` also passed. The four existing atomic-API deprecation
warnings remain outside this change.

The DWB follow-up below supplies independent frame authority; a frame's own CRC is not proof of
freshness. The final follow-up covers the legacy preparation and retirement migration/reuse journey.
None of these local
results closes the other promotion, cross-pod retry, concurrent UNIQUE rebuild, backup or multimodel
resilience scenarios.

## Independent DWB authority

The new SQL counterexample stages a complete old frame for a reused MVCC page. Before the fix,
the generation check rejected the read only after DWB replay had overwritten the home and cleared
the evidence. The regression failed its unchanged-source assertion in 13.85 seconds.

Required-capability bit 5 now binds the complete raw frame to both superblock copies through a
length, digest and explicit phase. `Preparing` precedes staging; `Ready` precedes home writes;
`Consumed` precedes clearing. Recovery repeats both phase fences before its own home writes or
clearing. A valid-CRC old frame with a different independent digest fails before mutation, including
read-only recovery. Interrupted staging and clearing are discarded only in phases where the home
barrier permits it. See [ADR 0084](../../.red/adr/0084-independent-dwb-frame-publication.md).

All 55 portable cases passed in 230.18 seconds, including four new DWB journeys: every write/fsync
and either-superblock fallback, complete stale-frame rejection, another crash during recovery,
and final-frame recovery without rolling back previous frames. All nine native page-integrity
cases passed in 226.43 seconds. The interrupted recovery I/O matrix passed in 59.30 seconds with
its injected fsync failures updated to target the actual home and clearing barriers after the new
metadata fences. These are functional recovery tests; no benchmark lane was added.

All 56 offline-compaction/recovery cases passed together in 213.32 seconds, including preserved-copy
recovery after interrupted staging/clearing. The stale-frame native regression also passed separately
with its explicit read-only rejection check in 12.33 seconds. The four portable DWB journeys were
repeated with short staging-header prefixes (0, 1, 4, 5, 8, 9, 12 and 17 bytes); all passed in 9.80
seconds. All 365 existing file-codec tests passed in 7.66 seconds.

## Legacy preparation and retirement

A new native journey constructs a pre-proof file in a fresh test directory using the existing
record/catalog/resume contracts, with its fixed page-1 bootstrap, disk-residency capability, persisted
HASH/BTREE indexes and overflow records. Fixture construction verifies the modern source and creates
new unverified trees; it does not downgrade an existing database or add an alternate runtime writer.

The first run failed in 13.33 seconds: root 67 remained in the retirement ledger of the first protected
catalog without an independent proof. Retained legacy trees now receive proofs over their existing
pages before publication. Identity, CRC, topology and overflow ownership are checked during preparation.
Their data addresses and images remain unchanged, and the prepared handle shares the old reader pin.
Preparation establishes a baseline from the legacy integrity checks; freshness guarantees begin with
the independently published generation, rather than proving history that predates those roots.
Catalogs that contain a proof block require descriptors for every nonzero live/key/secondary/retired
tree; native and offline readers no longer have an exception for unverified retired roots.

The portable preparation journey checks an existing ordered cursor and overflow read across conversion,
unchanged data bytes, release of the reader pin and a verified cold reopen. The first native positive
round passed in 87.66 seconds: crashes before/after publication and loss of the newest one-slot header,
then six cold update/vacuum/overflow delete/recreate cycles per crash phase. Exact indexed/scan rows,
UNIQUE rejection, complete ownership proofs and actual reuse of legacy addresses are required. Every
child has a 60-second progress deadline; file-backed child output avoids a blocked logging pipe.

The expanded native round additionally removes one retired descriptor, then correctly re-seals the
catalog and both header copies. Both native and offline readers must reject that structurally incomplete
generation without altering the source. The patch release entry lives in
`.changeset/native-generation-authority.md`; no version number or release tag is edited manually.

The complete native page-integrity round passed all ten cases in 291.11 seconds, including this
malformed-publication rejection. All 56 portable engine cases passed in 199.54 seconds, and all 56
offline cases passed in 212.00 seconds. The native repeated-recovery I/O matrix passed again in
59.52 seconds. These results qualify the native generation changes; the remaining promotion,
cross-pod retry, concurrent UNIQUE rebuild, backup and multimodel fault scenarios remain separate work.

## Truncated recovery extent

Review found that a short file returned before consulting the independently sealed DWB authority.
Two new integration regressions reproduced the problem in 0.06 seconds: with durable `Ready` headers,
cold open accepted a file cut at the start of the reserved zone and served the surviving old vault
page. A required recovery frame was silently treated as absent staging.

Both recovery entry points now reject a short reserved extent when the guarded capability is present.
Tests cut before the frame, inside its header and payload, and one byte before the zone's end. Raw
recovery and cold pager open, in both reader and writer modes, must fail without writes. A complete
authorized extent still restores the newer image; an unguarded short bootstrap retains its previous
behavior. All seven selected DWB integration cases passed after the fix in 11.63 seconds.

The full portable engine round then passed all 59 cases in 209.03 seconds. The short-extent guard
changes only the previously accepted short-file branch; complete reserved zones enter the same
phase-validation and replay sequence as before.
`cargo check --locked -p reddb-io-engine -j1`, formatting and `git diff --check` also passed.
