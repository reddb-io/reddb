---
"@reddb-io/cli": patch
---

Serve `SHOW INDEXES` from the operational index registry so introspection no longer scans unrelated collection payloads or starves query workers on large databases.
