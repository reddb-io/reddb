# PRD: Parameterized queries (prepared statements) across engine, wire, and all drivers

Labels: prd

GitHub: https://github.com/reddb-io/reddb/issues/351

GitHub issue number: #351

## Problem Statement

Today RedDB's only query API across every driver (`@reddb-io/sdk`, Python, Go, Rust, Java, .NET, Kotlin, PHP, Dart, C++, Zig, Bun) is `query(sql: string)`. There is no way to send a SQL statement plus typed parameter values. Users must concatenate values into the SQL string themselves.

This is a problem for three separate audiences:

1. **Application developers** building on RedDB — they cannot write the idiomatic `client.query('SELECT * FROM users WHERE email = $1', email)` they expect from every other modern database (Postgres, SQLite, MySQL, MongoDB drivers, etc.). They are forced to either (a) hand-escape strings and risk SQL injection, or (b) abandon the SQL surface and use the model-specific helpers like `db.insert()`.

2. **AI / vector workloads** — the most common embedded use case for RedDB is `SEARCH SIMILAR <vec> IN <collection>` where `<vec>` is a `number[]` produced by an embedding model at runtime. Today the only way to send that vector is `JSON.stringify(vec)` glued into the SQL string. This works for `number[]` (no injection surface) but is awkward, allocation-heavy, and offers no batch / prepared reuse.

3. **Postgres-wire clients** — extended-protocol Parse/Bind/Execute is explicitly not implemented (`docs/api/postgres-wire.md:141`). Any standard Postgres client library (psycopg, pgx, JDBC, node-postgres) that uses prepared statements falls back to simple-query mode or breaks. This blocks the "drop-in Postgres replacement" story.

The gap is not partial: it is in every driver, in the wire protocol, and in the parser. There is no incremental workaround that fixes it for one transport.

## Solution

Introduce parameterized queries end-to-end:

- A SQL placeholder syntax (`$1, $2, ...` Postgres-style, plus `?` positional for SQLite/JDBC familiarity — both supported)
- A parameter-binding layer in the engine that takes a parsed AST plus a list of typed values and produces a bound AST ready for execution
- An extension to the RedWire protocol carrying `(sql, params: Vec<Value>)` as a first-class frame, alongside the existing string-only `Query` frame
- Postgres-wire Parse/Bind/Execute backed by the same binder
- A new `query(sql, params)` overload on every official driver, plus a typed `Value` mapping per language

Application code becomes:

```typescript
// JS / TS
const vec = await embed(userText)
await db.query(
  'INSERT INTO articles (title, body, dense) VALUES ($1, $2, $3)',
  [title, body, vec],
)

const hits = await db.query(
  'SEARCH SIMILAR $1 IN embeddings K $2 MIN_SCORE $3',
  [vec, 5, 0.7],
)
```

```python
# Python
await db.query(
  "SELECT * FROM users WHERE email = $1 AND active = $2",
  email, True,
)
```

```go
// Go
rows, err := db.Query(ctx,
  "SEARCH SIMILAR $1 IN embeddings K $2",
  vec, 5,
)
```

The original `query(sql)` signature stays valid (no params = no binding). Existing code does not break.

## User Stories

1. As a TypeScript developer using `@reddb-io/sdk`, I want to call `db.query(sql, params)` with a parameter array, so that I never have to concatenate user input into SQL strings.

2. As an AI engineer, I want to pass a `Float32Array` or `number[]` embedding directly as a query parameter, so that I do not have to `JSON.stringify` vectors into SQL.

3. As a Python developer, I want to pass parameters as positional `*args` to `db.query`, so that the API matches `psycopg` and `asyncpg` conventions I already know.

4. As a Go developer, I want `db.Query(ctx, sql, args...)` to accept variadic parameters, so that the API matches `database/sql` conventions.

5. As a Rust developer using the embedded API, I want `db.query_with(sql, &[param1, param2])` so that my code is safe by construction.

