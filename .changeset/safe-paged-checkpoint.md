---
"@reddb-io/cli": patch
---

Preserve the published tree generation when a paged checkpoint fails. Build changed trees separately, synchronize their pages before publishing the catalog, and reuse unchanged trees to avoid rewriting the database on every checkpoint. Serialize borrowed entities to reduce checkpoint memory overhead.
