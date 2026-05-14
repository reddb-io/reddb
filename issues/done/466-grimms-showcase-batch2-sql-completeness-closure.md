# PRD: Close batch-2 SQL completeness + introspection gaps from the Grimms showcase

Labels: prd, needs-triage

## Current state

Closed locally. The batch-2 work was decomposed into local slices
`issues/done/467-*` through `issues/done/479-*`, covering the SQL mode
detector, parameter binder, SELECT-led JOINs, subqueries, concat coercion,
current-time builtins, DESCRIBE, SHOW CREATE TABLE, SHOW INDEXES, expression
column names, RETURNING, JS transactions, and SQL reference docs.

No additional AFK slice is currently open for this PRD.

## Problem Statement

The same multi-model demo author who produced the feedback that drives PRD #445 has filed a longer second pass against `@reddb-io/sdk@1.0.8` in embedded mode. The new batch overlaps the first on the multi-model breakage already tracked under #446–#465, but it also surfaces a distinct class of problems that PRD #445 does not cover: **SQL completeness and introspection gaps that make the engine feel half-finished even for users who never touch the multi-model side**.

A code-cite triage plus live repro against the current `main` HEAD confirms that the new items split into three clean groups:

- **Bugs caused by tiny dispatch / heuristic mistakes.** A single line in the mode detector routes any `SELECT … ?` into the SPARQL parser, breaking the SDK's documented parameterised query path. A `||` between two string literals now works correctly, but `name || '!'` (column + literal) and the function form `CONCAT('a', 'b')` still re-quote at SQL print time, producing `'updated''!'` and `'a''b'`. `SELECT CURRENT_TIMESTAMP` does not resolve to `NOW()` and is processed as a bare column reference that dumps every row of `red_stats`, `red_config`, and the user's own tables. `JOIN` from a SELECT-led statement is rejected even though the parser already implements `INNER / LEFT / RIGHT / FULL / CROSS JOIN` for FROM-led statements. Subqueries in expression context (`WHERE id IN (SELECT …)`) are rejected even though FROM-clause subqueries work.
- **Missing introspection surfaces.** `DESCRIBE <table>`, `SHOW CREATE TABLE`, and `SHOW INDEXES` are either parsed-and-empty or rejected outright. `SHOW INDICES` is recognized but the executor doesn't populate rows. Without a working introspection path, the only way to learn a table's schema is to `SELECT *` and inspect the keys, which is also leaking `red_*` columns until PRD #450 lands.
- **Presentation defaults that make expressions feel broken.** `SELECT UPPER(name)` returns `{ "UPPER": "UPDATED" }` instead of `{ "UPPER(name)": "UPDATED" }`. `SELECT id * 2` returns `{ "MUL": 2 }`. The default column name is the operator/function tag rather than the expression source, which makes every untyped client lookup harder and surprises users with prior SQL experience.

A handful of items the first batch flagged as broken are **now fixed in HEAD**: `'a' || 'b'` returns `'ab'`; `UPDATE` / `DELETE` / single `INSERT` return correct `affected` counts. These do not need new work, but they do imply the showcase ran against a binary older than the SDK's published 1.0.8 `red`, and a re-verification pass should land before any release notes claim them as new fixes.

This PRD owns the unfixed batch-2 items only. Items duplicated from batch-1 stay tracked under PRD #445 and issues #446–#465.

## Solution

Walk the unfixed batch-2 items end to end, with the same three-class disposition rule used by PRD #445:

1. **Fix.** Land the engine or driver change. Regression-test against the exact query the showcase tried.
2. **Reshape.** Where the user's syntax was wrong but the underlying capability exists, surface a better error or an SDK helper that emits the correct form.
3. **Defer.** Where the work is substantial and not blocking the demo (e.g. `RETURNING id` clause), file as a follow-up with explicit acceptance criteria.

The deliverable is that a user with an ordinary SQL background can write `SELECT a.name, b.title FROM authors a JOIN books b ON a.id = b.author_id`, run `db.query("SELECT … WHERE id = ?", [5])` safely, and introspect their schema with `DESCRIBE` or `SHOW CREATE TABLE` — without falling back to `SELECT *` + key-inspection or to string interpolation with manual escaping.

