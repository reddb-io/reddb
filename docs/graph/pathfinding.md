# Pathfinding Algorithms

RedDB supports shortest-path and traversal queries over graph collections through HTTP, SQL-like graph commands, and the embedded runtime.

## Shortest Path

Use `POST /graph/shortest-path` or `GRAPH SHORTEST_PATH` when you need the minimum path between two nodes.

HTTP example:

```bash
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "target": "diana",
    "algorithm": "dijkstra",
    "direction": "outgoing"
  }'
```

SQL form:

```sql
GRAPH SHORTEST_PATH FROM 'alice' TO 'diana' ALGORITHM dijkstra
```

### Algorithms

| Algorithm | Uses Weights | Negative Weights | Typical Use |
|:----------|:-------------|:-----------------|:------------|
| `bfs` | No | No | Minimum hop count |
| `dijkstra` | Yes | No | Weighted shortest path with non-negative costs |
| `astar` | Yes | No | Heuristic-guided weighted shortest path |
| `bellman_ford` | Yes | Yes | Weighted shortest path with negative edges |

### When to Use Each One

- `bfs`: when every edge should count the same and you only care about hop count.
- `dijkstra`: when edge weights represent cost, latency, trust distance, or any non-negative metric.
- `astar`: when you want the `A*` interface and have a domain where heuristics can help. In the current runtime, the generic public path uses a neutral heuristic, so it is correct but may behave similarly to Dijkstra unless a more specific heuristic is introduced.
- `bellman_ford`: when some edges can be negative and you still want the minimum path. The response includes `negative_cycle_detected`.

## Negative Edge Handling

`bellman_ford` is the correct choice when a graph can contain negative edges.

Example:

```bash
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{
    "source": "A",
    "target": "D",
    "algorithm": "bellman_ford"
  }'
```

If the runtime detects a negative cycle reachable from the source in the evaluated traversal space, the response sets `negative_cycle_detected` and does not report a stable shortest path.

## Direction

| Direction | Description |
|:----------|:------------|
| `outgoing` | Follow edges from source to target |
| `incoming` | Follow edges in reverse |
| `both` | Treat incoming and outgoing edges as traversable |

This applies both to shortest-path queries and to traversal queries.

## Traversal

Use `POST /graph/traverse` to explore the graph from a seed node.

### BFS Traversal

```bash
curl -X POST http://127.0.0.1:8080/graph/traverse \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "strategy": "bfs",
    "direction": "outgoing",
    "max_depth": 3
  }'
```

### DFS Traversal

```bash
curl -X POST http://127.0.0.1:8080/graph/traverse \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "strategy": "dfs",
    "direction": "both",
    "max_depth": 5
  }'
```

## Neighborhood

Use `POST /graph/neighborhood` when you want local expansion rather than a full traversal:

```bash
curl -X POST http://127.0.0.1:8080/graph/neighborhood \
  -H 'content-type: application/json' \
  -d '{
    "node": "alice",
    "direction": "both",
    "max_depth": 1
  }'
```

## Result Shape

Shortest-path responses include:

- `source`
- `target`
- `algorithm`
- `direction`
- `nodes_visited`
- `negative_cycle_detected`
- `path`

The `path` object includes:

- `hop_count`
- `total_weight`
- ordered `nodes`
- ordered `edges`

Example:

```json
{
  "source": "alice",
  "target": "diana",
  "algorithm": "bellman_ford",
  "direction": "outgoing",
  "nodes_visited": 7,
  "negative_cycle_detected": false,
  "path": {
    "hop_count": 3,
    "total_weight": 1.5,
    "nodes": [
      {"id": "alice", "label": "alice", "node_type": "person", "out_edge_count": 2, "in_edge_count": 1},
      {"id": "bob", "label": "bob", "node_type": "person", "out_edge_count": 1, "in_edge_count": 2},
      {"id": "charlie", "label": "charlie", "node_type": "person", "out_edge_count": 1, "in_edge_count": 1},
      {"id": "diana", "label": "diana", "node_type": "person", "out_edge_count": 0, "in_edge_count": 1}
    ],
    "edges": [
      {"source": "alice", "target": "bob", "edge_type": "follows", "weight": 1.0},
      {"source": "bob", "target": "charlie", "edge_type": "follows", "weight": -0.5},
      {"source": "charlie", "target": "diana", "edge_type": "reports_to", "weight": 1.0}
    ]
  }
}
```
