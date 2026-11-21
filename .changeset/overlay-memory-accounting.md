---
"@reddb-io/cli": patch
---

Index overlay memory accounting no longer walks the overlay. Every statement refreshes memory accounting, and the HASH and BTREE overlays recounted every entry changed since the last checkpoint each time: with 138k changed rows a `SELECT 1` took 11.6 ms instead of 1.4 ms, and the cost kept growing until the next checkpoint. The total is now kept current as entries change.