## User Stories

1. As an embedded-mode user, I want `db.query("SELECT … WHERE id = ?", [5])` to bind the placeholder safely, so that I do not have to interpolate values into SQL strings and absorb the injection risk myself.
2. As an embedded-mode user, I want the parameterised query path to also work with `INSERT`, `UPDATE`, and `DELETE`, so that the safe path covers every mutation, not just SELECT.
3. As an embedded-mode user, I want a `SELECT` that contains a `?` character to stay routed to the SQL parser instead of switching to SPARQL, so that the error I see (when there is one) is a SQL error pointed at my query.
4. As an embedded-mode user, I want `SELECT a.col, b.col FROM t1 a JOIN t2 b ON a.id = b.id` to parse and execute, so that I can express "characters and their tales" without materialising the join in TypeScript.
5. As an embedded-mode user, I want every JOIN flavor the parser already supports (`INNER`, `LEFT`, `RIGHT`, `FULL`, `CROSS`) to be reachable from a SELECT-led statement, so that the SQL surface is internally consistent.
6. As an embedded-mode user, I want `SELECT * FROM t WHERE id IN (SELECT id FROM other WHERE name = 'x')` to parse and execute, so that I can write standard correlated and uncorrelated subqueries instead of two-round-trip client logic.
7. As an embedded-mode user, I want subqueries to be supported in scalar position as well (`SELECT (SELECT MAX(value) FROM ts) AS peak`), so that the SQL surface is not asymmetric between `FROM` (works today) and expression contexts (rejected today).
8. As an embedded-mode user, I want `SELECT 'a' || 'b'`, `SELECT name || '!' FROM t`, and `SELECT CONCAT('a', 'b')` to all return the concatenated string `ab` / `name!` / `ab`, so that string assembly works regardless of how I spell it or whether one of the operands is a column reference.
9. As an embedded-mode user, I want `SELECT CURRENT_TIMESTAMP` to behave as a scalar function call equivalent to `NOW()`, so that I do not get a 107-row dump of every system table when I ask for the current time.
10. As an embedded-mode user, I want every other SQL standard "current" keyword (`CURRENT_DATE`, `CURRENT_TIME`) to either resolve to its function counterpart or fail with a clear `NOT_IMPLEMENTED` error, so that I do not hit the same "column lookup over system tables" surprise on a different keyword.
11. As an embedded-mode user, I want `DESCRIBE <table>` to return one row per column with `(name, type, nullable, default, indexed)` so that I can learn a table's shape without a `SELECT *` probe.
12. As an embedded-mode user, I want `SHOW CREATE TABLE <table>` to return the canonical `CREATE TABLE …` statement that would reproduce the table's current schema, so that I can copy a schema across `.rdb` files without writing migration glue.
13. As an embedded-mode user, I want `SHOW INDEXES` (and the equivalent `SHOW INDICES`) to return one row per index with `(name, table, columns, kind, unique, entries_indexed)`, so that I can verify the planner's choice of `index_seek` against the indexes I think I created.
14. As an embedded-mode user, I want `SELECT UPPER(name)` to return a column literally named `UPPER(name)` (and `SELECT id * 2` → `id * 2`, `SELECT name || '!'` → `name || '!'`), so that I can index into the row by the expression source instead of the operator tag.
15. As an embedded-mode user, I want explicit `AS <alias>` to continue to override the default column name, so that the new default-naming rule does not regress existing aliased queries.
16. As an embedded-mode user, I want `INSERT INTO t (...) VALUES (...) RETURNING id` (and `RETURNING *` / `RETURNING col1, col2`) to return the inserted row, so that I do not need a second `SELECT` to learn the assigned id or computed defaults.
17. As an embedded-mode user, I want `db.transaction(async (tx) => { ... })` to bracket my work with `BEGIN` and either `COMMIT` on success or `ROLLBACK` on throw, so that the BEGIN/COMMIT/ROLLBACK ceremony does not appear at the call site.
18. As a release maintainer, I want a follow-up verification pass on the batch-1 items that triage now believes are fixed in HEAD (`'a' || 'b'`, UPDATE/DELETE/INSERT affected counts) but were reported broken against published 1.0.8, so that I know whether the release notes can claim them as already-shipped or whether they need a backport.
19. As a release maintainer, I want each fixed item in this PRD to ship with a regression test that exercises the exact query the showcase tried, so that future engine releases do not silently regress these surfaces.
20. As a docs maintainer, I want a "What works in 1.0.x SQL" reference page listing every supported clause, operator, function, and DDL form, so that users can build with confidence instead of probing the parser.

