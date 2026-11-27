---
"@reddb-io/cli": patch
---

`SHOW CONFIG` lists values that are already on disk. Under bounded disk residency it only listed the rows still in memory, so a value set before the last restart was missing from the listing although the engine still applied it. A retention sweep that removes none of the rows it found expired, or whose statement fails, now says so in the log (at most once every ten minutes per collection); it used to fail silently.
