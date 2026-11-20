---
"@reddb-io/cli": patch
---

/health/ready now reports how many executions outlived their deadline and where the longest-running ones came from when it answers handler_deadline_exceeded.
