# PRD: Close multi-model SDK gaps from the Grimms showcase feedback

Labels: prd, needs-triage

## Problem Statement

A multi-model demo author building against `@reddb-io/sdk@1.0.8` in embedded (`file://` / stdio JSON-RPC) mode produced a detailed feedback report after trying to exercise graph + timeseries + KV + tables + probabilistic structures in a single `.rdb` file. The report flagged 20+ items as broken, rough, or missing.

A code-cite triage plus live repro against the current `main` HEAD shows that **only one item (single-node `MATCH … RETURN n.field` projection) is genuinely fixed since 1.0.8**. Two more (SKETCH/FILTER create-time parameters) are partially fixed. The remaining issues are still present in HEAD, span both the engine and the JS driver, and block the "multi-model in one file" story that the embedded mode is positioned to tell.

The cost of leaving these open:

- A user that lands on the embedded experience and tries Cypher-style traversal gets a silent cross-product (multi-node `MATCH` returns the union of single-node matches with the edge label discarded) and walks away believing the graph engine is unusable.
- Probabilistic collections (HLL / Count-Min sketch / Bloom filter) accept inserts, accept DDL, advertise themselves in `SHOW COLLECTIONS`, and have working executor paths — but the user-facing commands (`HLL COUNT`, `SKETCH COUNT`, `FILTER CHECK`) are not discoverable from the SQL surface they tried (`SELECT CARDINALITY …`, `SKETCH ESTIMATE …`).
- `CREATE VECTOR | DOCUMENT | GRAPH | COLLECTION` is rejected by the parser despite each token appearing in the parse-error's *expected* list, which is actively misleading.
- The JS driver leaks SDK-specific behavior (silent rewrite of `:` to `_` in KV keys; HTTP-only cache client surface; no queue client; no `kv.get`; no `id` on `insert` envelope; per-row stdio for `bulkInsert`) that callers can't work around without reading the driver source.
- Timeseries `tags` columns come back over the wire as the literal string `<json N bytes>`, even though the same `Value::Json` round-trips correctly in other presentation paths.

This is a closure project, not new functionality. The motivating goal is that the embedded multi-model demo runs end-to-end on the next published SDK release without TS-side reimplementations of graph traversal, vector similarity, or probabilistic estimate reads.

## Solution

Treat the feedback list as a fixed inventory and resolve every item into one of three states:

1. **Fix.** Land the engine or driver change. Cover with a regression test that exercises the exact surface the showcase tried.
2. **Reshape.** Where the user's syntax was wrong but the underlying capability exists (e.g. `KV GET corpus_version` not `KV GET 'corpus_version'`; `QUEUE PUSH q v` not `PUSH q v`; `SKETCH COUNT name 'x'` not `SKETCH ESTIMATE`), produce a parser-side better error or an SDK helper that emits the correct form, plus documentation.
3. **Defer.** Where the work is large and not required to unblock the demo (e.g. on-disk vector collection with native cosine search, per-metric timeseries retention/downsampling), split into a separate follow-up issue with explicit acceptance criteria.

The deliverable is a complete walkthrough of the showcase queries against a freshly-built `red` binary and the next SDK release, with no TS-side reimplementations and no `<json N bytes>` placeholders in the output.

## User Stories

