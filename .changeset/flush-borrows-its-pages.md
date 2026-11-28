---
"@reddb-io/cli": patch
---

A flush no longer holds a second copy of the pages it writes through the double-write buffer. Since 1.43.0 it cloned each batch to separate the pages that skip the frame, which doubled the transient memory of a flush (9.4 MiB instead of 4.5 MiB for a 128 MiB dirty cache). The frames borrow their pages again.
