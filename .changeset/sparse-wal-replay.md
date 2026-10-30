---
"@reddb-io/cli": patch
---

Reuse unchanged collection trees during committed WAL recovery and project sparse table-row changes using bounded exact WAL images. Keep full validation when proof is unavailable and retain the existing durable page-ownership, undo and catalog-publication ordering.