1. As an embedded-mode user, I want `MATCH (a)-[:LABEL]->(b) RETURN a, b` to filter by edge label, so that Cypher-style traversal returns the actual subgraph and not the cross-product of node matches.
2. As an embedded-mode user, I want multi-node MATCH patterns to actually traverse edges, so that I do not have to write a TS-side pattern matcher on top of the graph store.
3. As an embedded-mode user, I want `MATCH (n) RETURN n LIMIT k`, so that exploratory queries do not have to materialize the entire match set.
4. As an embedded-mode user, I want `SELECT col FROM <graph-collection>` to project the named columns of stored nodes, so that table-shaped reads work without falling back to aggregates.
5. As an embedded-mode user, I want a column literally named `count` to participate in aggregates (`SELECT word, SUM(count) FROM tw GROUP BY word`), so that idiomatic word-frequency tables do not have to be renamed to avoid a reserved-keyword collision.
6. As an embedded-mode user, I want `CREATE GRAPH <name>`, `CREATE VECTOR <name>`, `CREATE DOCUMENT <name>`, and `CREATE COLLECTION <name> KIND <kind>` to either succeed or fail with an error that tells me the token is recognized but not yet supported here, so that the parse-error's "expected" list stops lying to me.
7. As an embedded-mode user, I want `CREATE HLL <name> PRECISION p` to accept a precision parameter, so that I can tune cardinality accuracy at create time the way SKETCH and FILTER already accept their parameters.
8. As an embedded-mode user, I want HLL / SKETCH / FILTER estimate reads to be reachable from SQL (`SELECT CARDINALITY FROM hll`, `SELECT FREQ('x') FROM sketch`, `SELECT CONTAINS('x') FROM filter`) and not only from the command-form (`HLL COUNT`, `SKETCH COUNT`, `FILTER CHECK`), so that probabilistic collections feel like first-class read sources.
9. As an embedded-mode user, I want `SHOW COLLECTIONS` to report HLL / SKETCH / FILTER under their declared probabilistic kind, not as generic `table`, so that the metadata round-trip is honest.
10. As an embedded-mode user, I want `db.kv.put('corpus:version', '1.0.0')` to either preserve the `:` character in the key or reject the put with a clear error, so that a later `db.kv.get('corpus:version')` returns the value I stored.
11. As an embedded-mode user, I want a `db.kv.get(key)` (and `getMany`) method on the SDK, so that I can read KV values without dropping down to raw `db.query()`.
12. As an embedded-mode user, I want `db.queue` on the SDK with `push`, `pop`, `peek`, and `len` methods that wrap the existing `QUEUE PUSH/POP/...` commands, so that creating a queue is followed by a working write path.
13. As an embedded-mode user, I want `db.cache.put/get` to either work against the embedded transport or throw a clear `UNSUPPORTED_TRANSPORT` error before the network call, so that the typed surface does not advertise a method that always fails in this mode.
14. As an embedded-mode user, I want `SELECT tags FROM <timeseries>` to return the actual JSON value I stored, not the literal string `<json N bytes>`, so that I can read back the structured metadata I wrote.
15. As an embedded-mode user, I want `db.insert(...)` and `db.bulkInsert(...)` to return the assigned entity ids, so that I do not have to assume sequential id allocation under concurrent writers.
16. As an embedded-mode user, I want `GRAPH NEIGHBORHOOD '<label>'` to accept a node label and resolve it internally, so that I do not have to know the numeric entity id.
17. As an embedded-mode user, I want `GRAPH NEIGHBORHOOD` and `GRAPH TRAVERSE` to accept an `EDGES IN (...)` filter, so that I can produce a labelled-subgraph view in one query instead of post-filtering in TS.
18. As an embedded-mode user, I want a `SELECT *` variant that returns only the user-declared columns (with the `red_*` / `created_at` / `updated_at` metadata available under an opt-in form like `SELECT * WITH METADATA`), so that user-facing tables are not noisy.
19. As an embedded-mode user, I want `bulkInsert` to pack many payloads into a single multi-row `INSERT … VALUES` over the wire when no transaction is open, so that ingest of tens of thousands of rows does not pay the round-trip cost per row.
20. As an embedded-mode user, I want `db.exists(collection)` and `db.list()` helpers, so that idempotency guards do not require a `SELECT COUNT(*)` inside a `try/catch`.
21. As an embedded-mode user, I want generic typing on collection reads (`db.from<{ word: string; freq: number }>('tale_words')`), so that I stop reaching into untyped `row['col']` access on every projection.
22. As an embedded-mode user, I want a parse error that distinguishes "I do not recognize this token" from "I recognize this token but do not accept it here", so that the false-positive *expected* list in the existing error stops sending me down dead-end syntax searches.
23. As a release maintainer, I want every fixed item to ship with a regression test that exercises the exact query the showcase tried, so that future SDK / engine releases do not silently regress the multi-model walkthrough.
24. As a release maintainer, I want the showcase's `pnpm insights …` commands to all run against the next published SDK with no TS-side reimplementations of graph traversal, vector similarity, or probabilistic reads, so that the demo can be the canonical multi-model on-ramp.
25. As an embedded-mode user, I want `CREATE VECTOR <name> DIM <d>` to actually create a vector collection with a native cosine / L2 / inner-product similarity index, so that `pnpm insights cosine` can drop its TS-side similarity fallback and call the engine directly.
26. As an embedded-mode user, I want `VECTOR SEARCH <coll> SIMILAR TO [...]` to return results from a real on-disk vector index, so that similarity queries scale past what an in-memory TS implementation can serve.

