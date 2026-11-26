---
"@reddb-io/cli": patch
---

Checkpoints no longer fill rebuilt indexes one posting at a time: the key index and secondary indexes are loaded from sorted runs, and page reservations and captured page images are made durable once before the first page reaches the file instead of once per page. A checkpoint that took 97 s on a 120k-row indexed table takes 10 s, and an incremental checkpoint about half as long.