## Implementation Decisions

The work decomposes into a small set of deep modules. None of them is new; each is a focused change inside an existing module with a stable public surface.

**Query-mode detector.** Today the detector flips a `SELECT` containing any `?` into SPARQL because SPARQL uses `?var` for variables. Tighten the heuristic so that `?` only triggers SPARQL when it is followed by a SPARQL-shaped variable name (`?` immediately followed by an identifier letter, in a position where SPARQL would expect one). A bare `?` placeholder (positional bind) or `?N` (numbered bind) stays in SQL mode. The change is intentionally minimal — the detector is a single function with no public-API surface area — and includes a regression test that covers `SELECT name FROM t WHERE id = ?`, `SELECT name FROM t WHERE id = ?1`, and a positive SPARQL test (`SELECT ?x WHERE { ?x rdf:type :Foo }`) to confirm SPARQL is not weakened.

**Parameterised query binder.** The engine already binds named and positional parameters in some paths. Once the detector stops misrouting, the SQL path needs to accept the `params` array argument (passed through from the JSON-RPC envelope) and substitute placeholders before execution. The binder is the single deep module that owns: (a) placeholder discovery in the parsed AST, (b) coercion from JSON types to schema `Value` variants per slot, and (c) injection of the resolved values back into the AST before lowering. The wire surface (`db.query(sql, params)`) does not change; the SDK already types it.

**SELECT-led JOIN dispatcher.** The full `INNER / LEFT / RIGHT / FULL / CROSS JOIN` parser already exists for FROM-led statements. Extend the SELECT-led entry point so that the same join-parsing helper is invoked after the first table reference is consumed. The downstream join executor is unchanged; only the dispatch from the parser's SELECT arm is added. This is a small change that unblocks story 4 and 5 together without growing the join surface.

**Subquery-in-expression evaluator.** Subqueries are already supported in FROM. Extend the expression parser so a parenthesised SELECT in an expression context (right-hand side of `IN`, `=`, comparison ops, or as a scalar operand) is accepted. The evaluator must execute the inner query before the outer comparison and feed its rows into the surrounding expression. Correlated subqueries are out of scope for the initial slice — only uncorrelated forms land first.

**Expression renderer for concatenation.** `'a' || 'b'` works today because the binary-op codegen handles it as a `CONCAT` operator. The remaining broken cases are: (a) `name || '!'` (column on one side) — the column's resolved value is re-emitted through the SQL printer and then concatenated as if it were a literal, producing the surrounding quotes; (b) `CONCAT(a, b)` — the function-call path takes a different render branch that always re-quotes its arguments. Unify both paths through a single string-coercion helper that converts any Value variant to its plain text form (no SQL-literal escaping) before concatenation. The helper is a deep module with one entry point and is easy to property-test.

**`CURRENT_TIMESTAMP` (and family) as scalar functions.** Today the parser treats `CURRENT_TIMESTAMP` as a column reference; the executor cannot resolve it against any table and the planner falls back to a system-collection scan. Recognize `CURRENT_TIMESTAMP`, `CURRENT_DATE`, and `CURRENT_TIME` as zero-argument function calls in the expression parser and route them to the existing time-source the `NOW()` builtin uses. The semantics chosen: `CURRENT_TIMESTAMP` returns the same value as `NOW()`; `CURRENT_DATE` and `CURRENT_TIME` extract the date / time portion respectively. Any other unrecognized bare keyword that the parser was previously treating as a column reference and that returns multi-table junk should fail-fast with a clear `UNKNOWN_REFERENCE` error in a follow-up — that fix is its own thin slice (story 10), not bundled here.

