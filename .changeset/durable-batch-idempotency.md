---
"@reddb-io/cli": patch
---

Persist batch idempotency receipts in `red.batch.idempotency` in the same transaction as the inserted rows. Retries after a lost response, process restart, or writer takeover now replay the committed response without duplicating data.
