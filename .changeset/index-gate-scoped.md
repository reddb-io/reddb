---
"@reddb-io/cli": patch
---

A statement in flight on one table no longer turns reads of other tables into scans, and a read of the same table waits for it instead of scanning. An autocommit UPDATE or DELETE now names the collection it changes, so only readers of that collection treat it as a writer their indexes may be behind. A single-table statement outside a transaction that finds such a writer hidden from its snapshot waits up to 100 ms for it to finish and reads a fresh snapshot, which takes milliseconds where the scan took seconds.
