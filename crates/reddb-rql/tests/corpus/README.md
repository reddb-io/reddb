# RQL standard-SQL conformance corpus

Each `*.slt` file in this directory is a [sqllogictest][slt]-format script. The
harness (`tests/conformance.rs`) discovers every `.slt` file automatically and
runs it end-to-end against the current in-server engine
(`reddb-server`'s `RedDBRuntime::in_memory()`), one fresh runtime per file so
state never leaks between scripts.

## Truth is the SQLite oracle

The standard-SQL slice is sourced from the **public SQLite sqllogictest
corpus**. Every expected result block is the value **SQLite** produces — never
whatever the current engine happens to emit (ADR 0053). Engine output is the
thing under test, not the source of truth.

## Recording dialect divergences (skip, never drop)

When a standard-SQL case is genuinely correct against the SQLite oracle but the
RedDB engine diverges (a dialect difference, an unimplemented surface, or a
rendering seam), the case is **kept and skipped with a reason**, never silently
dropped. Use sqllogictest's conditional directive keyed on the engine name
(`reddb-server`) and write the reason in a comment directly above it:

```
# RedDB renders an integer SUM as a REAL; SQLite renders a bare integer.
# Divergence tracked under PRD #1098. Keep the oracle value; skip the run.
skipif reddb-server
query I nosort
SELECT SUM(v) FROM nums
----
80
```

`skipif reddb-server` skips that one record for our engine while leaving the
oracle-correct expectation on the page as documentation. `onlyif reddb-server`
is the inverse (run only on our engine) and should be avoided here — it would
let engine output masquerade as truth.

## Cell rendering

The harness renders each engine `Value` into a comparison cell with the rules
SQLite's reference harness uses (see `src/conformance.rs`): `NULL` → the literal
`NULL`; text passes printable ASCII through and scrubs the rest to `@`; integers
print in decimal; reals print with exactly three decimals. The `query <types>`
header documents the intended column shape; the engine's intrinsic value kind
drives the actual rendering.

[slt]: https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki
