---
"@reddb-io/cli": patch
---

Reclaim obsolete table history in bounded background batches for writable
standalone embedded runtimes. Avoid copying row payloads during VACUUM and
planner statistics collection, and bound cardinality and histogram working sets.
Rebalance deleted B-tree pages iteratively so cascading merges do not retain
page buffers from every level on a maintenance worker's stack.
Reuse trees after completed physical delete batches instead of re-encoding
unchanged survivors at the next checkpoint, with revision checks that retain
full validation after untracked edits.
