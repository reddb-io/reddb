---
"@reddb-io/cli": patch
---

Refuse a second writer process on an embedded single-file `.rdb` instead of letting the last one to close silently discard the other's commits, and reuse checkpoint space so the file no longer grows by a full snapshot on every open/close.
