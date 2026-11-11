---
"@reddb-io/cli": patch
---

Preserve disk-resident queue and secondary-index roots across result-cache format upgrades,
defer embedded L2 result-cache publication off the request path, and strengthen checkpoint,
retention, indexed-query, aggregation, startup-I/O, and memory-stat regressions.
