---
"@reddb-io/cli": patch
---

Keep incremental checkpoint mutation proofs when growing segments are sealed
and new segments are created. Match prior segments by identity, validate new
segments from their first revision, and retain full validation when an old
source disappears or its mutation journal is incomplete. This avoids rebuilding
unchanged payload history after ordinary segment lifecycle transitions.
