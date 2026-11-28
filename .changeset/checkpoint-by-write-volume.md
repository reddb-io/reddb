---
"@reddb-io/cli": patch
---

A checkpoint is also due by write volume. A load into a large table used to pass the 8,192 changed rows a checkpoint writes as a delta long before the schedule or memory pressure asked for one, and the checkpoint that came then copied the whole table with every writer waiting: a 400,000-row load held writers for up to 39 s and one statement failed. A large collection is now checkpointed while its delta still fits (4,096 changed rows, or 512 when the WAL references a delta needs are nearly full); the same load holds writers for at most 3.4 s and never copies the table. Checkpoints taken this way less than 30 s apart log a warning, the way PostgreSQL's checkpoint_warning does.

A table that crashes before its first checkpoint is read by key without walking its rows. After a kill with 358,000 rows only in the WAL, a 1,000-row INSERT took five minutes; it takes 0.1 s.