6. As a Java developer, I want `db.query(sql, Object... params)` matching JDBC `PreparedStatement.setObject` semantics, so that integration with existing Java codebases is natural.

7. As a .NET developer, I want `db.QueryAsync(sql, params object[] args)` matching ADO.NET conventions, so that RedDB feels like other .NET data providers.

8. As a Kotlin developer, I want a coroutine-friendly `db.query(sql, vararg params)` API, so that I can use it from `suspend` functions naturally.

9. As a PHP developer, I want `db->query(sql, [params])` matching PDO conventions, so that PHP applications can adopt RedDB without rewriting query layers.

10. As a Dart / Flutter developer, I want `db.query(sql, params)` with a `List<Object?>` parameters argument, so that my mobile app can store data safely without raw SQL building.

11. As a C++ developer, I want a parameter API based on `std::variant` or a typed `Value` class, so that template safety prevents type mismatches at compile time.

12. As a Zig developer, I want a slice of tagged-union `Value` parameters, so that the API is idiomatic Zig without hidden allocations.

13. As a Bun developer, I want the same `@reddb-io/sdk` API to work transparently in Bun's runtime, so that I do not need a separate driver.

14. As a developer integrating via Postgres clients, I want `psycopg`, `pgx`, `pg-promise`, and JDBC to use prepared statements normally, so that existing application frameworks work unchanged.

15. As a developer using positional placeholders, I want `$1, $2` (Postgres-style) to be the canonical form, so that queries are portable to/from Postgres.

16. As a developer migrating from SQLite, I want `?` placeholders to also be accepted, so that I can copy queries directly without rewriting them.

17. As a developer using parameters, I want a clear error if I pass too few or too many parameters for the placeholders in my SQL, so that mistakes are caught immediately rather than producing wrong results.

18. As a developer, I want a clear error if I pass a parameter of the wrong type for a placeholder context (e.g., a string where a vector is required), so that type mismatches surface at bind time, not query time.

19. As a developer, I want `NULL` to be a representable parameter value in every driver (not just an empty string), so that nullable columns are supported correctly.

20. As a developer, I want to pass binary blobs as parameters (`Uint8Array` / `bytes` / `[]byte` / `byte[]`), so that I can store binary content without base64 encoding in my application code.

21. As a developer, I want to pass JSON objects / nested structures as parameters, so that document inserts can use parameterization too.

22. As a developer, I want to pass timestamps / dates as parameters using each language's native type (`Date`, `datetime`, `time.Time`, `Instant`), so that I do not have to format dates as strings.

23. As a developer using AUTO EMBED, I want `INSERT INTO ... VALUES ($1, $2) WITH AUTO EMBED ...` to work with parameters in the VALUES clause, so that auto-embedding works in safe parameterized inserts.

24. As a developer, I want `SEARCH SIMILAR TEXT $1 USING openai` to accept parameterized text, so that I can search by user-provided strings safely.

25. As a developer, I want parameterized values for `K`, `LIMIT`, `OFFSET`, and `MIN_SCORE` clauses, so that pagination and tuning are dynamic and safe.

26. As a developer, I want parameter binding to work for all data models (tables, documents, KV, graph, vector, time-series, queue), so that the safety guarantee is not surface-area dependent.

27. As a developer, I want the same parameter syntax to work over RedWire TCP, gRPC, HTTP, embedded stdio JSON-RPC, and Postgres-wire, so that I can switch transports without rewriting queries.

28. As a developer using HTTP, I want a JSON request body shape like `{"sql": "...", "params": [...]}`, so that custom HTTP integrations can pass parameters too.

29. As a developer using the gRPC / RedWire binary protocol, I want a parameter list encoded compactly (not as JSON strings), so that vector inserts have minimal overhead.

30. As an operator, I want servers that predate parameter support to negotiate down gracefully when a new client connects, so that rolling upgrades do not break.

