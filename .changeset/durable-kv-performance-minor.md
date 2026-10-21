---
"@reddb-io/cli": minor
---

Release the KV/SQL performance and durable-write cycle as 1.25: publish
single-file KV replacements atomically, share durable grouped commits, avoid
redundant overflow checkpoints and reserve bounded WAL headroom. Preserve both
durability barriers, fence uncertain publication failures until reopen, and
restore indexed SQL/KV reads and recovery metadata. Existing storage and wire
formats remain unchanged. Full snapshots still cause pauses; measured results
and upgrade boundaries are documented in docs/release/v1.25.0.md.
