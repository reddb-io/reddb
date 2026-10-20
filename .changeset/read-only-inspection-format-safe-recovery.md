---
"@reddb-io/cli": patch
---

Keep physical metadata, health and authority inspection from publishing native
artifacts or healing metadata files. Preserve explicit maintenance and recovery
as the paths that publish repairs.

Drain periodic background workers when the last foreground runtime or connection
closes, so an active weak upgrade cannot retain the database lock after close.
Unpark idle workers rather than waiting for their next timer tick.

Reject paged RDDB stores in embedded SCRUB and salvage before scanning payloads.
Recognize either paged superblock so a damaged primary cannot cause embedded
recovery to misinterpret stored values or load a large paged file for carving.
