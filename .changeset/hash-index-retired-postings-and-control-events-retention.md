---
"reddb-io": patch
---

Stop scanning the whole collection to find a row that changed since the last
checkpoint: the paged hash index no longer treats the posting of a row it saw
removed as proof of an incomplete index, and resolving a logical row id under a
snapshot returns the live version directly when the reader can see it. On a
50k-row table an `UPDATE ... WHERE id = ?` of an already-updated row drops from
250-490 ms to 4 ms.

`red.control_events` now has native retention: `control_events.retention` /
`REDDB_CONTROL_EVENTS_RETENTION` (default `7d`, `off` disables).