31. As an operator, I want clients that predate parameter support to keep working against new servers, so that I can upgrade the server first.

32. As a Postgres-wire user, I want `Parse / Bind / Execute / Describe / Close` to be implemented, so that drivers using prepared statements work.

33. As a Postgres-wire user, I want `$1` placeholders in `Parse` to map to the same binder used by other transports, so that behavior is consistent.

34. As a developer, I want to reuse a parsed statement across multiple binds (true prepared-statement reuse), so that hot-path queries skip re-parsing — even if this lands in a follow-up issue, the wire shape should support it.

35. As a developer, I want parameterized queries inside transactions, so that all the safety benefits apply to multi-statement work units.

36. As a developer of an ORM or query builder, I want a stable parameter contract documented in an ADR, so that I can target it confidently.

37. As a developer, I want example code in every driver's README showing the parameterized form (especially with vectors), so that the safe pattern is the obvious default.

38. As a developer reading `docs/data-models/vectors.md`, I want every vector example to use the parameterized form, so that I do not learn the unsafe string-concat pattern from official docs.

39. As a security reviewer, I want a guarantee that parameter values cannot be reinterpreted as SQL, so that injection is impossible by construction.

40. As a developer, I want clear behavior when a parameter is referenced multiple times in one SQL statement (e.g., `WHERE a = $1 OR b = $1`), so that reuse is well-defined.

41. As a developer, I want documentation on how `?` and `$N` interact (mixing forbidden? `?` numbered left-to-right?), so that I am not surprised.

42. As a maintainer of a community driver in a language we do not officially ship, I want a written wire spec for the parameter frame, so that I can implement compatibly.

43. As a developer, I want benchmarks comparing string-concat vs parameterized inserts (especially bulk vector inserts), so that I know the performance characteristics.

44. As a developer using the CLI (`red sql ...`), I want a way to pass parameters from the command line or a JSON file, so that scripted queries are also safe.

45. As a developer using MCP (`docs/api/mcp.md`), I want parameterized queries exposed as MCP tools, so that LLM agents producing queries do so safely.

## Implementation Decisions

**Placeholder syntax.** Both `$N` (Postgres-style, canonical) and `?` (positional, SQLite/JDBC) are accepted. Mixing the two in a single statement is rejected. With `?`, placeholders are numbered left-to-right starting at 1 and map to the same parameter list. With `$N`, the same `$N` may appear multiple times and refers to the same parameter slot.

**Engine — placeholder parser (deep module).** A pure function that scans SQL text and returns the set of placeholder slots, their positions, and a normalized "max slot index". Pure, no engine dependencies, testable in isolation. Detects: undefined gaps (e.g., `$1` and `$3` without `$2`), mixed `$N`/`?`, references inside string literals (must be ignored).

**Engine — parameter binder (deep module).** A pure function `bind(ast, params: &[Value]) -> Result<BoundAst, BindError>`. Validates arity (params length matches max slot), validates type compatibility per placeholder context (vector context requires `Value::Vector`, K/LIMIT requires integer, etc.), and substitutes placeholders with literal AST nodes. Pure, isolated, testable.

**Engine — `Value` enum.** A typed value representation covering: null, bool, int (64-bit), float (64-bit), text, bytes, vector (`Vec<f32>`), json (canonical), timestamp (epoch nanos), uuid. This is the engine-internal lingua franca — every wire codec, every driver, and the binder all agree on this shape. Extending later (e.g., decimals) is additive.

**Engine — AST node.** New `Expr::Placeholder(slot: u32)` variant. The parser emits these; the binder rewrites them away before execution. Execution layers never see placeholders.

**RedWire — new frame `QueryWithParams` (deep module: wire Value codec).** Compact binary encoding of `(sql: String, params: Vec<Value>)`. The Value codec is its own deep module: pure encode/decode, round-trip testable, used by both client and server. Frame versioning per ADR 0001 — old `Query` frame stays untouched; clients negotiate capability.

