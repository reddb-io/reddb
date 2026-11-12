---
"@reddb-io/cli": minor
---

Bind native paged data, indexes, retired generations and double-write recovery to independently sealed catalog and superblock authority. Reject checksum-valid stale pages and recovery frames before they can replace current data, and preserve recovery evidence after interrupted writes. Preparing a legacy store also authenticates retained generations without moving their data or releasing active readers. Older readers reject the new required capabilities at open.

Guarded readers and writers also reject a truncated reserved double-write area before serving surviving stale home pages, without consuming recovery evidence.

Preserve streaming MCU recovery for legacy files, and reject prepared batches or incompatible read budgets before they can bypass guarded recovery.

Keep initial tree insertion separate from the existing-tree buffers so cold multimodel checkpoints can allocate generation proofs on ordinary worker stacks.

Upgrade notes: legacy files remain readable and keep their existing addresses during preparation. Once a store enables the new catalog, tree-proof or double-write publication capabilities, older readers reject it at open. Retain a verified pre-upgrade backup; downgrading requires restoring that backup or exporting verified data into a compatible store. Reverting the application binary does not downgrade the file. Existing Rust `DatabaseHeader` struct literals and exhaustive `DatabaseHeaderError` matches remain source compatible; publication extensions use additive accessors and a separate error type.

The existing header decoder keeps rejecting publication-specific capabilities even when an older engine links this newer file codec. Participating engines use the explicit publication-aware decoder, which validates catalog and recovery extensions before page access.
