---
"@reddb-io/cli": patch
---

Keep checkpoints incremental after repeated updates and garbage collection of a bounded set of row versions. Retain the latest mutation revision per physical ID, account for bounded tracking memory, and preserve full validation when an evicted ID makes an older checkpoint proof incomplete.
