---
"@reddb-io/cli": patch
---

Stop an embedded store from growing on every open: config writes now replace a key's earlier value instead of appending a row, and reading a config key returns its latest value.