**HTTP transport.** New JSON shape: `{"sql": "...", "params": [...]}`. Existing `{"query": "..."}` continues to work. Params are typed JSON: numbers, strings, booleans, null, arrays (vectors), objects (json), and `{"$bytes": "<base64>"}` / `{"$ts": <nanos>}` / `{"$uuid": "..."}` for non-JSON-native types.

**Embedded stdio JSON-RPC.** New method `query_with_params` (or `query` accepting an optional `params` field — to be decided in the ADR). Same Value shape as HTTP for consistency.

**Postgres-wire (extended protocol).** Implement `Parse`, `Bind`, `Describe`, `Execute`, `Close`. Parameter parsing maps Postgres OIDs to engine `Value` types. Reuses the same binder as other transports — single source of truth. Closes the gap noted in `docs/api/postgres-wire.md:141`.

**Drivers — common contract.** Every official driver gets `query(sql, params)` matching language idioms:

- JS/TS/Bun: `db.query(sql, params?: unknown[])`
- Python: `db.query(sql, *params)` plus `db.query(sql, params=[...])`
- Go: `db.Query(ctx, sql, params ...any)`
- Rust (embedded + remote): `db.query_with(sql, &[Value])` with `IntoValue` trait
- Java: `db.query(String sql, Object... params)`
- .NET: `db.QueryAsync(string sql, params object[] args)`
- Kotlin: `suspend fun query(sql: String, vararg params: Any?)`
- PHP: `$db->query(string $sql, array $params = [])`
- Dart: `db.query(String sql, [List<Object?>? params])`
- C++: `db.query(std::string_view sql, std::span<const Value> params)`
- Zig: `db.query(sql: []const u8, params: []const Value)`

Each driver maps its native types to engine `Value` (deep module per driver: parameter serialization).

**Driver — vector support.** All drivers must accept the language's natural vector type (`Float32Array` or `number[]` in JS, `numpy.ndarray` and `list[float]` in Python, `[]float32` in Go, `&[f32]` in Rust, `float[]` in Java/C#, etc.) and serialize as `Value::Vector` with no allocation surprises documented.

**Backwards compatibility.** Original `query(sql)` signature stays. No params = no binding overhead. Old wire `Query` frame stays. Capability negotiation per ADR 0001 + 0010 (wire adapters translate, never duplicate). Old client + new server: works (uses old frame). New client + old server: degrades to string-concat with deprecation warning, OR errors loudly — to be decided in the ADR.

**ADR.** A new ADR `00XX-parameterized-queries.md` codifies: placeholder syntax, `Value` enum surface, wire frame layout, capability negotiation, error taxonomy, and the deprecation policy for unsafe string-concat patterns in docs and examples.

**Documentation.** Every vector / SEARCH / INSERT example in `docs/data-models/`, `docs/vectors/`, `docs/query/`, and every driver guide in `docs/clients/` and `docs/guides/` is updated so the parameterized form is the default shown to new users.

## Testing Decisions

**What makes a good test here.** Tests verify external behavior at module boundaries — they call public APIs with input values and assert output values or error variants. They do not assert on internal state, internal types, or internal function call sequences. They survive refactors of the implementation.

**Tested modules.**

- **Placeholder parser.** Pure unit tests: well-formed `$1..$N`, well-formed `?...?`, mixed (rejected), gaps (rejected), placeholders inside string literals (ignored), placeholders inside comments (ignored), empty parameter list, very large indices, repeated `$N`. Property-based tests: random valid sequences round-trip through parse → reconstruct.

- **Parameter binder.** Pure unit tests: correct arity → bound AST; wrong arity → arity error; type mismatch per context (vector required, integer required, text required) → typed error variants; null in nullable position → ok; null in non-nullable position → typed error; reused `$N` substituted consistently. Cover every clause that can take a parameter: VALUES, WHERE, K, LIMIT, OFFSET, MIN_SCORE, SEARCH SIMILAR vector slot, SEARCH SIMILAR TEXT slot.

