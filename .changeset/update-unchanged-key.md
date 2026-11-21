---
"@reddb-io/cli": patch
---

An UPDATE that leaves a PRIMARY KEY or UNIQUE column as it was is no longer refused because another row already holds the same key. Engines before 1.39.11 did not check keys against rows on disk and could admit such a duplicate; since the check was added, every UPDATE of an affected row that named the key column (`SET id = id, ...`) failed with "primary key violated", including the ones that would repair it. A key the statement does not change cannot be newly violated, so that rule is skipped for the row. Taking a key another row holds, by UPDATE or INSERT, is refused as before.
