---
"@reddb-io/cli": patch
---

A checkpoint no longer copies a whole collection because a few hundred of its rows changed. The incremental path now takes up to 8192 changed rows of a collection (it stopped near 512, and at about 660 row images store-wide), the staged copy of a primary tree is made only for a change that path would decline, the pages of a replaced tree are returned from its proof inventory instead of being read one by one, and a collection that is republished in full is logged with the reason.
