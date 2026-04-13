# Graph Properties

RedDB can compute structural properties of a graph collection through `POST /graph/analytics/properties` or the SQL-like command `GRAPH PROPERTIES`.

This is the quickest way to answer questions like:

- Is the graph connected?
- Is it strongly connected?
- Is it complete?
- Does it contain cycles?
- Is it a tree?
- How dense is it?
- Does it contain self-loops or negative-weight edges?

## HTTP

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/properties \
  -H 'content-type: application/json' \
  -d '{}'
```

You can also apply a projection if you want the analysis over a filtered subgraph.

## SQL

```sql
GRAPH PROPERTIES
```

## Reported Fields

| Field | Meaning |
|:------|:--------|
| `node_count` | Number of nodes in the materialized graph |
| `edge_count` | Number of directed edges |
| `self_loop_count` | Number of edges where source equals target |
| `negative_edge_count` | Number of edges with weight `< 0` |
| `connected_component_count` | Component count when edges are treated as undirected |
| `weak_component_count` | Weakly connected component count |
| `strong_component_count` | Strongly connected component count |
| `is_empty` | `true` when there are no nodes |
| `is_connected` | `true` when the undirected view has a single component |
| `is_weakly_connected` | `true` when the directed graph is weakly connected |
| `is_strongly_connected` | `true` when every node can reach every other node |
| `is_complete` | `true` when every unordered pair of distinct nodes is connected |
| `is_complete_directed` | `true` when every ordered pair of distinct nodes is connected |
| `is_cyclic` | `true` when at least one cycle exists |
| `is_circular` | Alias of `is_cyclic` in the current runtime |
| `is_acyclic` | Logical negation of `is_cyclic` |
| `is_tree` | `true` when the graph is connected and has `|V| - 1` undirected edges |
| `density` | Undirected density over distinct node pairs |
| `density_directed` | Directed density over ordered node pairs |

## Interpreting Results

### Connected vs Weakly Connected vs Strongly Connected

- `is_connected`: treats edges as undirected.
- `is_weakly_connected`: same idea, but reported explicitly for directed-graph terminology.
- `is_strongly_connected`: keeps edge direction and requires mutual reachability.

If you store a graph as directed edges, these three values may differ.

### Complete Graphs

- `is_complete` checks the undirected notion of completeness.
- `is_complete_directed` checks whether every ordered pair has a direct edge.

For a fully bidirectional triangle:

- `is_complete = true`
- `is_complete_directed = true`

For a triangle with only one directed edge per pair:

- `is_complete = true`
- `is_complete_directed = false`

### Cycles

`is_cyclic` becomes `true` as soon as RedDB finds a cycle in the current graph view.

Typical examples:

- dependency loops
- trust loops
- referral cycles
- routing loops

If `is_acyclic = true`, then `GRAPH TOPOLOGICAL_SORT` is a meaningful follow-up for directed acyclic graphs.

### Trees

`is_tree` is useful for hierarchies and spanning structures. In the current implementation it means:

- the graph is connected in the undirected sense
- the undirected edge set has exactly `|V| - 1` distinct pairs

This is the common structural test for a tree.

## Example Response

```json
{
  "node_count": 3,
  "edge_count": 6,
  "self_loop_count": 0,
  "negative_edge_count": 1,
  "connected_component_count": 1,
  "weak_component_count": 1,
  "strong_component_count": 1,
  "is_empty": false,
  "is_connected": true,
  "is_weakly_connected": true,
  "is_strongly_connected": true,
  "is_complete": true,
  "is_complete_directed": true,
  "is_cyclic": true,
  "is_circular": true,
  "is_acyclic": false,
  "is_tree": false,
  "density": 1.0,
  "density_directed": 1.0
}
```

## Related Commands

- [Pathfinding Algorithms](/graph/pathfinding.md)
- [Cycle Detection](/graph/cycles.md)
- [Graph Commands](/query/graph-commands.md)
