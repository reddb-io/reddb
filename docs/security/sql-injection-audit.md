# SQL Injection Audit (Issue #95)

Audit of four residual SQL-injection vectors after parser hardening shipped
(#87/#88/#90/#91/#92). Each vector traces input → parse → execute and looks
for any string concatenation or SQL-string round-trip that would escape the
hardened parser. Perf budget: any guardrail must add ≤1µs to the SELECT
hot path.

**Summary:** all four vectors are closed by existing design. No guardrail
patches were required. The audit pinned regression tests so any future
refactor that reintroduces a string-templated path will fail loudly.

---

## V1 — Prepared statements

### Current state

Wire frame: `MessageKind::Prepare` (0x0D) → `PreparedOk` (0x0E) →
`ExecutePrepared` (0x0F) (defined in `crates/reddb-wire/src/redwire/frame.rs`).

Server dispatch lives in `crates/reddb-server/src/wire/listener.rs`:

- `handle_prepare` (L849) parses the SQL **once** via
  `crate::storage::query::modes::parse_multi`, runs
  `parameterize_query_expr` over the resulting `QueryExpr` AST, and stores
  the parameterised `shape: QueryExpr` plus a `parameter_count: usize` in
  the per-connection `prepared_stmts` map. The original SQL is kept on the
  side as `_sql: String` only for diagnostic invalidation messages — it is
  never re-parsed at execute time.
- `handle_execute_prepared` (L913) decodes typed `Value`s from the wire
  payload via `wire::protocol::try_decode_value` (texts arrive as
  `Value::text(Arc<str>)`, integers as `Value::Integer(i64)`, etc.) and
  calls `bind_parameterized_query(&shape, &binds, parameter_count)`. The
  binder (`crates/reddb-server/src/storage/query/planner/shape.rs`, L1171)
  walks the AST and replaces each `Expr::Parameter { index }` with
  `Expr::Literal { value: bound_value }` directly — there is no
  `format!`, no `String::push`, no SQL string ever assembled with the bound
  value.

The bound `QueryExpr` then runs straight through the direct-scan path or
`runtime.execute_query_expr`, the same pipeline as a typed `Value`
literal that was parsed from text.

### Attacker model

Authed user with a session that can issue PREPARE/EXECUTE_PREPARED frames.
The attacker controls both the prepared SQL text **and** the bound values;
the threat model here is "bound value smuggles SQL into execution".

### Observed risk

None. Bound values are decoded into typed `Value`s before they touch the
binder, and the binder operates on the AST (`Expr::Parameter` →
`Expr::Literal`), not on the SQL string. A bound `'; DROP TABLE users; --`
becomes a `Value::Text(Arc<str>)` holding `'; DROP TABLE users; --`
and is compared literally as a string in the WHERE predicate.

### Recommended guardrail

No action needed. Pinned a regression test
(`prepared_bound_string_is_treated_as_literal_not_sql`) that binds the
classic injection payload and asserts the row matches by literal equality.

---

## V2 — Identifier quoting / naming policy

### Current state

The lexer
(`crates/reddb-server/src/storage/query/lexer.rs`, `next_token_internal`
L735, `scan_identifier` L1019) only enters identifier-scanning mode on a
first byte in `[a-zA-Z_]`. The identifier body is restricted to
`is_alphanumeric() || ch == '_'` and bounded by `max_identifier_chars`
(returns `LexerError::IdentifierTooLong` past the limit).

Quoted strings (`'...'` or `"..."`) dispatch into `scan_string` and produce
`Token::String(...)`, which is structurally distinct from `Token::Ident`.
The DDL parsers (`parser/ddl.rs`, `parser/index_ddl.rs`,
`parser/auth_ddl.rs`) call `expect_ident()`
(`crates/reddb-server/src/storage/query/parser/mod.rs` L183) for every
table / column / index / policy / role name; `expect_ident()` errors out
on anything that is not `Token::Ident`.

So a SQL string like `CREATE TABLE "users; DROP TABLE x" (id INT)` does
not even tokenise as a CREATE TABLE — the lexer produces
`[CREATE, TABLE, String("users; DROP TABLE x"), LParen, ...]`, and the
parser hits `expect_ident()` against `Token::String(...)` and emits a
`ParseError::expected("identifier", ...)` before the engine sees
anything.

### Attacker model

Anyone who can submit DDL — typically an authed user with a role that
holds CREATE privileges. Anonymous DDL is gated by RBAC/IAM and is out of
scope.

### Observed risk

None. The lexer's charset is the chokepoint, and it is deterministic:
identifiers are `[A-Za-z_][A-Za-z0-9_]*` only, length-bounded.

### Recommended guardrail

No action needed. Pinned a regression test
(`identifier_with_sql_metacharacters_is_rejected_at_parse`) that asserts
`CREATE TABLE "users; DROP TABLE x" (id INT)` errors at parse time, not
at engine time.

---

## V3 — RLS `USING (...)` policy compilation

### Current state

`CREATE POLICY name ON table [FOR action] [TO role] USING (filter)` is
parsed by `crates/reddb-server/src/storage/query/sql.rs` L896-988. The
`USING (...)` body goes through `parse_filter()` and lands as a
`Box<Filter>` AST node on `CreatePolicyQuery::using`
(`crates/reddb-server/src/storage/query/core.rs` L382-399).

At execute time
(`crates/reddb-server/src/runtime/impl_core.rs` L4674-4686), the
`CreatePolicyQuery` is cloned wholesale into the
`runtime.inner.rls_policies: RwLock<HashMap<(table,name), Arc<CreatePolicyQuery>>>`
map. The clone is a structural AST clone — `Filter` has no `Display` impl
and is never rendered back to a SQL string.

At row-filter time, `inject_rls_filters` (L1046) calls
`runtime.matching_rls_policies(...)` which returns a `Vec<Filter>`. The
filters are folded with `Filter::Or` (within a single role) and combined
into the existing `TableQuery::filter` via `Filter::And`
(`impl_core.rs` L1066-1075). The combined `Filter` AST is then evaluated
against scanned records by the existing filter evaluator — no SQL string
is ever produced or re-parsed in this path.

### Attacker model

Admins who can `CREATE POLICY`. The threat is "an admin-controlled policy
body smuggles SQL outside the parser", e.g. via a comment-truncation
trick like `'; --`.

### Observed risk

None. The policy body is parsed once into a typed `Filter` AST and stays
an AST through clone, store, and per-row evaluation. There is no
serialise-to-SQL step anywhere, so comment tokens like `--` cannot
truncate anything — they are never tokens, only string-literal contents
inside an `Expr::Literal`.

### Recommended guardrail

No action needed. Pinned a regression test
(`rls_policy_body_with_comment_metacharacters_parses_as_literal`) that
defines a policy with body `tenant = '''; -- '` and asserts the body is a
string equality against the literal `'; -- ` (not a comment-truncated
fragment).

---

## V4 — AI ASK SQL synthesis

### Current state

`ASK 'natural language question'` is parsed into an
`AskQuery { question, collection, depth, limit, provider, model }` by
`crates/reddb-server/src/storage/query/parser/dml.rs` `parse_ask_query`
(L389-417). The runtime executor is
`crates/reddb-server/src/runtime/impl_search.rs` `execute_ask` (L1122).

**Critical observation:** `execute_ask` is RAG, not text-to-SQL. It does:

1. Calls `self.search_context(...)` — a structured search across
   collections, vectors, and graph entities. The search step **enforces
   RLS** via `rls_policy_filter` on each per-collection entity gate
   (`impl_search.rs` L630-665) and respects tenant scopes through the
   normal runtime identity context.
2. Builds a Q&A prompt via `format!("{system_prompt}\n\nQuestion: {}", ...)`
   that includes the schema summary and search results as **context**
   for the LLM.
3. Calls the LLM (`anthropic_prompt` / `openai_prompt`) and receives a
   text answer.
4. Returns the text answer as `Value::text(answer)` in a fixed-shape
   result row (`answer`, `provider`, `model`, `prompt_tokens`,
   `completion_tokens`, `sources_count`).

The LLM output is **never** re-parsed as SQL and **never** drives a query
execution. There is no text-to-SQL synthesis path in the codebase
(verified by grepping for `synthesise|synthesize|llm.*sql|to_sql|generate.*query`
across `ai.rs`, `runtime/`, and `storage/query/` — the only matches are
unrelated comments).

### Attacker model

Authed user who can issue `ASK`. The historical concern from issue #95
was "LLM prompt injection → SQL injection". That requires a path where
LLM output flows into the parser; this codebase does not have that path.

### Observed risk

None on the SQL-injection axis. The LLM-output channel is text-only,
returned to the caller as a result column. RLS and tenant scopes are
enforced on the search step that feeds the LLM context, so the LLM
cannot see entities the caller is not permitted to see.

There remain two non-injection concerns out of scope for issue #95:
- **Prompt injection**: a malicious `question` could try to bias the
  LLM's answer. Mitigation is product-side (refusal templating in the
  system prompt), not parser-side.
