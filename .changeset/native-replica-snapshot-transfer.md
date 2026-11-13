---
"@reddb-io/cli": patch
---

Transfer native replica snapshots through a consistent checkpoint copy made with the pager's owned file handle. Keep the image and its logical cursor under the same publication fence. This avoids Windows file-lock failures and prevents a fresh replica from silently falling back to WAL replay with a second replication identity when its snapshot cannot be read.
