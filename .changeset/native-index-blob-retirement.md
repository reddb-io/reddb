---
"@reddb-io/cli": patch
---

Terminate shrinking native index blob chains at their new boundary and reclaim detached or removed artifact pages through the durable delayed-free ledger. Preserve exact artifact bytes across checkpoints and restart, reuse reclaimed pages during repeated index churn, and avoid counting free IDs twice when consuming allocator trunks.
