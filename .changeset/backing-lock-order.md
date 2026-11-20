---
"@reddb-io/cli": patch
---

Fix a second deadlock between a checkpoint and readers of an on-disk collection: the checkpoint waited for the collection base (to append in place, or to install the new base) while holding the pager ownership epoch, and a reader holding that base waited for the epoch to load a page. The in-place append now declines a base that stays in use, and the new base is installed after the epoch is released.
