# KV / Vault Substitution Audit (Issue #109)

The reporter posed an attack vector against textual SQL substitution:

```sql
PUT my.special.key = '1=1 OR 123x';
SELECT * FROM users WHERE foo = bar OR $my.special.key = '123x';
```

If `$my.special.key` substitutes textually (i.e. the stored string is
spliced into SQL source and re-parsed), an attacker who can write that
key controls the WHERE predicate. This audit answers four questions:

1. What KV / vault / config substitution syntax exists today?
2. For each form, how is the substituted value transported — typed
   `Value` (safe) or interpolated SQL text (dangerous)?
3. How does Postgres handle the equivalent?
4. Does RedDB deviate from the Postgres baseline?

**Verdict:** the reporter's `$my.special.key` example is **hypothetical
syntax that does not exist** in RedDB. The substitution forms that do
exist (`$config.*`, `$red.config.*`, `$secret.*`, `$red.secret.*`) all
transport values as typed `Value` instances through a function-call AST
node. There is no SQL string round-trip on any path, so there is no
deviation from the Postgres parameterised-query baseline. No
guardrails were added; one regression test pins the reported scenario
and four assert the typed-value semantics of the existing surface.

---

## Substitution syntax inventory

### S1 — `$secret.<path>` and `$red.secret.<path>`

Read a vault secret by path. Parsed by
`crates/reddb-server/src/storage/query/parser/expr.rs` L281-308. The
lexer emits `Token::Dollar` (`lexer.rs` L259, L837-840), the
prefix-parser consumes it, calls `parse_dollar_ref_path` (L389-396) to
gather a dot-joined identifier, then desugars the form into a typed
function call:

```text
$secret.mycompany.stripe.key
  ⇒ Expr::FunctionCall { name: "__SECRET_REF",
                         args: [Expr::Literal { value: Value::Text("mycompany.stripe.key") }] }
```

### S2 — `$config.<path>` and `$red.config.<path>`

Read a config value by path. Same parser path
(`parser/expr.rs` L281-308); desugars to:

```text
$config.app.mode
  ⇒ Expr::FunctionCall { name: "CONFIG",
                         args: [Expr::Literal { value: Value::Text("app.mode") }] }
```

Any other `$<ident>` is rejected at parse time with
`"unknown $ reference \`$<path>\`; expected $secret.*, $red.secret.*,
$config.*, or $red.config.*"` (`parser/expr.rs` L292-298). The
reporter's `$my.special.key` lands on this branch and **fails to
parse**.

### S3 — `KV(collection, key [, default])`

Builtin function for reading from a user-defined KV collection. Lexed
as `Token::Kv` (`lexer.rs` L1153) and parsed via the standard function
call path. Arguments are `Expr` nodes — typed literals or column refs
only (see `expr_is_path_like` / `expr_is_source_free`,
`runtime/query_exec.rs` L208-215).

### S4 — `CONFIG(path [, default])`

Builtin equivalent of `$config.path` with an optional default. Same
runtime evaluator as S2; same path-typed argument constraint.

### S5 — Prepared-statement parameters (`Expr::Parameter`)

Wire-level prepared statements lift literals to `Expr::Parameter
{ index: usize }` via `parameterize_query_expr`
(`storage/query/planner/shape.rs`); execute substitutes typed `Value`s
back into `Expr::Literal` via `bind_parameterized_query`. Already
audited as **V1** in `docs/security/sql-injection-audit.md` and pinned
by `prepared_bound_string_is_treated_as_literal_not_sql` in
`tests/sql_injection_audit.rs`. Surface is parameter-only — no `?`/`$1`
text appears in SQL source today, only on the wire.

### S6 — `&<path>` secret reference (designed, not implemented)

Specified in `docs/security/config-secrets-vault-design.md` §8 as a
config-value form: `SET CONFIG x = &red.secret.tls.key`. **The lexer
has no `&` token today** (`lexer.rs` `next_token_internal` falls
through to `LexerError::Unexpected character: '&'`), so `&path` does
not parse. Out of scope until S6 lands. When it lands the design
mandates the same typed-value transport as S1/S2.

