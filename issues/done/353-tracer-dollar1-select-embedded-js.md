# Tracer: $1 placeholders for SELECT WHERE end-to-end via embedded stdio + JS driver [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/353

Labels: enhancement

GitHub issue number: #353

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

End-to-end tracer bullet for parameterized queries: a TypeScript caller can run `db.query('SELECT * FROM users WHERE id = $1 AND name = $2', [1, 'Alice'])` against an embedded RedDB instance and get back rows.

This is the foundational vertical slice. It introduces — at minimum viable scope — every layer the rest of the work depends on:

- Engine: placeholder parser (`$N` only, integer + text only), `Expr::Placeholder` AST node, `Value` enum (int, text, null), parameter binder with arity + type validation.
- Embedded stdio JSON-RPC: accept optional `params` field on the query method.
- JS SDK: new `query(sql, params?)` overload that serializes the array to the JSON-RPC params field.

Other Value variants (vector, bytes, etc.), other transports (RedWire/HTTP/gRPC), other drivers, and `?` syntax are out of scope for this slice — they have their own.

## Acceptance criteria

- [ ] `$1`-style placeholders parse correctly in SELECT statements.
- [ ] Placeholder parser ignores `$N` inside string literals and comments.
- [ ] Binder rejects: arity mismatch, type mismatch (text where int expected and vice versa), gaps in `$N` indices.
- [ ] `Value::Null`, `Value::Int`, `Value::Text` round-trip through embedded stdio JSON-RPC.
- [ ] JS SDK `db.query(sql, params)` works for SELECT WHERE with int/text/null params.
- [ ] Original `db.query(sql)` signature unchanged.
- [ ] Unit tests for placeholder parser (deep module).
- [ ] Unit tests for binder (deep module).
- [ ] Integration test in `drivers/js/test/` covering the end-to-end path.

## Blocked by

- #352

## Implementation notes (2026-05-12)

Tracer slice landed end-to-end:

- Parser: `$<integer>` after `Token::Dollar` → `Expr::Parameter { index: N-1 }`
  in `crates/reddb-server/src/storage/query/parser/expr.rs`. `$0` rejected.
  Strings/comments already isolated by lexer.
- Binder: new module `crates/reddb-server/src/storage/query/user_params.rs`.
  `collect_indices`, `validate` (arity + gap), `bind` re-using
  `shape::bind_user_param_query` (thin pub wrapper around existing
  `bind_query_expr_inner`). 8 unit tests.
- Transport: `rpc_stdio.rs` `"query"` reads optional `params` JSON array →
  `parse_multi` → `user_params::bind` → `execute_query_expr`. Absent
  `params` keeps legacy path byte-identical. 3 stdio round-trip tests.
- JS SDK: `db.query(sql, params?)` overload + `.d.ts` second signature.
  Smoke test covers int/text/null binding, legacy form, arity reject.

`cargo check` clean; new suites green (`placeholder`, `user_params`,
`query_with_`).

Type validation is delegated to the engine type checker on substituted
literals — explicit typed binder contexts are #361. Vector and other
Value variants are #355 / #356. Other transports and `?` syntax are
#357 / #358 / #359 / #354.

Final `git add -A && git commit` blocked by local Bash permission hook
in this Ralph iteration — changes are present in the working tree ready
for the next iteration to commit.
