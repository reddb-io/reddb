---
"@reddb-io/cli": patch
---

Avoid cloning complete collections while computing checkpoint metadata fingerprints. Visit borrowed rows in the existing stable ID order, retaining identical fingerprint values with compact row references instead of duplicate payloads.