**Schema introspection module.** A single module owns the three forms `DESCRIBE <table>`, `SHOW CREATE TABLE <table>`, and `SHOW INDEXES` / `SHOW INDICES`. Each form is a parser branch that produces a dedicated AST node; each executor consults the existing catalog to produce row output. The row shape for `DESCRIBE` is `(name, type, nullable, default, indexed)`. The output of `SHOW CREATE TABLE` is a single-row, single-column result containing the canonical DDL string. `SHOW INDEXES` returns one row per index with `(name, table, columns, kind, unique, entries_indexed)`. `INDEXES` and `INDICES` are accepted as aliases at parse time.

**Expression default column-name policy.** The renderer that produces the result-set column names from a projection AST needs a single rule: if the projection has an explicit `AS <alias>`, use the alias; otherwise, render the projection's source-text form (the AST is preserved well enough for this — the lexer already tracks token spans). Drop the current behavior of emitting the operator / function tag (`MUL`, `UPPER`, `CONCAT`). The policy is one function and is testable in isolation against a representative set of expressions.

**`RETURNING` clause.** A new parser branch on `INSERT`, `UPDATE`, and `DELETE` that accepts `RETURNING *`, `RETURNING <col>, <col>, …`, or `RETURNING <expr>`. The mutation executor already computes the affected row set; surface it through the same result codec used by `SELECT`. The change is parser + executor wire-up, no new storage.

**SDK transaction wrapper.** A new `db.transaction(async (tx) => {...})` entry point that wraps `BEGIN`, runs the user's callback with a `tx` handle that has the same `query` / `insert` / `bulkInsert` surface as `db`, and either `COMMIT`s on resolve or `ROLLBACK`s on throw. `tx` operations are routed to the same JSON-RPC session, which the engine already detects as in-tx; the wrapper does not need engine changes. Exists in a single new module inside the JS driver.

**Documentation reference.** A "What works in RedDB 1.0.x SQL" doc page enumerating every supported clause, operator, builtin function, and DDL form. Generated where possible (e.g. from the lexer's keyword table + the parser's dispatch table); maintained where not (e.g. semantic notes on JOIN flavors and isolation levels). Lives in the existing docs tree and is referenced from the SDK README.

## Testing Decisions

The bar for every fix is: a test exercises the exact query the showcase reported and asserts a row-shape result, not an AST shape. Tests should not pin internal column names like `_entity_id` or `red_*`. Tests should treat the engine and the SDK as black boxes.

Modules covered by tests:

- **Mode detector.** Unit tests on the function directly: `SELECT name FROM t WHERE id = ?` → SQL; `SELECT name FROM t WHERE id = ?1` → SQL; `SELECT ?x WHERE { ?x rdf:type :Foo }` → SPARQL. Plus a higher-level integration test that runs a parameterised SELECT through `db.query` and asserts the result.
- **Parameterised binder.** Property-based test: a randomly generated SQL with N placeholders, each bound to a randomly typed JSON value within the slot's allowed types, executes equivalently to its inlined-literal counterpart.
- **SELECT-led JOIN.** Integration tests that mirror the existing FROM-led JOIN integration tests, one per join flavor. The showcase's `SELECT a.name, b.name FROM tt a JOIN tt b ON a.id = b.id` is one of them.
- **Subquery in expression.** Tests for `WHERE col IN (SELECT …)`, `WHERE col = (SELECT … LIMIT 1)`, and `SELECT col, (SELECT COUNT(*) FROM other) AS n FROM t`.
- **Concatenation renderer.** Unit tests on the string-coercion helper: `Value::Text` → no extra quotes; `Value::Integer` → decimal string; etc. Integration tests covering `'a' || 'b'`, `name || '!'`, `CONCAT('a', 'b')`, and `CONCAT(name, '!')` all returning the expected concatenated string.
- **`CURRENT_TIMESTAMP` family.** Tests that each form returns a scalar value of the correct shape (integer for `CURRENT_TIMESTAMP` and `CURRENT_TIME`, date string for `CURRENT_DATE`) and not a multi-row dump. Cross-check that `SELECT CURRENT_TIMESTAMP` and `SELECT NOW()` return values within a few milliseconds of each other.
- **Schema introspection.** A round-trip test: create a table with a few columns and an index → `DESCRIBE` returns the expected rows; `SHOW CREATE TABLE` returns a string that, when re-executed against a fresh database, produces an equivalent table; `SHOW INDEXES` returns the index by name and reports its column set.
- **Expression default column names.** Tests for every operator / function variant the user's feedback names: `UPPER(name)`, `id * 2`, `name || '!'`, `COALESCE(name, 'fb')`. Plus a test that `AS alias` still overrides.
- **`RETURNING` clause.** Tests for `INSERT … RETURNING id`, `INSERT … RETURNING *`, `UPDATE … RETURNING old_value, new_value`, `DELETE … RETURNING *`. Tests that the result-row count matches the affected count.
- **SDK transaction wrapper.** Tests that the success path commits, the throw path rolls back, and that an early `tx.query()` failure also rolls back. Round-trip with a real engine.

