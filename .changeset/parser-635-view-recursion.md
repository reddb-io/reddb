---
"@reddb-io/cli": patch
---

**Fix: parser stack overflow + view filter desync (#635).** Parsing a recursive
view body could overflow the stack (the `parse_sql_command` match frame was
oversized — extracted the CREATE arm to shrink it). Separately, the view
rewriter merged the inner query's `filter`, but the executor prefers `where_expr`
and nulled `filter` when present, so the merged predicate was silently dropped
(`view_chain_resolves_recursively`); the rewriter now keeps `where_expr` in sync
with the merged filter. Re-enables the previously quarantined view/materialized-view
parser binaries (#593/#594/#595/#596/views).
