---
"@reddb-io/cli": patch
---

Reject new mutations and checkpoint retries after a page writeback failure until the database is reopened. Recovery uses the existing WAL and double-write buffer so a partial page or catalog write cannot leave later successful mutations queued behind an unpublishable checkpoint.
