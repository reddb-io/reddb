---
"@reddb-io/cli": patch
---

Reject checksummed pages whose embedded identity differs from the requested address or whose LSN cannot be proven by the completed checkpoint or the attached durable WAL. This prevents a misdirected collection page from being served as another table's rows. Complete double-write recovery can still restore a trusted current image before reads begin.
