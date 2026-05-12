# GRAPH PROPERTIES '<id-or-label>' per-node property lookup [DONE]

GitHub: https://github.com/reddb-io/reddb/issues/423

Labels: enhancement

GitHub issue number: #423

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement

## What to build

`GRAPH PROPERTIES '<id-or-label>'` returns the full property bag of a specific node:

```sql
GRAPH PROPERTIES '177'
GRAPH PROPERTIES 'cinderella'
```

Today: parse error. Only the bare `GRAPH PROPERTIES` (no argument) works, returning graph-wide stats.

## Acceptance criteria

- [x] `GRAPH PROPERTIES '<id>'` returns all properties of the node.
- [x] `GRAPH PROPERTIES '<label>'` resolves via the same label index as `GRAPH NEIGHBORHOOD`.
- [x] Clear error when id/label does not exist.
- [x] Tests covering id form, label form, missing-id, ambiguous-label.

## Completion note

Implemented in `main`; GitHub issue #423 is closed. Regression coverage lives
in `crates/reddb-server/tests/runtime_query_behavior.rs`:

- `graph_properties_no_arg_returns_graph_wide_stats`
- `graph_properties_by_label_returns_property_bag`
- `graph_properties_by_numeric_id_returns_property_bag`
- `graph_properties_missing_label_errors`
- `graph_properties_ambiguous_label_errors`
