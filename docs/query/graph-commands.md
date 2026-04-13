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
```

Algorithms:
- `bfs`: menor caminho por número de saltos
- `dijkstra`: menor caminho ponderado com pesos não-negativos
- `astar`: busca guiada por heurística; atualmente usa heurística nula no runtime genérico
- `bellman_ford`: suporta pesos negativos e detecta ciclos negativos

## GRAPH CENTRALITY

Compute centrality scores:

```sql
GRAPH CENTRALITY ALGORITHM pagerank
```

Algorithms: `degree`, `closeness`, `betweenness`, `eigenvector`, `pagerank`.

## GRAPH COMMUNITY

Detect communities in the graph:

```sql
GRAPH COMMUNITY ALGORITHM louvain MAX_ITERATIONS 100
```

Algorithms: `louvain`, `label_propagation`.

## GRAPH COMPONENTS

Find connected components:

```sql
GRAPH COMPONENTS MODE weakly_connected
```

Modes: `weakly_connected`, `strongly_connected`.

## GRAPH CYCLES

Detect cycles:

```sql
GRAPH CYCLES MAX_LENGTH 10 MAX_CYCLES 50
```

## GRAPH CLUSTERING

Compute the clustering coefficient:

```sql
GRAPH CLUSTERING
```

## GRAPH HITS

Compute hub and authority scores (HITS algorithm):

```sql
GRAPH HITS
```

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
