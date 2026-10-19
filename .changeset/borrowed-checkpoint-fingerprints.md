---
"@reddb-io/cli": patch
---

Avoid cloning complete collections while computing checkpoint metadata fingerprints. Visit borrowed rows in the existing stable ID order, retaining identical fingerprint values with compact row references instead of duplicate payloads. Keep unchanged collection checkpoints reusable when rollback or vault visibility changes affect only other collections, using a shared frozen visibility snapshot and borrowed row-header checks.
