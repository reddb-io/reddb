---
"@reddb-io/cli": patch
---

Serialize logical-WAL retention with durable appends and indexed replica reads. Prevent a concurrent rewrite from losing acknowledged records, invalidating seek offsets, or allowing an oldest-record lookup to truncate a live append.
