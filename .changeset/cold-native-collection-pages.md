---
"@reddb-io/cli": patch
---

Read native collection pages from the merged disk and resident view. Preserve
physical ID order, row schemas and exact counts, skip earlier encoded payloads
without decoding them, and propagate cold-read or page-budget failures instead
of returning a successful empty collection.

Preserve recovery column order and the native shape of live rows. Retain the
durable index inventory for cold collections instead of inventing indexes from
an empty resident view. Keep full and savepoint rollback versions protected
from MVCC reclamation until their secondary-index undo completes.
