---
"@reddb-io/cli": patch
---

A read no longer waits for the WAL to cache its result. When writes had been quiet, a cacheable SELECT published its own result to the durable cache tier: a WAL append that waited for an fsync, taken under the publication fence. The statement now keeps its result in memory and the cache sweeper promotes it to the durable tier, as it already did for results cached while writes were in flight. A lookup by key on a disk-resident table went from 2.6 ms to 1.1 ms.
