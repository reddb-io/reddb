---
"@reddb-io/cli": patch
---

Return unused GNU allocator pages under Linux cgroup memory pressure during paced
background maintenance. Freed native recovery and query buffers no longer need
to remain resident until the container reaches its hard limit. Memory admission,
live allocations, persistent formats and transaction durability stay unchanged.
