---
"@reddb-io/cli": patch
---

Fix a regression where any autocommit INSERT turned every indexed SELECT and every UPDATE/DELETE target lookup in the database into a full scan until the next UPDATE, DELETE or explicit transaction.
