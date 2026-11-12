---
"@reddb-io/cli": minor
---

Add the native `TEMPERATURE` type across SQL/RQL, storage codecs, comparison, JSON, and wire compatibility. Introduce required-capability bits in paged `.rdb` superblocks so readers reject unsupported codecs before scanning data, while preserving resident and disk-resident formats through restart, result caching, and offline compaction.
