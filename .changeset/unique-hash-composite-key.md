---
"@reddb-io/cli": patch
---

A composite `CREATE UNIQUE INDEX … USING HASH` now keys on every column. It keyed on the first column only, so `(x,'2')` was rejected as a duplicate of `(x,'1')`. A row with a NULL or missing column has no key and never conflicts, as with declared UNIQUE constraints. `UNIQUE` combined with `USING BITMAP`, `SPATIAL` or `H3` is now a parse error instead of being accepted and never enforced. Single-column indexes keep their key encoding, so existing indexes, which are rebuilt from the table rows when a database opens, are unaffected.
