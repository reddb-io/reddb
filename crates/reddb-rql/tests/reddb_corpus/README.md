# RedDB-only conformance corpus (no external oracle)

Each `*.slt` file here is a [sqllogictest][slt]-format script covering a
**RedDB-only query surface** — one with no standard-SQL counterpart and no
external oracle: vector search, the native `GRAPH` command family, the four
graph DSL modes (Gremlin / Cypher / SPARQL / Path), natural-language, and
vector extensions. The harness (`tests/reddb_conformance.rs`) discovers every
`.slt` file here automatically and runs it end-to-end against the in-server
engine (`reddb-server`'s `RedDBRuntime::in_memory()`), one fresh runtime per
file so state never leaks between scripts.

This is the S3 companion to the standard-SQL slice in `../corpus/`, whose truth
is the public SQLite oracle (ADR 0053).

## Truth is hand-authored — engine output is the thing under test

These surfaces are RedDB inventions, so no third-party engine can adjudicate
them. Every expected value in a `query` block is authored from the **semantics**
of the surface — cosine ranking, neighbourhood reachability, shortest-path hop
counts, downstream PageRank flow — and never copied from whatever the engine
happens to emit. The fixtures are kept deliberately small and tie-free so the
correct answer is unambiguous (e.g. a vector fixture whose cosine similarities
are 1.00 / 0.60 / 0.00 gives one strict ranking).

## Volatile columns are projected away, not pinned

The engine decorates each vector/graph result row with engine-internal,
run-varying columns: auto-assigned entity ids (`entity_id`, `node_id`,
`red_entity_id`), wall-clock stamps (`created_at`, `updated_at`), sequence
numbers, `red_*` capability metadata, and raw distance/score floats whose exact
value is engine-defined rather than oracle-defined. The harness projects every
result down to a fixed allowlist of **semantic, deterministic** columns
(`label`, `name`, `content`, `depth`, `path_found`, `hop_count`,
`total_weight`, `nodes_visited`, `negative_cycle_detected`) in a canonical
order. A golden therefore asserts the *meaning* of a result and stays silent
about engine bookkeeping, so it never freezes non-determinism.

## Characterization is clearly marked regression-only

`characterization_unprojected_surfaces.slt` covers surfaces the executor
currently **accepts but does not project** (the four graph DSL modes,
natural-language, and the `SEARCH SIMILAR … COLLECTION` form, which return an
empty semantic column set today). Those have no hand-authorable result yet, so
they are pinned with `statement ok` — asserting only that today's engine
accepts the query. Per ADR 0053 this is a **cheap regression layer, never
promoted to conformance truth**: the file is loudly marked, and each surface is
upgraded to a real `query` golden the moment the executor begins projecting
semantic columns for it.

## Files

- `vector_search.slt` — TRUTH: cosine nearest-first ranking, `LIMIT`,
  `THRESHOLD`, and the `inner_product` metric extension.
- `graph_commands.slt` — TRUTH: `GRAPH NEIGHBORHOOD` / `TRAVERSE` /
  `PROPERTIES` / `SHORTEST_PATH` / `CENTRALITY` over a directed chain.
- `characterization_unprojected_surfaces.slt` — REGRESSION-ONLY: Gremlin,
  Cypher, SPARQL, Path, natural-language, and the `SEARCH SIMILAR … COLLECTION`
  form, each pinned with `statement ok`.

[slt]: https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki
