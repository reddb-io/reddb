# Graphs

RedDB includes a first-class graph engine for nodes, edges, traversals, pathfinding, and analytics. The graph model is fully integrated with the query engine and shares the same storage layer as tables and vectors.

## SQL First

Graph usage in RedDB is not an afterthought. The query engine has graph-native commands, so the shortest explanation is with query examples:

```sql
MATCH (a:person)-[r:REPORTS_TO]->(b:person)
RETURN a.name, b.name, r.since
```

```sql
GRAPH TRAVERSE FROM 'alice' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 3
```

```sql
GRAPH SHORTEST_PATH FROM 'alice' TO 'charlie' ALGORITHM dijkstra
```

```sql
GRAPH CENTRALITY ALGORITHM pagerank
```

```sql
PATH FROM 'web-01' TO 'db-01' ALGORITHM bfs DIRECTION both
```

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

SQL form:

```sql
GRAPH TRAVERSE FROM 'alice' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 3
```

```sql
GRAPH TRAVERSE FROM 'alice' STRATEGY dfs DIRECTION both MAX_DEPTH 2
```

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

SQL form:

```sql
GRAPH SHORTEST_PATH FROM 'alice' TO 'charlie' ALGORITHM dijkstra
```

```sql
PATH FROM 'alice' TO 'charlie' ALGORITHM bfs DIRECTION both
```

## Graph Analytics

RedDB provides built-in graph analytics:

### Centrality

```bash
curl -X POST http://127.0.0.1:8080/graph/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "pagerank"}'
```

Available algorithms: `degree`, `closeness`, `betweenness`, `eigenvector`, `pagerank`.

SQL form:

```sql
GRAPH CENTRALITY ALGORITHM pagerank
```

### Community Detection

```bash
curl -X POST http://127.0.0.1:8080/graph/community \
  -H 'content-type: application/json' \
  -d '{"algorithm": "louvain", "max_iterations": 100}'
```

Algorithms: `louvain`, `label_propagation`.

SQL form:

```sql
GRAPH COMMUNITY ALGORITHM louvain MAX_ITERATIONS 100
```

### Connected Components

```bash
curl -X POST http://127.0.0.1:8080/graph/components \
  -H 'content-type: application/json' \
  -d '{"mode": "weakly_connected"}'
```

SQL form:

```sql
GRAPH COMPONENTS MODE weakly_connected
```

### Cycle Detection

```bash
curl -X POST http://127.0.0.1:8080/graph/cycles \
  -H 'content-type: application/json' \
  -d '{"max_length": 10, "max_cycles": 50}'
```

SQL form:

```sql
GRAPH CYCLES MAX_LENGTH 10 MAX_CYCLES 50
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

More examples:

```sql
MATCH (a:person)-[r:COLLABORATES]->(b:person)
WHERE a.department = 'engineering'
RETURN a.name, b.name, r.weight
```

```sql
MATCH (svc:service)-[r:DEPENDS_ON]->(dep:service)
RETURN svc.name, dep.name
```

You can also inspect graph entities through the universal envelope:

```sql
FROM ANY WHERE _kind = 'node' AND _collection = 'social' LIMIT 20
```

```sql
FROM ANY WHERE _kind = 'edge' AND _collection = 'social' LIMIT 20
```

See [Graph Commands](/query/graph-commands.md) for the full graph query syntax.
