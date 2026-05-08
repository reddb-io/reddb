# Parser coverage uplift D: auth_ddl.rs + migration.rs + probabilistic_commands.rs to 90% [AFK]

## Parent

#227

## What to build

Raise line coverage for the current worst parser files to the 90% floor:

- `parser/auth_ddl.rs`: 0.0% -> >=90%
- `parser/migration.rs`: 0.0% -> >=90%
- `parser/probabilistic_commands.rs`: 16.2% -> >=90%

Baseline reproduced locally with:

```bash
cargo llvm-cov --lib -p reddb-server --lcov --output-path /tmp/reddb-parser-filtered-lcov.info -- storage::query::parser
```

Then extracted parser/lexer file coverage from LCOV.

These files are not covered by #234, #235, or #236, but they are part of the red/double-red parser surface for the 90% goal.

## Acceptance criteria

- [ ] `parser/auth_ddl.rs` reports >=90% line coverage.
- [ ] `parser/migration.rs` reports >=90% line coverage.
- [ ] `parser/probabilistic_commands.rs` reports >=90% line coverage.
- [ ] Auth DDL tests cover documented GRANT/REVOKE/role/user/policy-related forms handled by the parser, including rejection of quoted/string identifiers that could hide SQL metacharacters.
- [ ] Migration tests cover CREATE/APPLY/ROLLBACK/EXPLAIN forms, dependencies, body parsing, malformed bodies, reserved-name errors, and DoS limits already snapshotted in integration tests.
- [ ] Probabilistic tests cover CREATE/DROP HLL/sketch/filter variants plus add/check/delete paths and invalid numeric options.
- [ ] Tests are behavior-focused via public parser entry points; no private helper-only testing unless a branch is otherwise unreachable.
- [ ] `cargo llvm-cov --lib -p reddb-server --lcov --output-path /tmp/reddb-parser-filtered-lcov.info -- storage::query::parser` completes successfully and shows the three target files >=90%.
- [ ] `cargo test -p reddb-server` passes, or unrelated pre-existing failures are explicitly documented.

## Out of scope

- Queue/timeseries uplift (#234).
- DML/DDL uplift (#235).
- Lexer/table uplift (#236).
- New fuzz targets (#233).

## Blocked by

None - can start immediately.
