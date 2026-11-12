---
"@reddb-io/cli": patch
---

Fence serverless snapshot publication with immutable payloads and a conditional canonical head claimed by each writer lease. Hydrate the exact observed snapshot before serving, prevent stale uploads from replacing a successor's state, and publish flush/checkpoint/shutdown through the same fence. Operational serverless boots isolate their local cache from previous WAL tails.

Drain all writers before upgrading a shared database: older binaries do not understand the canonical head. This change protects snapshot publication; remote WAL durability before acknowledging writes remains separate work.
