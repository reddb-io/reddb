---
"@reddb-io/cli": patch
---

Fix PRIMARY KEY and UNIQUE constraints being ignored for rows that live on disk in bounded disk residency: duplicates were accepted and ON CONFLICT inserted a second row after a checkpoint and restart.
