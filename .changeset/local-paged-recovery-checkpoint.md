---
"@reddb-io/cli": patch
---

Schedule local recovery-WAL checkpoints for writable standalone paged CLI servers without remote backup. Skip empty WAL and pace retries by size and dirty age; preserve the existing durable publication and shutdown ownership.

Keep cross-segment reads consistent while a sealed row moves into the writable segment. Publish its destination before removing the source and retain the topology gate across growing/sealed probes, including key lookups and membership checks.
