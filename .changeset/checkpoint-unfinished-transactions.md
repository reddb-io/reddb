---
"@reddb-io/cli": patch
---

Exclude unfinished row versions from crash-recoverable checkpoints and retain
their committed preimages. Invalidate checkpoint reuse when transaction outcomes
change, and fence commit WAL publication with visibility so a concurrent
checkpoint cannot discard a durable commit's recovery records.
