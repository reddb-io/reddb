---
"@reddb-io/cli": minor
---

In bounded disk residency mode, checkpoints can prepare a large changed SQL table in batches. Independent transactions can commit between batches. The checkpoint verifies the table is still current before publishing it. Small, WAL-backed row changes can update compatible cold indexes incrementally, and repeated broad range queries can avoid redundant index probes during publication.
