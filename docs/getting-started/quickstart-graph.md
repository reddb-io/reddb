# Quickstart: Graphs

Model entities and their relationships, then traverse them. The **graph**
model is a semantic layer over a `collection` (the universal container): nodes
and edges live in one `network` collection, and RedDB runs pathfinding over
them.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Insert nodes

The first user-inserted item in a fresh database gets `rid = 1024` (ids
`1..1023` are reserved for internal bootstrap records), so these three nodes
are `1024`, `1025`, and `1026`:

```sql
INSERT INTO network NODE (label, node_type, role) VALUES ('gateway', 'Host', 'gateway');
INSERT INTO network NODE (label, node_type, role) VALUES ('app', 'Host', 'application');
INSERT INTO network NODE (label, node_type, role) VALUES ('db', 'Host', 'database');
```

## 3. Connect them with weighted edges

```sql
INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1025, 1.0);
INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1025, 1026, 1.0);
INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1026, 5.0);
```

## 4. Your first meaningful result

Find the cheapest path from the gateway to the database. Dijkstra prefers the
two 1.0-weight hops over the single 5.0 edge:

```sql
GRAPH SHORTEST_PATH '1024' TO '1026' ALGORITHM dijkstra;
```

```text
 hop_count | total_weight
-----------+-------------
 2         | 2.0
```

## Where to go next

- [Graphs](/data-models/graphs.md) — nodes, edges, and properties
- [Graph commands](/query/graph-commands.md) — traversals and analytics
