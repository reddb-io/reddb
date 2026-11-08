---
"@reddb-io/cli": patch
---

Group concurrent queue READ reservations and ACKs into durable WAL publications on the native paginated store. Validate ownership under the publication fence, preserve per-request ACK errors, and retain the lifecycle path for explicit transactions, ordered keys, expiry and exhausted attempts. Existing `.rdb` and WAL formats are unchanged.
