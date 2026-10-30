---
"@reddb-io/cli": patch
---

Reuse the checked native artifact directory within each catalog index reconciliation instead of rereading it for every index. Preserve fresh observations and decoding-error fallbacks between calls.