## Implementation Decisions

The work decomposes into a small set of deep modules, each with a narrow public surface and the bulk of the behavior hidden behind it. Where a module already exists, the change is scoped to its existing entry point.

**Engine — graph match executor.** Extend the multi-node `MATCH` execution path so that an edge pattern `(a)-[r:LABEL]->(b)` performs actual edge expansion against the graph store and applies the label filter. The single-node fast path already projects correctly and stays unchanged. The module's interface (planner-emitted `GraphQuery` → row stream) does not change; only the per-pattern matcher does. Multi-node patterns continue to compose with `WHERE` filters and the existing projection layer.

**Engine — MATCH parser tail.** Accept an optional `LIMIT n` after the `RETURN` list in the `MATCH` parser, with the same semantics as `SELECT … LIMIT`. The MATCH AST gains an optional limit field; the executor short-circuits after `n` matches.

**Engine — DDL surface.** Accept `CREATE GRAPH <name>`, `CREATE VECTOR <name> DIM <d> [METRIC cosine|l2|inner_product]`, `CREATE DOCUMENT <name>`, and `CREATE COLLECTION <name> KIND <kind>` at the top level. Where the underlying storage support is not ready yet (currently: document), the parser accepts the form and the executor returns a clear `NOT_YET_SUPPORTED` error instead of the current confusing parse error. Where storage is ready (graph, vector — see the next module), the DDL creates the collection. `CREATE HLL <name> PRECISION p` is accepted with the same shape as the existing SKETCH/FILTER parameter syntax.

**Engine — native vector collection.** A new collection kind backed by an on-disk vector index. The module owns: (a) per-collection dimension and distance metric, fixed at create time; (b) row inserts of the shape `INSERT INTO <coll> (id, embedding, metadata) VALUES (...)`, where `embedding` is a fixed-length float array; (c) `VECTOR SEARCH <coll> SIMILAR TO [...] [METRIC m] [THRESHOLD t] [LIMIT k]` returning top-k matches with scores. The initial index is brute-force scan (correct, predictable, O(N·d) per query); the index abstraction is designed so that an HNSW or IVF implementation can replace the brute-force scan without touching the executor or the wire surface. The metric set is `cosine`, `l2`, `inner_product`; the default is `cosine`.

**Engine — probabilistic SQL read surface.** Add a SQL-level read form for HLL / SKETCH / FILTER that maps to the existing `HllCount` / `SketchCount` / `FilterCheck` runtime ops. The shape should mirror what the user tried: `SELECT CARDINALITY FROM <hll>`, `SELECT FREQ('x') FROM <sketch>`, `SELECT CONTAINS('x') FROM <filter>`. The existing command form (`HLL COUNT name`) is preserved.

**Engine — collection metadata.** When a collection is created via `CREATE HLL/SKETCH/FILTER`, ensure `SHOW COLLECTIONS` reports its model under its declared kind, not as `table`. The fix is in the catalog renderer, not the underlying physical store.

**Engine — graph traversal command parser.** Extend `GRAPH NEIGHBORHOOD` and `GRAPH TRAVERSE` to accept an optional `EDGES IN ('label1', 'label2', ...)` filter and an optional `BY LABEL` source resolution. The executor consults the existing label registry to resolve a string source argument to an entity id when the literal is not a numeric id.

**Engine — value rendering on SELECT.** Route `Value::Json` columns in the SELECT result codec through the existing `storage_value_to_json` decoder rather than the `Display` fallback that produces `<json N bytes>`. The bug is path-specific to the SELECT presentation; other paths already decode correctly.

**Engine — INSERT result envelope.** Surface the assigned entity id in the stdio JSON-RPC `insert` and `bulk_insert` responses. The existing helper that extracts `_entity_id` from the first result record is correct in shape but never fires for SQL INSERT because the result record is empty; populate it from the entity created in the application layer. For `bulk_insert`, return an array of ids matching the input order.

**Engine — bulk insert path.** When the stdio handler receives `bulk_insert` outside a transaction, build a single multi-row `INSERT … VALUES (...), (...), ...` statement per chunk and execute it once, rather than running one execute per payload. The chunk size is bounded by an internal limit (initial value: 500 rows per statement).

