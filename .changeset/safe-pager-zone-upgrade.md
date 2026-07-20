---
"@reddb-io/cli": patch
---

Expose the reversible legacy sidecar-to-zoned store conversion as `red migrate-pager-zone --path <FILE>` so applications can safely upgrade pre-1.23 databases before opening them.
