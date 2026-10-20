---
"@reddb-io/cli": patch
---

Honor the stderr destination for slow-query events without creating fallback
files. If a configured slow-query file or its directory cannot be opened,
report the error and use stderr instead of panicking during database startup.
Keep sampling, the bounded nonblocking writer and the telemetry ring store.
