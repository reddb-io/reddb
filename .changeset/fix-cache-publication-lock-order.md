---
"@reddb-io/cli": patch
---

Prevent durable Blob Cache publication, result-cache promotion, and database
write invalidation from deadlocking under concurrent traffic.
