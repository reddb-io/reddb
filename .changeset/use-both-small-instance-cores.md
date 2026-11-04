---
"@reddb-io/cli": patch
---

Use both engine workers on two-core instances so one blocked query cannot
serialize every database request and exhaust the HTTP admission limit.
