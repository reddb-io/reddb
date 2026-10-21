---
"@reddb-io/cli": patch
---

Enforce the configured read-result byte ceiling with bounded JSON serialization,
add materialized-read concurrency admission, and apply cooperative query deadlines
to existing read work checkpoints. Output streams charge cumulative payload bytes.
Mutation acknowledgements remain outside read-result limits; operator/cgroup and
materialization limits remain necessary for memory and non-cooperative work.
