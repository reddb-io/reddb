---
"@reddb-io/cli": patch
---

Keep incremental checkpoint mutation proofs when growing segments are sealed
and new segments are created. Match prior segments by identity, validate new
segments from their first revision, and retain full validation when an old
source disappears or its mutation journal is incomplete. This avoids rebuilding
unchanged payload history after ordinary segment lifecycle transitions.

Preserve an existing row's sequence identity when an update moves it out of a
sealed segment. Pending preimages then continue to match their durable image
after MVCC normalization, without requiring an uncommitted WAL record or
rewriting the collection. The destination allocator advances past adopted
sequences; indexes, metadata and mutation tracking are retained.
