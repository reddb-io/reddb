---
"@reddb-io/cli": patch
---

S3 backup requests can use a mounted, atomically rotated JSON file with scoped temporary credentials. Each request signs the current session token and rejects unavailable, expired or mismatched credentials without falling back to static keys.
