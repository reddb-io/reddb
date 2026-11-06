---
"@reddb-io/cli": patch
---

Recover a closed store with pending WAL without dropping its verified active or rotated audit logs, preserving byte parity and hash-chain continuity in the published copy.
