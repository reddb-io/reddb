---
"@reddb-io/cli": patch
---

Keep physical metadata, health and authority inspection from publishing native
artifacts or healing metadata files. Preserve explicit maintenance and recovery
as the paths that publish repairs.

Reject paged RDDB stores in embedded SCRUB and salvage before scanning payloads.
Recognize either paged superblock so a damaged primary cannot cause embedded
recovery to misinterpret stored values or load a large paged file for carving.
