---
"reddb": patch
"reddb-io-server": patch
---

Allow offline physical compaction on read-only OCI FSS/NFS sources while still
excluding active writers through the database file lock.
