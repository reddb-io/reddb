---
"@reddb-io/cli": minor
---

Qualify RedDB's four deployment-mode contract across the official language
facades. Add the versioned shared ABI for embedded single-file databases,
explicit primary/replica read routing, and term-fenced manual promotion with
post-promotion writes and stale-primary rejection. The promotion response adds
the backward-compatible `term` field; storage and WAL formats are unchanged.
