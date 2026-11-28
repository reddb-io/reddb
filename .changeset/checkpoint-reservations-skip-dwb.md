---
"@reddb-io/cli": patch
---

A checkpoint writes the pages of the generation it is building straight to their place instead of through the double-write buffer: nothing reads them before the manifest names them, and recovery releases them whatever they hold. A checkpoint that changed 20 of 173 small tables went from about 157 fsync calls to about 115. The catalog root, pages written after publication and pages that existed before keep the double-write path.
