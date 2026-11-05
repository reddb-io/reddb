---
"@reddb-io/cli": patch
---

Stop advertising readiness while a handler that exceeded its deadline is still occupying the blocking executor, and recover readiness automatically when that work unwinds.
