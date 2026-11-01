---
"@reddb-io/cli": patch
---

Hash checkpoint metadata without materializing payload-sized strings, preserving the existing root values. Prevent legacy paged metadata reconciliation from resurrecting dropped collections' derived index descriptors, and reclaim historical descriptors only when roots, contracts and removal history confirm the drop. Preserve unrelated orphan and global descriptors for diagnosis.
