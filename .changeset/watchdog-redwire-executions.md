---
"reddb": patch
---

**RedWire executions join the shared execution watchdog** (staging incident 2026-11-17): a wire execution that outlives `execution_queue_timeout` now marks the shared watchdog aggregate, degrading `/health/ready` to 503 exactly like an HTTP handler that overruns `handler_timeout`. Kubernetes readiness probes observe a wedged wire worker and the pod self-heals via restart, instead of answering ready forever while a single stuck execution holds its admission permit and saturates the execution queue (`execution_queue_timeout`, `active:1` fixed, queue full).

The watchdog moved from the HTTP server to the runtime (`reddb_server::runtime::execution_watchdog`): HTTP and RedWire now arm tickets against the same aggregate, and the runtime exposes it for any future transport. Ticket markers run on their own task at the deadline; the worker-owned completion guard records the eventual finish race-safely.
