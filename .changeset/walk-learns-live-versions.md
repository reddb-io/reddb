---
"@reddb-io/cli": patch
---

After a restart, an UPDATE of a row that had been updated before no longer walks its whole table. The location of a row's live version is a hint kept in memory, so after a restart no row whose live version sits in the disk base had one, and each such row cost a walk of the collection the first time a statement resolved it, with other UPDATEs of the table queued behind. The first walk now records the live version of every row it passes, so a table pays for one walk after a restart instead of one per row.
