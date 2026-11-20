---
"@reddb-io/cli": patch
---

Fix UPDATE, DELETE and unique-index checks missing rows after a crash (1.39.11 regression). The HASH index overlay introduced in 1.39.11 started empty for a collection with a disk base, so rows recovery replayed from the WAL were unknown to it: a statement that reached them by an indexed key found nothing until the next checkpoint. The overlay now starts from the rows resident at boot.
