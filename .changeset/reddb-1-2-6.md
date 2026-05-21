---
"@reddb-io/cli": patch
---

Ship the work merged since v1.2.5.

**HTTP transport hardening (#569).** Bounded handler concurrency via a deep
`HttpConnectionLimiter` (hard cap → `503 Service Unavailable` + `Retry-After`
on saturation, rejection before parse/routing), an overall per-handler
wall-clock timeout that reclaims the limiter slot on expiry, the same shared
cap enforced on the TLS accept loop (HTTP + HTTPS draw one cap), and Prometheus
telemetry (`http_active_handler_threads`, `http_handler_rejected_total`,
`http_handler_duration_seconds`, `http_handler_cap`). New config: `--http-max-handlers`,
`--http-handler-timeout-ms`, `--http-retry-after-secs`.

**QueueLifecycle foundations (#527 prereqs).** `QueueTxn` now participates in the
caller's transaction via the runtime MVCC path so lifecycle ack/purge/delete are
rollback-safe; the primary `QueueStore` adapter reads the legacy `queue_pending`
state (closing the parallel meta-row divergence); and lifecycle gains
`group_read` + `claim` methods that preserve the legacy `consumer`/`delivery_count`
result shape. (The atomic cutover and the `delivery_id` wire handle remain
follow-ups — the queue ACK/NACK contract is unchanged in this release.)

**Analytics-event primitives (#575)** and **SDK Helper Spec v1.0 + cross-driver
conformance (#449)** as previously merged on main.

Also documents that table column names persist across a file-backed reopen and
that aggregate result columns use a single canonical `FUNC(arg)` casing —
both verified by regression guards; these were 1.1.x-era reports already
resolved on the 1.2 line.
