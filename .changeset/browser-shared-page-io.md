---
"@reddb-io/cli": patch
---

Share native pager byte I/O, superblock publication and double-write recovery with the WebAssembly core. Keep the native file mutex through protected writeback batches and propagate superblock metadata I/O errors. Persistent formats, WAL-first fences and allocation semantics remain unchanged. The Browser package remains private while storage-backed execution and transactions are completed.
