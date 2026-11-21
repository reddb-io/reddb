---
"@reddb-io/cli": patch
---

Retention and MVCC reclaim keep up on a busy node. Their background ticks only tried the publication fence and skipped the tick whenever a statement held it, so under sustained load they almost never ran: a table with a retention window kept growing and old row versions were not reclaimed. A tick now waits up to 50 ms for its turn.
