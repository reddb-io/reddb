---
"@reddb-io/client": patch
---

Defer reuse of physically removed B-tree and overflow pages until their removal
from the collection catalog is durable. Preserve pending reclamation across
reopen with an optional paged-metadata ledger, and publish physical delete
batches without serializing unchanged survivors. Add crash/reuse, tree-shrink
and repeated-reopen storage regressions. Older readers can ignore the new
ledger; older writers do not preserve its reclamation state, so writer
downgrades require a separately qualified recovery procedure.
