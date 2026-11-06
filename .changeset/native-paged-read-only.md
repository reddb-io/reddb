---
"@reddb-io/cli": patch
---

Allow read-only opens of native paged databases, including WAL and interrupted-checkpoint recovery into a private memory view, without rewriting database or recovery files. Enforce shared-reader locking and retain paged indexes in bounded residency mode.
