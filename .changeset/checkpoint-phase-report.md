---
"@reddb-io/cli": patch
---

A checkpoint that holds writers for 200 ms or more now logs how long each of its phases took (`checkpoint held writers`): WAL sync, collections, catalog, indexes, publication, WAL truncation and base installation.
