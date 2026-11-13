---
"@reddb-io/cli": patch
---

Continue logical replication after promotion without reusing an already applied LSN. Restore the published and allocated log frontiers from the local applied position and durable promotion history, even when the former replica has no primary WAL spool.

Keep replication slots and WAL retention on the promoted timeline across cold restarts. Preserve ancestor slots within the shared history, invalidate divergent acknowledgements, and reject slot histories unrelated to the current branch.

Retire the promoted node's former upstream slot before publishing its new timeline, so it cannot retain WAL indefinitely as a consumer of itself. Other replicas keep their retention positions.
