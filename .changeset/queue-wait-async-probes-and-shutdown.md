---
"@reddb-io/cli": patch
---

**Live queue waits no longer stall on storage, and shutdown releases parked waits** (follow-up to the staging incident 2026-11-19): the RedWire live queue wait (`QueueWaitOpen`) ran its storage probes directly on a tokio worker. Those probes are `group_read`, whose lease reclaim publishes to the WAL under the vault publication lock, and the delayed-message scan. A probe held up by a lock could therefore pin the worker and keep the wait from timing out; in staging a `wait_ms=30000` wait only resolved at SIGTERM, about 88 s after it parked.

Each probe now runs on the blocking pool under the shared execution admission and is bounded by the wait deadline, so the wait times out on schedule even while a probe is stalled. The park itself still holds no permit and no OS thread.

Graceful shutdown (SIGTERM / SIGINT / admin shutdown) now cancels the queue-wait registry first, so parked `QUEUE READ … WAIT` and live waits resolve as `cancelled` immediately instead of hanging until the process exits.
