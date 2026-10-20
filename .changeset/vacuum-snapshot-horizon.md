---
"@reddb-io/cli": patch
---

Preserve row versions required by snapshot transactions that excluded an older
writer. VACUUM now retains that snapshot's horizon after the writer commits and
releases it when the reader commits or rolls back.
