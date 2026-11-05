---
"@reddb-io/cli": patch
---

Stop deadline-expired UPDATE and DELETE workers before durable publication so timed-out requests release CPU and memory without leaving partial mutations.
