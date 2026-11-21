---
"@reddb-io/cli": patch
---

`SHOW CREATE TABLE` and `DESCRIBE` read only the table they name. Both built the whole catalog first, which on a disk-resident store decodes every row of every collection while other statements wait: on a 6 GiB store either statement took about 50 seconds, whatever the size of the table asked for.
