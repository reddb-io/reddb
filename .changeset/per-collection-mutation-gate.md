---
"@reddb-io/cli": patch
---

The indexed-mutation gate is scoped to one collection: a write on one table no longer forces UPDATE/DELETE target selection on other tables to scan.
