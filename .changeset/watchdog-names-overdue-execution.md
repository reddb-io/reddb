---
"@reddb-io/cli": patch
---

The execution watchdog now says what it is waiting for. When an execution
outlives its deadline and is still running, the server logs a warning with the
statement (redacted like the slow-query log) and how long it has been running,
repeats it every minute while it stays stuck, and logs when it finishes. A
server that degraded readiness with `handler_deadline_exceeded` while its
process sat idle gave no way to tell which statement was blocked (#2908).
