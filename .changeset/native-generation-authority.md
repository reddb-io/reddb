---
"@reddb-io/cli": patch
---

Bind native paged data, indexes, retired generations and double-write recovery to independently sealed catalog and superblock authority. Reject checksum-valid stale pages and recovery frames before they can replace current data, and preserve recovery evidence after interrupted writes. Preparing a legacy store also authenticates retained generations without moving their data or releasing active readers. Older readers reject the new required capabilities at open.

Guarded readers and writers also reject a truncated reserved double-write area before serving surviving stale home pages, without consuming recovery evidence.

Preserve streaming MCU recovery for legacy files, and reject prepared batches or incompatible read budgets before they can bypass guarded recovery.
