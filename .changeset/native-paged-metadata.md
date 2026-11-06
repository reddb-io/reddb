---
"@reddb-io/cli": patch
---

Persist complete physical metadata inside disk-resident paged databases, including catalogs larger than the native summaries. Preserve it through offline compaction and canonical catalog recovery, use the native DDL catalog instead of external collection markers, and report metadata decoding errors without hiding them behind a missing JSON fallback. Files using the new native metadata layout reject older writers; rollback requires the preserved pre-upgrade source.
