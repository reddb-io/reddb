---
"@reddb-io/cli": patch
---

The on-disk `PRIMARY KEY`/`UNIQUE` check no longer holds a scan allowance while it probes the key index. Under sustained concurrent writes that allowance left no room for the probe and inserts failed with "index probe workspace exceeds the shared memory budget".
