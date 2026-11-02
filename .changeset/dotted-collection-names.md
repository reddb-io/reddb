---
"@reddb-io/cli": patch
---

Fix inconsistent support for dotted collection names across creation, CRUD, joins, indexes, keyed commands, queues, search, and catalog selectors. Bare paths, separately quoted segments, and whole quoted collection names address the same collection. Keep the red system namespace reserved and preserve existing column and KV key syntax. Storage and wire formats are unchanged.
