---
"@reddb-io/cli": patch
---

Allow explicit recovery of pending WAL on an isolated copy while retaining the canonical system configuration. Reject WAL actions and cross-references targeting the divergent legacy identity instead of silently redirecting them.
