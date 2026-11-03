---
"@reddb-io/cli": patch
---

Read native collection pages from the merged disk and resident view. Preserve
physical ID order, row schemas and exact counts, skip earlier encoded payloads
without decoding them, and propagate cold-read or page-budget failures instead
of returning a successful empty collection.
