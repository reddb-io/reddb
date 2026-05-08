# reddb-server Agent Notes

## Parser Coverage CI

`.github/workflows/parser-coverage.yml` posts a per-file coverage table
on every PR touching `src/storage/query/**` or `Cargo.lock`.

The report is **warn-only** — it never blocks merge. Thresholds are visual
markers (✅ ≥90%, ❌ <90%) for tracking progress toward full parser coverage.

**Future PRD:** flip to hard-fail once all parser/* and lexer.rs files reach
the 90% threshold. Until then the workflow is advisory.

### Running coverage locally

```sh
# Full lib coverage for the parser/lexer scope
cargo llvm-cov --lib -p reddb-server -- storage::query

# LCOV output (same as CI)
cargo llvm-cov --lib -p reddb-server --lcov --output-path lcov.info
```

### Delta baseline

On push to main the workflow caches a coverage snapshot under the key
`parser-coverage-main-<sha>`. PRs restore the nearest ancestor snapshot
and display the coverage delta per file. If no baseline exists the delta
column shows `n/a`.
