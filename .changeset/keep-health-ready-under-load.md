---
"@reddb-io/cli": patch
---

Keep health, liveness, readiness, and startup probes responsive when normal
HTTP query capacity is saturated by giving the exact GET probe routes a small,
independently bounded admission reserve.
