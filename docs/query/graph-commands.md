# Graph Commands

RedDB supports graph-specific commands within the query engine for traversals, pathfinding, and analytics.

## MATCH (Graph Pattern)

Query the graph using pattern matching:

```sql
MATCH (a:person)-[r:REPORTS_TO]->(b:person) RETURN a.name, b.name, r.since
```

### Syntax

```sql
MATCH (node_alias[:label])-[edge_alias[:label]]->(node_alias[:label])
[WHERE condition]
RETURN expressions
```

### Examples

```sql
-- Find all relationships from a node
MATCH (a:alice)-[r]->(b) RETURN b.label, r.label

-- Find incoming edges
MATCH (a)<-[r:FOLLOWS]-(b:person) WHERE a.label = 'bob' RETURN b.name

-- Multi-hop pattern
MATCH (a:person)-[:WORKS_AT]->(c:company)-[:LOCATED_IN]->(city)
RETURN a.name, c.name, city.name
```

## GRAPH NEIGHBORHOOD

Expand the immediate neighborhood of a node:

```sql
GRAPH NEIGHBORHOOD 'alice' DIRECTION outgoing DEPTH 2
```

## GRAPH TRAVERSE

Run BFS or DFS traversal from a starting node:

```sql
GRAPH TRAVERSE FROM 'alice' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 3
```

Parameters:
- `STRATEGY`: `bfs` or `dfs`
- `DIRECTION`: `outgoing`, `incoming`, or `both`
- `MAX_DEPTH`: maximum traversal depth

## GRAPH SHORTEST_PATH

Find the shortest path between two nodes:

```sql
GRAPH SHORTEST_PATH FROM 'alice' TO 'charlie' ALGORITHM dijkstra
GRAPH SHORTEST_PATH FROM 'alice' TO 'charlie' ORDER BY hop_count ASC LIMIT 1
```

Algorithms:
- `bfs`: shortest path by hop count
- `dijkstra`: weighted shortest path (non-negative weights)
- `astar`: heuristic-guided search; the generic runtime currently uses a null heuristic
- `bellman_ford`: supports negative weights and detects negative cycles

Ordering metrics: `hop_count`, `total_weight`, `nodes_visited`.

## GRAPH CENTRALITY

Compute centrality scores:

```sql
GRAPH CENTRALITY ALGORITHM pagerank
GRAPH CENTRALITY ALGORITHM pagerank ORDER BY centrality_score DESC LIMIT 10
```

Algorithms: `degree`, `closeness`, `betweenness`, `eigenvector`, `pagerank`.

`GRAPH CENTRALITY` keeps the historical implicit top-100 cap when `LIMIT`
is omitted. Use `LIMIT N` to choose a different cap, or `LIMIT 0` to return
no rows. Ordering metrics: `score`, `centrality_score`.

## GRAPH COMMUNITY

Detect communities in the graph:

```sql
GRAPH COMMUNITY ALGORITHM louvain MAX_ITERATIONS 100
GRAPH COMMUNITY ALGORITHM louvain ORDER BY size DESC LIMIT 5
```

Algorithms: `louvain`, `label_propagation`.

Ordering metrics: `size`, `community_size`.

## GRAPH COMPONENTS

Find connected components:

```sql
GRAPH COMPONENTS MODE weakly_connected
GRAPH COMPONENTS MODE weakly_connected ORDER BY component_size DESC LIMIT 20
```

Modes: `weakly_connected`, `strongly_connected`.

Ordering metrics: `size`, `component_size`.

## GRAPH CYCLES

Detect cycles up to a maximum length (default `10` when omitted):

```sql
GRAPH CYCLES MAX_LENGTH 10
```

> [!NOTE]
> `MAX_CYCLES n` is **not** parsed today. Track [#465](https://github.com/reddb-io/reddb/issues/465)
> for the result-cap form.

## GRAPH CLUSTERING

Compute the clustering coefficient:

```sql
GRAPH CLUSTERING
```

> [!NOTE]
> `GRAPH HITS` (hub/authority scoring) is **not** parsed today. Use
> `GRAPH CENTRALITY ALGORITHM pagerank` for influence ranking until a HITS
> grammar is wired up.

## GRAPH TOPOLOGICAL_SORT

Compute topological ordering of a DAG:

```sql
GRAPH TOPOLOGICAL_SORT
```

## GRAPH PROPERTIES

Compute structural graph properties:

```sql
GRAPH PROPERTIES
```

Returns a summary with connectivity, completeness, cyclicity, density, and component counts.

## PATH Query

Dedicated path queries between two nodes:

```sql
PATH FROM alice TO charlie ALGORITHM dijkstra DIRECTION both
```

### Node Selectors

Nodes can be selected by label:

```sql
PATH FROM 'web-server-01' TO 'db-primary' ALGORITHM bfs
```

## Query Flow

```mermaid
flowchart LR
    A[Graph Command] --> B{Command Type}
    B -->|MATCH| C[Pattern Match Engine]
    B -->|TRAVERSE| D[BFS/DFS Engine]
    B -->|SHORTEST_PATH| E[Dijkstra/A*/Bellman-Ford/BFS]
    B -->|CENTRALITY| F[Analytics Engine]
    B -->|COMMUNITY| F
    B -->|PROPERTIES| F
    C --> G[Result Set]
    D --> G
    E --> G
    F --> G
```
