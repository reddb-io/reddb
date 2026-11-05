# Paced reclamation cost — 2026-10-30

Signed candidate source: `49bf6ce9fece320a02d369779ee11ca3a2d2d899`.
Binary SHA-256: `d8df3b29870b9f6b1c6e66497ffe5e2f80a9e413ab91cf2a29facff92af27124`.
The source tree is identical to merged `4f651394c`.

`results.json` retains all twelve cases, including unfavorable samples. Each
series runs OFF/ON/ON/OFF on fresh synthetic stores. One CPU maximum, zero swap,
four clients, 256 rows with 24-KiB payloads, continuous acknowledged updates and
verified reads. The supplied artifact runs in a real cgroup; clients are outside
it. All 128,000 measured requests and twelve checkpoint/SIGKILL/recovery checks
passed integrity and OOM guards.

| Offered requests/s | Memory limit | Achieved OFF / ON | Write p99 OFF / ON | Scheduled pair p99 OFF / ON |
| --- | --- | --- | --- | --- |
| 160 | 512 MiB | 160.11 / 160.12 | 12.36 / 10.09 ms | 14.00 / 11.53 ms |
| 400 | 512 MiB | 389.11 / 400.18 | 31.81 / 13.38 ms | 533.12 / 54.46 ms |
| 800 | 1,024 MiB | 566.43 / 644.85 | 41.26 / 40.99 ms | 8,244.40 / 4,736.85 ms |

Table values are medians of the two process runs in each mode, not pooled
request percentiles. Scheduled pair latency includes delay before dispatch.
Both modes overload at 800 offered requests/s: this is **not** a passing
capacity qualification. The 400/s controls vary considerably: OFF achieved
400.08 then 378.14 requests/s. These shared-host samples have no independent
confidence interval and cannot establish an SLA or a universal speedup.

At 160 requests/s, maximum cgroup memory across load, checkpoint and recovery
was 483.81 MiB OFF and 270.20 MiB ON. ON used 7.13–7.48 CPU seconds during each
50-second interval, versus 8.83–9.28 OFF. No CPU throttling occurred at that rate.
The host does not delegate the I/O controller to user services; I/O counters
are null, not zero. Main-file size after checkpoint was 141,115,392 bytes in
both OFF cases versus 17,154,048–17,678,336 bytes ON. This demonstrates history
reclamation in this fixture, not shrinking an existing large file.

Reproduce each series with `scripts/qualify-maintenance-cost.py`; the JSON
contains its numeric arguments. The checkpoint and recovery stages are outside
the timed load interval. No explicit VACUUM or GC drain is used. Credentials,
payloads, certificates, raw logs and absolute local paths are excluded here.
These measurements do not qualify OCI I/O, all page states, arbitrary orphan
repair, full-file rewriting or the historical STG databases.
