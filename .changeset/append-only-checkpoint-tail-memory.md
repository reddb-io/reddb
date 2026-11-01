---
"@reddb-io/cli": patch
---

Avoid cloning already published append-only payloads during checkpoints. Preserve physical row order while materializing only new segment rows, reducing temporary memory and work under the publication fence.