### Forms that do NOT exist

- `PUT key = value` (the reporter's example): **no SQL `PUT` keyword**.
  The `PUT` string only appears as an HTTP method label
  (`server/routing.rs`, `storage/backend/{http,s3}.rs`). The SQL
  surface for KV mutation is `SET CONFIG`, `SET SECRET`, `INSERT INTO
  <kv-collection>`, or HTTP `PUT /kv/{collection}/{key}` — none of
  which produce textual SQL substitution.
- `$<arbitrary>.path` for user-defined collections: rejected at parse
  time (see S2). User-defined KV reads must go through `KV(coll,
  key)`.
- `:name` parameters: `Token::Colon` exists but is never consumed in
  expression position; `:` is only used inside type-cast context and
  some grammar-specific separators.
- `@var` session variables: not implemented.

---

## Per-form safety classification

| Form | Transport | Re-parsed? | Verdict |
|:--|:--|:--|:--|
| S1 `$secret.path` | `Expr::FunctionCall("__SECRET_REF", [Value::Text(path)])` ⇒ `runtime::impl_core::current_secret_value(path) -> Option<String>` ⇒ `Value::text(s)` | No | Safe — typed |
| S2 `$config.path` | `Expr::FunctionCall("CONFIG", [Value::Text(path)])` ⇒ `current_config_value(path) -> Option<Value>` | No | Safe — typed |
| S3 `KV(c, k)` | `Expr::FunctionCall("KV", [Value::Text(c), Value::Text(k)])` ⇒ `lookup_latest_kv_value(db, c, k) -> Option<Value>` | No | Safe — typed |
| S4 `CONFIG(p)` | Same as S2 | No | Safe — typed |
| S5 `Expr::Parameter` | wire-typed `Value` ⇒ `Expr::Literal` | No | Safe — typed |
| S6 `&path` | not implemented | n/a | n/a |

**Common property:** every form lifts its result into a `Value` enum
variant (`Value::Text(Arc<str>)`, `Value::Integer`, `Value::Json`,
etc.) and feeds that into the runtime expression evaluator
(`runtime/expr_eval.rs::evaluate_runtime_expr_with_db`). The evaluator
operates on the typed `Value` via `apply_binop` and friends; the
stored string is never tokenised, never re-parsed, and never
concatenated with adjacent SQL. A stored secret of `"1=1 OR 123x"`
compares as the literal 12-byte string `1=1 OR 123x`, not as a
predicate.

The two helper paths confirm this:

- `current_secret_value` (`runtime/impl_core.rs` L341-357) returns
  `Option<String>` from an in-memory snapshot keyed by lowercase path.
- `current_config_value` (`runtime/impl_core.rs` L389-402) returns
  `Option<Value>` from an in-memory snapshot.
- Both are wrapped to `Value` at the function-call evaluation site
  (`runtime/expr_eval.rs` L133-144) and never touched by the parser
  again.

---

## Postgres baseline

Postgres exposes one hardened substitution surface and one
non-substitution surface relevant here:

1. **Parameterised queries** (`PREPARE … AS SELECT … WHERE x = $1;
   EXECUTE stmt('1=1 OR 1=1')`). The bound value rides over the
   protocol as a typed parameter, lands in the planner as a typed
   `Const`, and is compared against `x` by the operator catalog. The
   value never re-enters the SQL parser. This is the strict baseline
   any text-substitution surface should compare against.

2. **`current_setting('foo.bar')`** — a Postgres function that reads
   a typed `text` (or other-typed) setting from the GUC table. It
   returns a typed `Datum`, not raw SQL. Same safety property as
   parameterised queries.

3. **String concatenation in client code** (`format!("WHERE x = '{}'",
   user_input)`) — the classic SQL injection vector. Postgres does
   not protect against this; it is caller error. RedDB's substitution
   surface is more analogous to (1) and (2) above, not to client-side
   string templating.

---

## Findings

### F1 — Reporter's `PUT $my.special.key` example does not parse

`PUT` is not a SQL verb; `$my.special.key` falls through the `$secret.*
| $config.*` whitelist and produces a parse error. The exact attack
described does not reach the engine.

### F2 — All implemented `$`-substitution forms transport typed `Value`

Each `$secret.*` / `$config.*` reference is a structural AST node
(`Expr::FunctionCall { name: "__SECRET_REF" | "CONFIG", args: [...] }`)
whose evaluator returns a typed `Value`. The stored content is
opaque to the SQL parser. A secret with content `1=1 OR 1=1` stays a
12-byte text value; comparing it with `=` calls `apply_binop(Eq,
Value::Text("1=1 OR 1=1"), …)`, never re-parses it.

### F3 — `SET SECRET` / `SET CONFIG` value position is locked to typed literals

`parse_literal_value` (`parser/dml.rs` L460-547) accepts only
`Token::String | JsonLiteral | Integer | Float | True | False | Null |
LBracket | LBrace | PASSWORD(…) | SECRET(…)`. Expression syntax is not
permitted, so an attacker cannot write `SET SECRET k = $config.x ||
'foo'` to launder one substitution into another.

### F4 — No deviation from Postgres baseline

The `$secret.*` / `$config.*` substitution form behaves like Postgres
`current_setting('path')` plus prepared-parameter binding: the
substituted value is typed, the SQL parser does not see it. There is
no textual round-trip anywhere on the substitution path.

### F5 — Designed but unimplemented surfaces

- `&path` secret reference (vault design §8) — lexer has no `&`
  token; parses as an error today. Track via the vault-design
  follow-up.
- `red.config.ai.openai.…` autopopulation from secret references —
  same: not implemented, not a concern until S6 lands.

When S6 lands, the design mandates that the vault path resolves to a
typed `Value` at config-read time (same shape as S1). A
regression test against that shape should land alongside that work.

---

## Recommendations

No action required for issue #109. The reported attack vector relies
on textual substitution that this codebase does not perform. Pinned
six regression tests in
`crates/reddb-server/tests/kv_substitution_audit.rs` to assert the
typed-value contract structurally:

- `unknown_dollar_reference_fails_at_parse` — the reporter's exact
  `$my.special.key` literally fails to parse; covers F1.
- `dollar_secret_payload_lands_as_typed_function_call` — a stored
  secret with predicate-shaped content lands as a `__SECRET_REF`
  function call over a path literal; covers F2.
- `dollar_config_reference_is_typed_function_call` — same shape for
  `$config.*`; covers F2.
- `set_secret_value_position_rejects_expression` —
  `SET SECRET k = a || b` (or any non-literal) fails at parse time;
  covers F3.
- `kv_function_args_are_typed_path_literals` — `KV('coll', 'key')`
  lands as a function call over typed string literals, never as
  re-parsed SQL; covers F4.
- `put_command_is_not_sql_syntax` — `PUT my.special.key = 'x'` fails
  at parse time, confirming the reporter's attack surface does not
  exist; covers F1.

The tests are pure parser exercises — no runtime, no I/O — and stay
off the SELECT hot path.

---

## Out of scope (not pursued under #109)

- Authorisation around `SET SECRET` / `$secret.*`. The vault-design
  doc specifies `secret:set` / `secret:use` actions and a
  `secret:reveal` separation. Enforcement is a separate hardening
  ticket.
- Snapshot / log redaction of secret values **at the engine layer**.
  The `tests/support/parser_hardening/secret_redactor.rs` machinery
  redacts secrets in committed `*.snap` files, which is sufficient
  for the parser-test surface this issue exercises. Engine-layer
  redaction (slow query log, EXPLAIN output, error messages
  containing literal values) deserves its own audit and is tracked
  separately.
- The unimplemented `&path` reference syntax (vault design §8). When
  it lands, mirror this audit's tests.
