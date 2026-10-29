---
"@reddb-io/cli": minor
---

Bound recovery, catalog statistics and optional search-artifact memory; preserve recovered document, PATCH and KV fields; and recover sparse structural checkpoints without copying entire collection trees. Add repeated crash, page reuse, storage plateau and memory regressions across native and browser engines.

Upgrade readers before opening databases written by this release. Paged format `0x0001_0003` requires checkpoint before-image recovery, and embedded collection contracts use logical WAL action tag 7. Older readers reject these contracts. Preserve a consistent full pre-upgrade backup, including WAL and metadata; downgrading requires restoring that backup. Qualify mixed-version replication before rolling out to replicas.
