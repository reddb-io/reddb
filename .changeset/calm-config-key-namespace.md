---
"@reddb-io/cli": patch
---

Normalize keys stored inside `red.config` by removing redundant `red.` prefixes, migrate existing databases and logical dumps without losing winning history, and reject new prefixed writes across SQL, HTTP, storage, and bulk/versioned paths.
