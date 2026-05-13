# LIMIT and ORDER BY on GRAPH <algorithm> commands [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/422

Labels: enhancement

GitHub issue number: #422

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

`LIMIT N` and `ORDER BY <metric> [ASC|DESC]` support on every `GRAPH <algorithm>` command:

```sql
GRAPH CENTRALITY tales LIMIT 10
GRAPH COMMUNITY tales ORDER BY size DESC LIMIT 5
GRAPH COMPONENTS tales LIMIT 20
GRAPH SHORTEST_PATH '<a>' TO '<b>' LIMIT 100
```

Today: parse error. `GRAPH CENTRALITY` returns implicit top-100 with no way to control.

## Acceptance criteria

- [x] `LIMIT N` parses and applies to every documented `GRAPH <algorithm>` clause.
- [x] `ORDER BY` with the algorithm's natural metric works (e.g. centrality_score, component_size).
- [x] Default top-K is documented; removed implicit truncation surfaces correctly.
- [x] Tests for limit cap, order direction, and combined `ORDER BY ... LIMIT`.

## Progress

Slice 1 (commit 7315cfb5): `GRAPH CENTRALITY LIMIT N` landed.

- Parser accepts `GRAPH CENTRALITY LIMIT n` and `GRAPH CENTRALITY ALGORITHM pagerank LIMIT n`.
- Runtime applies the cap to centrality output while preserving the historical implicit top-100 cap when `LIMIT` is omitted.
- `LIMIT 0` returns zero rows; negative limits fail through the existing integer parser.
- Tests landed in parser coverage and `tests/runtime_query_behavior.rs`.

Verification:

- `cargo test -p reddb-io-server --lib test_parse_graph_centrality`
- `cargo test -p reddb-io-server --test runtime_query_behavior graph_centrality_limit`
- `make check`

Slice 2: `GRAPH COMPONENTS LIMIT N` landed.

- Parser accepts `GRAPH COMPONENTS LIMIT n` and `GRAPH COMPONENTS MODE weak LIMIT n`.
- Runtime applies the cap to emitted component rows; omitted `LIMIT` preserves existing unbounded component output.
- Tests landed in parser coverage and `crates/reddb-server/tests/runtime_query_behavior.rs`.

Verification:

- `cargo test -p reddb-io-server --test graph_dsl_parser graph_components_limit_parses`
- `cargo test -p reddb-io-server --test runtime_query_behavior graph_components_limit_caps_returned_rows`

Slice 3 complete:

- Parser accepts `ORDER BY <metric> [ASC|DESC]` and `LIMIT N` for
  `GRAPH CENTRALITY`, `GRAPH COMMUNITY`, `GRAPH COMPONENTS`, and
  `GRAPH SHORTEST_PATH`.
- Runtime applies order+limit after producing graph command rows. Supported
  natural metrics are `score`/`centrality_score`, `size`/`community_size`,
  `size`/`component_size`, and shortest-path `hop_count`, `total_weight`,
  `nodes_visited`.
- `GRAPH CENTRALITY` keeps the historical implicit top-100 when no explicit
  limit is supplied.
- `docs/query/graph-commands.md` documents the default cap and examples for
  combined `ORDER BY ... LIMIT`.
- Added focused parser/runtime tests.

Final verification:

- `cargo test -p reddb-io-server --test graph_dsl_parser`
- `cargo test -p reddb-io-server --test runtime_query_behavior graph_`
- `cargo check -p reddb-io-server -p reddb-io-client -p reddb-io-client-connector`
