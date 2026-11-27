---
"@reddb-io/cli": patch
---

A checkpoint no longer copies a whole collection because a few hundred of its rows changed. The staged copy of a primary tree is made only when the incremental path lacks what it needs to apply the change in place (it was made past 512 changed rows), the WAL references that path relies on are budgeted from the page cache the store actually runs (about 660 row images store-wide before), it accepts up to 65,536 changed index entries, the pages of a replaced tree are returned from its proof inventory instead of being read one by one, and a collection that is republished in full is logged with the reason.
