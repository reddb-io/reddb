---
"@reddb-io/cli": patch
---

Retain active statement snapshot horizons through query completion so concurrent
VACUUM cannot reclaim versions still needed by ordinary or nested reads.
