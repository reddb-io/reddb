# Pathfinding Algorithms

RedDB supports pathfinding between graph nodes using BFS and Dijkstra algorithms.

## Shortest Path

### BFS (Unweighted)

Finds the shortest path by hop count. Ignores edge weights.

```bash
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "target": "diana",
    "algorithm": "bfs"
  }'
```

Best for: social networks, network topology, any graph where all edges have equal cost.

### Dijkstra (Weighted)

Finds the shortest path by total edge weight. Uses a priority queue for efficient exploration.

```bash
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "target": "diana",
    "algorithm": "dijkstra"
  }'
```

Best for: road networks, latency graphs, cost optimization.

## Traversal

### BFS Traversal

Explore the graph level by level from a starting node:

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

Explore the graph depth-first:

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

## Direction

| Direction | Description |
|:----------|:------------|
| `outgoing` | Follow edges from source to target |
| `incoming` | Follow edges from target to source (reverse) |
| `both` | Follow edges in both directions |

## Neighborhood

Get the immediate neighbors of a node:

```bash
curl -X POST http://127.0.0.1:8080/graph/neighborhood \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "direction": "outgoing"
  }'
```

## Response Format

```json
{
  "ok": true,
  "path": ["alice", "bob", "charlie", "diana"],
  "length": 3,
  "total_weight": 3.5,
  "edges": [
    {"from": "alice", "to": "bob", "label": "FOLLOWS", "weight": 1.0},
    {"from": "bob", "to": "charlie", "label": "FOLLOWS", "weight": 1.5},
    {"from": "charlie", "to": "diana", "label": "REPORTS_TO", "weight": 1.0}
  ]
}
```
