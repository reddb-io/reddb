---
"@reddb-io/cli": patch
---

Prevent stale or reclaimed secondary-index postings from hiding live MVCC rows after restart, and publish the runtime index registry under its canonical `red.index.registry` name.