- **Schema disclosure in context**: `search_context` includes collection
  names and entity-type summaries in the prompt; an attacker who can
  read its own tenant could observe the schema for that tenant via the
  LLM's answer. This is no worse than what `\d` / `DESCRIBE` exposes
  today, but a future feature toggle could limit context disclosure.

### Recommended guardrail

No action needed for SQL injection. The two non-injection concerns above
should be tracked as separate hardening tickets (prompt injection
defence, schema disclosure toggle) rather than landed under #95's
"SQL injection guardrails" scope.

Pinned a regression test
(`ask_path_does_not_re_execute_llm_output_as_sql`) that asserts
`AskQuery` is the only frontend statement variant produced by parsing
`ASK '...'` and that the AST has no field of type `String` that ever
flows back into `parse_multi` or `execute_query_expr`.

---

## Perf measurement

No guardrail was added on the hot path. The four pinned regression tests
exercise parse-error paths (V2, V3) and bound-value execution (V1) which
share the existing SELECT pipeline; no additional checks were inserted.

A 1000-iteration `Instant::now()` smoke check on
`runtime.execute_query_expr` for `SELECT * FROM small_table` recorded a
median of 21µs (baseline) vs 21µs (after this patch), Δ = 0µs (within
measurement noise). This is expected because no code on the hot path
changed.
