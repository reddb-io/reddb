---
"@reddb-io/cli": patch
---

**Parked queue waits no longer trip the execution watchdog** (staging incident 2026-11-19, engine v1.39.6): since #2887 every RedWire execution armed a watchdog ticket that degraded `/health/ready` to 503 (`handler_deadline_exceeded`) once it outlived `execution_queue_timeout`. A consumer long-polling with `QUEUE READ … WAIT 30000ms` on an empty queue parks for its full WAIT by design, so it reached the 30 s deadline every cycle and Kubernetes restarted the pod repeatedly.

A watchdog ticket is now suspended while its worker is intentionally parked in a queue wait, and the deadline restarts from zero when work resumes. Only the park is exempt: the queue probes still count, and an execution genuinely stuck in engine work still degrades readiness after the deadline and recovers only when it finishes. RedWire's completion guard is now owned by the blocking worker (it records the end of engine work, not of the session future), and the per-execution deadline marker is cancelled as soon as the execution settles instead of sleeping out the full deadline.
