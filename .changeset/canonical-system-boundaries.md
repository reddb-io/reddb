---
"@reddb-io/cli": patch
---

Canonicalize legacy internal collection aliases before IAM policy matching and system-schema DDL protection, preventing `red_*` aliases from bypassing `red.*` prefix rules.
