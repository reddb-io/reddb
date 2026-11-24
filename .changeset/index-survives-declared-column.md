---
"@reddb-io/cli": patch
---

Declaring a column no longer leaves a table's indexes unread after the next restart. The identity of a HASH or BTREE index includes the collection's declared schema, so `ALTER TABLE … ADD COLUMN` (or declaring a first column on a collection that had none) moved it; the running process kept answering, and after a restart every lookup through the index walked the whole table until a checkpoint republished it. The tree published under the schema as it was before the columns were appended holds the same key for every row and is now read until the next checkpoint publishes the index under its current identity.
