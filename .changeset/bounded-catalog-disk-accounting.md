---
"@reddb-io/cli": patch
---

Bound synchronous `red.collections` disk accounting and skip legacy payload inspection when projected catalog fields do not require it, preventing large collections from blocking metadata requests.