**Engine — reserved-keyword handling for `count`.** Allow `count` to be used as an identifier in column reference contexts (column lists in CREATE/INSERT, projection targets, GROUP BY keys, function arguments). The lexer keeps the keyword for `COUNT(...)` function calls; the parser disambiguates by context. Mirror handling for any other aggregate keyword that can plausibly appear as a user column name.

**Engine — parse error vocabulary.** When a token appears in the lexer but is not accepted at the current parser state, emit `"token X is not yet supported in this position"` instead of listing X among the expected set. This is a presentation-only change in the parse-error formatter and affects every parser rule.

**Engine — `SELECT *` metadata gate.** Add a `SELECT * WITH METADATA` form that opts into the `red_*` / `created_at` / `updated_at` columns. The default `SELECT *` returns only the user-declared columns. Existing callers that depend on metadata leakage must opt in.

**SDK — KV client.** Stop the silent regex rewrite of non-alphanumeric characters in keys. Either preserve the key by emitting a quoted-identifier in the SQL, or reject the put with a clear `INVALID_KV_KEY` error pointing at the offending character. Add `get(key, options?)` and `getMany(keys, options?)` methods that wrap `KV GET <collection>.<key>`.

**SDK — Queue client.** Add a new `QueueClient` exposing `push(queue, value, options?)`, `pop(queue, count?)`, `peek(queue, count?)`, `len(queue)`, and `purge(queue)`, each wrapping the corresponding engine command. Expose it as `db.queue` on the main client.

**SDK — Cache client transport guard.** When the underlying transport is embedded (stdio JSON-RPC) or any transport that does not implement `cache.*` server methods, every `CacheClient` method throws `UNSUPPORTED_TRANSPORT` before issuing the call. The typed surface (`index.d.ts`) gains a note that cache is HTTP / gRPC only.

**SDK — DB-level helpers.** Add `db.exists(collection): Promise<boolean>` and `db.list(): Promise<CollectionMeta[]>`, both wrapping existing catalog queries. Add a generic `db.from<T>(collection)` that returns a typed query builder with row narrowing.

**SDK — `insert` / `bulkInsert` return shape.** Once the engine surfaces ids, propagate them to the public types: `insert(collection, payload): Promise<{ affected: number; id: string | number }>` and `bulkInsert(collection, payloads): Promise<{ affected: number; ids: Array<string | number> }>`.

## Testing Decisions

A good test for this work asserts the externally observable behavior that the showcase author tried to use: a query string in, a row shape out. Tests should treat the engine and the SDK as black boxes — they should not pin internal AST shapes, internal column names like `_entity_id`, or specific parser positions, because those rot fast.

Modules covered by tests:

- The graph match executor. New integration tests under the existing graph executor test layout exercising `MATCH (a)-[:LABEL]->(b) RETURN a, b` against a fixture graph where the answer is small enough to enumerate (e.g. 3 nodes, 1 labeled edge), plus a negative test asserting the label filter is honored.
- The MATCH parser. Unit tests that the optional `LIMIT n` parses and that the resulting AST carries the limit.
- The DDL surface. Unit tests for each new accept form (`CREATE GRAPH`, `CREATE VECTOR … DIM d`, `CREATE DOCUMENT`, `CREATE COLLECTION`, `CREATE HLL … PRECISION p`), and a test that the new "token not supported here" parse-error variant fires for the still-deferred kinds.
- The native vector collection. Integration tests covering: (a) `CREATE VECTOR v DIM 4 METRIC cosine` followed by inserts of correctly-sized embeddings; (b) inserts of wrong dimension fail with a clear error; (c) `VECTOR SEARCH` against a small fixture (e.g. 10 vectors) returns results in the documented order for each metric; (d) `LIMIT k` and `THRESHOLD t` clauses are honored; (e) a golden test comparing the engine's cosine top-k against the showcase's TS-side cosine implementation on the same fixture, asserting identical ordering.
- The probabilistic SQL read surface. Tests that mirror the existing `HLL COUNT` / `SKETCH COUNT` / `FILTER CHECK` integration tests but go through the SQL read form.
- The catalog renderer. Test that `SHOW COLLECTIONS` reports the correct model field for each probabilistic collection.
- The graph traversal command parser. Tests for `EDGES IN (...)` and for label-source resolution on `NEIGHBORHOOD`.
- The SELECT value-rendering path. A test that round-trips a timeseries row with a non-trivial `tags` JSON object and asserts the response is structured JSON, not a placeholder string.
- The INSERT envelope. End-to-end tests via the stdio JSON-RPC path that `insert` returns an `id` and `bulk_insert` returns `ids`.
- The bulk insert wire packing. A benchmark-style test that confirms `bulk_insert` of N rows produces ≤ ceil(N/500) underlying execute calls (counted via a runtime probe).
- The reserved-keyword handling. A unit test that the showcase's exact query (`SELECT word, SUM(count) FROM tw GROUP BY word` with `count INTEGER` column) parses, executes, and returns the expected aggregate.
- The `SELECT *` metadata gate. A test that `SELECT *` does not return `red_*` columns and a paired test that `SELECT * WITH METADATA` does.
- SDK KV client. Tests that `kv.put('a:b', v)` either preserves the colon round-trip via `kv.get('a:b')`, or rejects with `INVALID_KV_KEY`. A test that `kv.get` returns the put value for both default and dotted-collection forms.
- SDK Queue client. Tests that wrap the engine's `QUEUE PUSH/POP/PEEK/LEN/PURGE` and assert the round-trip shape.
- SDK Cache client transport guard. A test that every cache method throws `UNSUPPORTED_TRANSPORT` when the underlying client is the stdio RPC client.

