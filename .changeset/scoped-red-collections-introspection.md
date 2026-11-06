---
"@reddb-io/cli": patch
---

Push collection-name filters and projected fields into `red.collections` catalog snapshots so targeted introspection no longer decodes unrelated collection payloads or computes unrequested on-disk sizes.
