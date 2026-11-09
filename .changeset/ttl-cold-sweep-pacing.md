---
"@reddb-io/cli": patch
---

Keep individual-TTL cleanup responsive after a disk-resident restart by
limiting each cold maintenance tick to 32 row materializations. Resident
workloads retain the 1,000-row batch, expired rows remain immediately hidden,
and later ticks continue physical reclamation without blocking foreground
queries behind a multi-second cold delete batch.
