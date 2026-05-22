---
"@reddb-io/cli": patch
---

1.3.1 patch release. Re-publishes the 1.3.x line across all registries (npm,
crates.io, GHCR, GitHub Release) after the 1.3.0 npm publish was blocked by a CI
token issue. No functional change since 1.3.0 — the parser fix (#635) and the
`GRAPH COMMUNITY ... RETURN ASSIGNMENTS` feature (#660) shipped in 1.3.0 are
included here as well.
