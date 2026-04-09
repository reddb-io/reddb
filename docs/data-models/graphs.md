# Graphs

RedDB includes a first-class graph engine for nodes, edges, traversals, pathfinding, and analytics. The graph model is fully integrated with the query engine and shares the same storage layer as tables and vectors.

## Creating Nodes

<!-- tabs:start -->

#### **HTTP**

```bash
curl -X POST http://127.0.0.1:8080/collections/social/nodes \
  -H 'content-type: application/json' \
  -d '{
    "label": "alice",
    "node_type": "person",
    "properties": {
      "name": "Alice Johnson",
      "department": "engineering",
      "level": "senior"
    }
  }'
```

#### **gRPC**

```bash
grpcurl -plaintext \
  -d '{
    "collection": "social",
    "payloadJson": "{\"label\":\"alice\",\"node_type\":\"person\",\"properties\":{\"name\":\"Alice Johnson\"}}"
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateNode
```

#### **Rust (Embedded)**

```rust
let node_id = db.node("social", "alice")
    .node_type("person")
    .property("name", "Alice Johnson")
    .property("department", "engineering")
    .save()?;
```

<!-- tabs:end -->

## Creating Edges

Edges connect two nodes with a labeled, optionally weighted relationship:

```bash
curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{
    "label": "REPORTS_TO",
    "from": 1,
    "to": 2,
    "weight": 1.0,
    "properties": {
      "since": "2023-06-01"
    }
  }'
```

## Edge Properties

| Field | Required | Description |
|:------|:---------|:------------|
| `label` | Yes | Relationship type (e.g. `REPORTS_TO`, `FOLLOWS`) |
| `from` | Yes | Source node entity ID |
| `to` | Yes | Target node entity ID |
| `weight` | No | Numeric weight (default `1.0`) |
| `properties` | No | Arbitrary key-value properties |
| `metadata` | No | Operational metadata |

## Graph Traversal

Traverse the graph from a starting node:

```bash
curl -X POST http://127.0.0.1:8080/graph/traverse \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "direction": "outgoing",
    "max_depth": 3,
    "strategy": "bfs"
  }'
```

| Parameter | Options | Default |
|:----------|:--------|:--------|
| `direction` | `outgoing`, `incoming`, `both` | `outgoing` |
| `strategy` | `bfs`, `dfs` | `bfs` |
| `max_depth` | any positive integer | `3` |

## Shortest Path

Find the shortest path between two nodes:

```bash
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{
    "source": "alice",
    "target": "charlie",
    "algorithm": "dijkstra"
  }'
```

Algorithms: `bfs` (unweighted) or `dijkstra` (weighted).

## Graph Analytics

RedDB provides built-in graph analytics:

### Centrality

```bash
curl -X POST http://127.0.0.1:8080/graph/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "pagerank"}'
```

Available algorithms: `degree`, `closeness`, `betweenness`, `eigenvector`, `pagerank`.

### Community Detection

```bash
curl -X POST http://127.0.0.1:8080/graph/community \
  -H 'content-type: application/json' \
  -d '{"algorithm": "louvain", "max_iterations": 100}'
```

Algorithms: `louvain`, `label_propagation`.

### Connected Components

```bash
curl -X POST http://127.0.0.1:8080/graph/components \
  -H 'content-type: application/json' \
  -d '{"mode": "weakly_connected"}'
```

### Cycle Detection

```bash
curl -X POST http://127.0.0.1:8080/graph/cycles \
  -H 'content-type: application/json' \
  -d '{"max_length": 10, "max_cycles": 50}'
```

### Additional Analytics

| Endpoint | Description |
|:---------|:------------|
| `POST /graph/clustering` | Clustering coefficient |
| `POST /graph/hits` | HITS (hubs and authorities) |
| `POST /graph/topological-sort` | Topological ordering |
| `POST /graph/personalized-pagerank` | Personalized PageRank from a source node |

## Graph Projections

Create named projections that filter nodes and edges for analytics:

```bash
grpcurl -plaintext \
  -d '{
    "name": "engineering-team",
    "source": "social",
    "node_labels": ["alice", "bob", "charlie"],
    "node_types": ["person"],
    "edge_labels": ["REPORTS_TO", "COLLABORATES"]
  }' \
  127.0.0.1:50051 reddb.v1.RedDb/SaveGraphProjection
```

## Graph Query via SQL

Use `MATCH` syntax in the query engine:

```sql
MATCH (a:person)-[r:REPORTS_TO]->(b:person) RETURN a.name, b.name, r.since
```

See [Graph Commands](/query/graph-commands.md) for the full graph query syntax.
