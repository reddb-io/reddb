---
"@reddb-io/cli": patch
---

Fix an unauthenticated remote crash: a SQL statement containing a multi-byte character (for example `SELECT 'héllo'`) made the server panic and, because release builds abort on panic, exit. The keyword matchers that sliced the statement at a fixed byte offset now return "no match" instead. The SPARQL parser had the same class of bug (its byte cursor advanced by 1 after reading a multi-byte character) and could additionally loop forever, growing memory without bound, on any token it did not recognise (`;`, `!`, an emoji); both are fixed.
