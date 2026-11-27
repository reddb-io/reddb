---
"@reddb-io/cli": patch
---

Events removed by the control-event ledger's retention are now reclaimed from disk on a store that has VCS commits. Every applied migration creates a commit, and a commit pinned the history of the ledger like that of every other system collection: retention marked the events removed and none was ever freed, so the ledger of such a store only grew. The retention pass also stops handing already-removed events to its statement again on every tick, and resumes where its last pass ended.
