# `GRAPH NEIGHBORHOOD` / `GRAPH TRAVERSE`: `EDGES IN (...)` filter + label source [DONE]

Local issue number: #457

GitHub issue: not found in `reddb-io/reddb`.

## Result

Implemented `EDGES IN ('label', ...)` for `GRAPH NEIGHBORHOOD` and
`GRAPH TRAVERSE`, wired through the command AST, parser, and runtime
executor.

The existing graph runtime already supported label-source resolution and
ambiguous-label errors through `resolve_graph_node_id`; this slice pinned
that behavior for `GRAPH NEIGHBORHOOD` as well.

## Verification

- `cargo test -q -p reddb-io-server edges_in --test graph_dsl_parser -- --test-threads=1`
- `cargo test -q -p reddb-io-server edges_in --test runtime_query_behavior -- --test-threads=1`
- `cargo test -q -p reddb-io-server graph_neighborhood_ambiguous_label_errors --test runtime_query_behavior -- --test-threads=1`
- `cargo test -q -p reddb-io-server edges_in --lib -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
