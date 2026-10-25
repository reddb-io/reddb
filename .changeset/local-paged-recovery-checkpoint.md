---
"@reddb-io/cli": patch
---

Schedule local recovery-WAL checkpoints for writable standalone paged CLI servers without remote backup. Skip empty WAL and pace retries by size and dirty age; preserve the existing durable publication and shutdown ownership.
