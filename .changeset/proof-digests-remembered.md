---
"@reddb-io/cli": patch
---

A tree with page proofs remembers the digests it has already proven and writes the proof entries of a generation once, when it is sealed, instead of walking and rewriting four proof pages on every page access. A checkpoint of a 120k-row indexed table goes from 10 s to under 5 s and an incremental checkpoint of 100 updated rows from 0.9 s to 0.4 s.
