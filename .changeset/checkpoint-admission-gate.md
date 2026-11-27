---
"@reddb-io/cli": patch
---

A statement that changes rows now waits for a checkpoint in progress before it marks its table or opens its transaction, instead of waiting inside with both held. Reads by key of that table keep using the index during the checkpoint; they used to fall back to scanning the table, and a scan that outlasted the checkpoint made it publish the whole table again. Both waits are bounded, so a statement or a checkpoint that cannot get its turn goes ahead as before.
