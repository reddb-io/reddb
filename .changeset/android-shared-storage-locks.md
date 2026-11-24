---
"@reddb-io/cli": patch
---

Open embedded stores on Android shared storage, whose filesystem has no file locks (`ENOSYS`): on Android such lock calls count as taken instead of failing the open with "Function not implemented (os error 38)".