- **Wire Value codec.** Round-trip property tests for every Value variant: null, bool, int (boundary values incl. i64::MIN/MAX), float (incl. NaN, ±inf, subnormals), text (incl. empty, multi-byte UTF-8, very long), bytes (incl. empty, very long), vector (incl. empty, 1024-dim, 4096-dim, NaN entries), json (nested), timestamp (epoch, far past, far future), uuid. Cross-language round-trip: Rust encodes → JS/Python/Go decodes and re-encodes → bytes match.

- **Driver-side parameter serialization (per driver).** For each driver, a unit/integration test that constructs every native parameter type and asserts the wire bytes match expected fixtures shared across drivers (golden files). This is the contract test that keeps drivers in sync.

- **Postgres-wire extended protocol.** Integration tests against `psycopg`, `pgx`, `pg-promise`, and JDBC issuing prepared `SELECT`, `INSERT`, and vector queries. Assertions on returned rows and on absence of fallback to simple-query mode.

- **End-to-end across transports.** A matrix test that issues the same parameterized query (text, int, vector params) over RedWire, gRPC, HTTP, embedded stdio, and Postgres-wire and asserts identical results.

**Prior art in the codebase.**

- Wire codec round-trip tests live near `crates/reddb-wire/` (sanitizer, conn_string).
- Parser hardening fixtures live under `crates/reddb-wire/tests/support/parser_hardening/` — the placeholder parser tests should follow that style.
- Cross-driver fixture / golden-file pattern: ADR 0010 (`wire-adapters-translate-never-duplicate`) implies a shared canonical representation; this PRD aligns with that direction.
- Per-driver smoke / cache tests already exist (`drivers/js/test/`, `drivers/python/`); extend them rather than starting new test harnesses.

## Out of Scope

- **True server-side prepared-statement caching** (parse-once, bind-many across requests). The wire shape designed here must not preclude it, but landing the cache itself is a follow-up issue.
- **Named parameters** (`:foo`, `@foo`). The ADR should call out that the slot model is positional only; named parameters can be added later as a layer above (driver-side rewrite to `$N`) without protocol changes.
- **Server-side cursor / batched Bind** (Postgres `Bind` with multiple parameter sets). Future extension.
- **Type coercion beyond the obvious** (e.g., implicit string-to-vector parsing). Parameters must match the placeholder context type; coercion rules are explicit only for numeric widening (i32 → i64, f32 → f64).
- **Migration of every example in every doc page in this PRD.** A docs-update issue is created as a child; the PRD only mandates the new ADR + the examples in the highest-traffic guides (vector data model, JS/Python driver guides).
- **Removing or breaking the existing `query(sql)` signature.** This is a strict superset.
- **CLI parameter passing UX details.** A child issue handles `red sql --param vec=@file.json` ergonomics.

## Further Notes

- This blocks the "drop-in Postgres replacement" narrative — see `docs/api/postgres-wire.md:141-142` (current note that extended protocol is unimplemented). Implementing this PRD removes that caveat.
- This unblocks safe AI / vector workloads which today must hand-serialize embeddings into SQL strings — see `docs/data-models/vectors.md` and the embedded examples in `docs/api/embedded.md:97-105`.
- Aligns with ADR 0010 (`wire-adapters-translate-never-duplicate`): the new `Value` enum and binder are the single canonical representation; HTTP, gRPC, RedWire, Postgres-wire, and embedded stdio all translate to/from it without duplicating logic.
- The `$N` + `?` dual-syntax decision adds parser surface but matches the "users come from Postgres or SQLite" reality.
- All twelve official drivers ship the new API in the same release. The deep modules (placeholder parser, binder, wire Value codec) live in the engine and are the single source of truth — drivers only own the language-side type mapping.
- A child PRD handles documentation sweep across `docs/` so the parameterized form becomes the default in every example.
