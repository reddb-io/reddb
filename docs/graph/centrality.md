# Centrality

Centrality algorithms measure the importance of nodes in a graph. RedDB supports five centrality measures.

## Default row cap

The SQL form `GRAPH CENTRALITY` applies an **implicit top-100 cap** when no
`LIMIT` is supplied — it returns only the 100 highest-scoring nodes, silently,
with no `truncated` flag. This is the historical default (`top_k = 100`).

This bites when centrality is used to resolve every node, for example to build
a `label → node_id` map for the full graph: on a graph with thousands of nodes
the response only carries 100 rows, so downstream lookups report results like
`unresolved N node ids` and it looks like a data bug when the engine is
behaving as designed.

To page through more (or all) nodes, pass an explicit `LIMIT`:

```sql
-- Default: top 100 nodes only
GRAPH CENTRALITY ALGORITHM pagerank

-- Full graph: cap at least as large as the node count
GRAPH CENTRALITY ALGORITHM pagerank LIMIT 100000

-- Metadata only (no rows)
GRAPH CENTRALITY ALGORITHM pagerank LIMIT 0
```

The HTTP form (`POST /graph/analytics/centrality`) has its own cap: the
`top_k` body field, which defaults to **25** when omitted. Raise it the same
way — pass `{"algorithm": "pagerank", "top_k": 100000}` for full-graph
resolution.

## Degree Centrality

Counts the number of connections for each node.

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "degree"}'
```

**Use case**: Find the most connected nodes (hubs).

## Closeness Centrality

Measures how close a node is to all other nodes (inverse of average shortest path).

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "closeness"}'
```

**Use case**: Find nodes that can reach others most efficiently.

## Betweenness Centrality

Measures how often a node lies on the shortest path between other pairs of nodes.

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "betweenness"}'
```

**Use case**: Find bridge nodes that control information flow.

## Eigenvector Centrality

Measures influence based on the importance of a node's neighbors (a node is important if its neighbors are important).

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "eigenvector"}'
```

**Use case**: Find influential nodes in social networks.

## PageRank

Google's PageRank algorithm. Similar to eigenvector centrality but with damping factor.

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "pagerank"}'
```

**Use case**: Rank nodes by importance considering link structure.

### Personalized PageRank

Run PageRank biased toward a specific source node:

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/pagerank/personalized \
  -H 'content-type: application/json' \
  -d '{"source": "alice"}'
```

## HITS (Hubs and Authorities)

Computes hub and authority scores:

```bash
curl -X POST http://127.0.0.1:5000/graph/analytics/hits \
  -H 'content-type: application/json' \
  -d '{}'
```

- **Hubs**: Nodes that point to many good authorities
- **Authorities**: Nodes pointed to by many good hubs

## Comparison

| Algorithm | Complexity | Best For |
|:----------|:-----------|:---------|
| Degree | O(n) | Quick connectivity check |
| Closeness | O(n^2) | Reachability analysis |
| Betweenness | O(n*m) | Bridge/bottleneck detection |
| Eigenvector | O(k*m) | Influence propagation |
| PageRank | O(k*m) | Link-based ranking |
| HITS | O(k*m) | Hub/authority classification |
