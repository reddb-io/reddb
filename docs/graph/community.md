# Community Detection

Community detection algorithms find groups of densely connected nodes within the graph.

## Louvain

The Louvain algorithm maximizes modularity through iterative optimization:

1. Each node starts in its own community
2. Nodes move to the community that maximizes modularity gain
3. Communities are merged and the process repeats

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/community \
  -H 'content-type: application/json' \
  -d '{
    "algorithm": "louvain",
    "max_iterations": 100
  }'
```

**Complexity**: O(n log n) in practice.

**Use case**: Large-scale social networks, citation networks, biological networks.

## Label Propagation

A fast, near-linear algorithm:

1. Each node starts with a unique label
2. Each node adopts the most frequent label among its neighbors
3. Repeat until convergence

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/community \
  -H 'content-type: application/json' \
  -d '{
    "algorithm": "label_propagation",
    "max_iterations": 50
  }'
```

**Complexity**: O(m) per iteration (m = number of edges).

**Use case**: When speed is more important than optimal community quality.

## Comparison

| Algorithm | Speed | Quality | Deterministic |
|:----------|:------|:--------|:-------------|
| Louvain | Medium | High | No |
| Label Propagation | Fast | Good | No |

## Response Format

```json
{
  "ok": true,
  "communities": [
    {
      "id": 0,
      "members": ["alice", "bob", "charlie"],
      "size": 3
    },
    {
      "id": 1,
      "members": ["diana", "eve", "frank"],
      "size": 3
    }
  ],
  "modularity": 0.42,
  "community_count": 2
}
```

## Connected Components

Related but distinct: find disconnected subgraphs:

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/components \
  -H 'content-type: application/json' \
  -d '{"mode": "weakly_connected"}'
```

Modes: `weakly_connected` (ignores edge direction) and `strongly_connected` (respects direction).