Prior art the tests should mirror: existing graph executor integration tests in the unified executor module; existing probabilistic command tests; existing KV parser tests; the rpc_stdio integration tests that go through the JSON-RPC envelope; the showcase's own `pnpm insights …` queries as a smoke test once both fixes are in place.

## Out of Scope

- Approximate vector indexes (HNSW, IVF, PQ). The native vector collection lands with a brute-force scan that is correct, deterministic, and adequate for the showcase's scale (≤ tens of thousands of vectors). Replacing the index with an ANN structure is a separate PRD that can land without touching the executor or the wire surface.
- Native document store backing `CREATE DOCUMENT <name>` beyond what the existing auto-table provides. The DDL form is accepted and returns `NOT_YET_SUPPORTED`; full document semantics (path indexing, query operators) is a separate scope.
- Per-metric timeseries retention and downsampling (`CREATE TIMESERIES … RETENTION 30d DOWNSAMPLE 1m AVG`). Flagged in the feedback as a `💡 idea`; tracked as a follow-up.
- TypeScript codegen from collection schema. The proposed `db.from<T>(collection)` accepts the caller's hand-written `T`; full schema-to-types generation is a separate scope.
- The `red_kind` "structured,table" capability string returned by `SELECT CAPABILITIES FROM <prob>` is left as-is once the collection metadata reports the correct model — the capability vocabulary is a separate piece of work.
- Renaming `count` in any internal contract. The reserved-keyword fix is parser-side disambiguation only.

## Further Notes

The feedback list contained a handful of items that turned out not to be bugs but wrong syntax. The PRD intentionally addresses these in the *Reshape* class by improving the parser's error vocabulary (user story 22) rather than changing the engine's syntax surface:

- `KV GET 'corpus_version'` (quoted) — engine syntax is a bare identifier or dotted form, `KV GET corpus_version` or `KV GET sessions.abc`. The colon-rewrite fix (user story 10) also removes the original confusion.
- `SKETCH ESTIMATE word_sketch 'wolf'` — engine syntax is `SKETCH COUNT word_sketch 'wolf'`. The new SQL read form (user story 8) also accepts the more discoverable `SELECT FREQ('wolf') FROM word_sketch`.
- `PUSH q_test 'msg'` — engine syntax is `QUEUE PUSH q_test 'msg'`. The new `db.queue` client (user story 12) hides the prefix entirely.
- `INSERT INTO q_test (payload) VALUES ('hello')` — engine rejects table writes to a queue collection; the new `db.queue.push` is the correct surface.

The newly-discovered side effect of triage is that `CREATE GRAPH <name>` and `CREATE COLLECTION <name>` are also rejected today, even though the feedback did not test those forms. They are bundled into user story 6 because the root cause (top-level `CREATE` dispatch table is missing the entries despite the keywords being in the lexer and the parse error's *expected* list) is the same as for `CREATE VECTOR` and `CREATE DOCUMENT`.

The single item that is genuinely fixed since 1.0.8 — single-node `MATCH (n) RETURN n.label` projection — is included here only as a regression-test target (user story 23). The fix itself does not need to land again.
