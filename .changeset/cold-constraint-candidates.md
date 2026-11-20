---
"@reddb-io/cli": patch
---

Checking a PRIMARY KEY or UNIQUE constraint against rows on disk uses a HASH index over the same columns when one exists, instead of a pass over the collection. The constraint's own rule still decides the conflict.
