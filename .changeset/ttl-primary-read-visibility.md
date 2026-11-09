---
"@reddb-io/cli": patch
---

Hide rows past their `WITH TTL` or `WITH EXPIRES AT` deadline immediately on
primary reads while the bounded background sweeper reclaims them physically.
Apply the same visibility rule to ordinary scans, point reads, grouped
aggregates and paged `COUNT(*)` without slowing collections that have no TTL
metadata. Storage and WAL formats are unchanged.