Prior art: existing parser integration tests in the SQL parser module; existing JOIN executor tests for FROM-led queries; the rpc_stdio integration tests that exercise the JSON-RPC envelope; the showcase's `pnpm insights` commands as the smoke test on top.

## Out of Scope

- Correlated subqueries (where the inner SELECT references columns from the outer query). The first slice supports uncorrelated subqueries; correlation is a follow-up.
- Window functions (`OVER (...)`), `WITH` (CTEs), `LATERAL JOIN`, `FULL OUTER JOIN` on graph-edge collections — none of these were in the feedback.
- A general "fail-fast on unknown bare identifiers in expression context" pass. Story 10 carves out `CURRENT_DATE` and `CURRENT_TIME` specifically; the broader cleanup (where today's parser treats unknown keywords as column references that fall through to system-collection scans) is a separate hardening project.
- `RETURNING <expr>` with arbitrary expressions over the affected row. The initial slice supports `RETURNING *` and `RETURNING <col>, <col>, …`; full expression support is a follow-up.
- Per-transaction isolation level DDL (`SET TRANSACTION ISOLATION LEVEL READ COMMITTED`). Snapshot isolation is already in use; explicit DDL is a separate project.
- Documentation generation tooling beyond the new "What works in 1.0.x SQL" page itself. If the keyword table or dispatch table grows the right shape for codegen, the doc is generated; if not, it is maintained by hand.

## Further Notes

Two items in the batch-2 feedback that the triage now believes are **already fixed in HEAD** but were reported as broken against published 1.0.8:

- `SELECT 'a' || 'b'` returns `'ab'` correctly when both operands are literals. The remaining `||` brokenness (column + literal) is owned by story 8 above.
- `UPDATE`, `DELETE`, and single `INSERT` return the correct `affected` count (1, 1, and the actual row count respectively). The showcase's `affected: 0` observation does not reproduce on a freshly-built `red` against the same SDK.

These do not need new engine work. They do imply that whatever binary the showcase resolved at `postinstall` time was older than the SDK's claimed 1.0.8. A separate verification slice should re-run the showcase against the next published binary and confirm both items show the fixed behavior end-to-end before the release notes claim them. That verification is bundled into the existing showcase smoke test (#465) — no new issue needed.

The new "wrong-syntax" surfaces from batch-2 are handled the same way as batch-1: they are not bugs, but the new parse-error vocabulary slice from PRD #445 (#451) covers the user-experience side, and the new SDK helpers (transaction wrapper, story 17) cover the discoverability side. No additional reshape work is required here beyond surfacing the `RETURNING` clause and the transaction wrapper.

This PRD intentionally does not re-open any item already owned by PRD #445 or by issues #446–#465. If a batch-2 finding looked like a duplicate of a batch-1 issue, the triage left it under the existing issue and that issue is the canonical tracker. The 13 unfixed items above are the only deltas.
