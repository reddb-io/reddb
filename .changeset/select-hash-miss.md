---
"@reddb-io/cli": patch
---

`SELECT ... WHERE <indexed column> = <value>` no longer walks the whole collection when no row has the value. An empty answer from the HASH index was treated as "index unusable" and fell back to a full scan, so every existence check on a large table cost seconds. The index answer is now final when the literal has the declared type of the column and no index mutation raced the lookup, which is what UPDATE and DELETE already did.
