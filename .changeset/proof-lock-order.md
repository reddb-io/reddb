---
"@reddb-io/cli": patch
---

Fix a deadlock that left the server idle and unready: a read that had to load a page of a proof-carrying tree waited for the pager ownership epoch while holding the tree proof lock, and a publication (checkpoint, registry flush, count projection step) holding that epoch waited for the same lock to seal the tree.
