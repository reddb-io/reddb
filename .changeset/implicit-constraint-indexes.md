---
"@reddb-io/cli": minor
---

`PRIMARY KEY` and `UNIQUE` declared in `CREATE TABLE` now get a unique HASH index (`<table>_pkey`, `<table>_<columns>_key`), so the constraint is checked with one index probe at any table size and lookups by the key use it. Running `CREATE TABLE IF NOT EXISTS` on an existing table creates the missing index.
