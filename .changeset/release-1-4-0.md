---
"@reddb-io/cli": minor
---

1.4.0 minor release. Raises the storage engine page size from 4KB to 16KB
(matching InnoDB's default) for higher B-tree fanout, and grows the maximum
inline attribute value from 1024 to 4096 bytes (now derived as `PAGE_SIZE / 4`).

**Breaking on-disk format change:** databases written by 1.3.x and earlier use
4KB pages and will not open under 1.4.0 — there is no in-place migration. Treat
this as a fresh-format release.
