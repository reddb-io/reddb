# Graph Analytics Guide

This guide shows how to use RedDB's graph analytics for network analysis, social graphs, and dependency mapping.

## 1. Build a Social Graph

```bash
# Create people
for name in alice bob charlie diana eve frank; do
  curl -X POST http://127.0.0.1:8080/collections/social/nodes \
    -H 'content-type: application/json' \
    -d "{\"label\": \"$name\", \"node_type\": \"person\"}"
done

# Create relationships
curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 1, "to": 2}'

curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 1, "to": 3}'

curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 2, "to": 3}'

curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 3, "to": 4}'

curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 4, "to": 5}'

curl -X POST http://127.0.0.1:8080/collections/social/edges \
  -H 'content-type: application/json' \
  -d '{"label": "FOLLOWS", "from": 5, "to": 6}'
```

## 2. Find Influential People

```bash
# PageRank
curl -X POST http://127.0.0.1:8080/graph/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "pagerank"}'

# Degree centrality (most connected)
curl -X POST http://127.0.0.1:8080/graph/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "degree"}'
```

## 3. Detect Communities

```bash
curl -X POST http://127.0.0.1:8080/graph/community \
  -H 'content-type: application/json' \
  -d '{"algorithm": "louvain"}'
```

## 4. Find Connections

```bash
# Shortest path between two people
curl -X POST http://127.0.0.1:8080/graph/shortest-path \
  -H 'content-type: application/json' \
  -d '{"source": "alice", "target": "frank", "algorithm": "bfs"}'

# Explore alice's network
curl -X POST http://127.0.0.1:8080/graph/traverse \
  -H 'content-type: application/json' \
  -d '{"source": "alice", "strategy": "bfs", "max_depth": 3}'
```

## 5. Check for Cycles

```bash
curl -X POST http://127.0.0.1:8080/graph/cycles \
  -H 'content-type: application/json' \
  -d '{"max_length": 5}'
```

## 6. Bridge Detection

Find nodes that bridge different communities:

```bash
curl -X POST http://127.0.0.1:8080/graph/centrality \
  -H 'content-type: application/json' \
  -d '{"algorithm": "betweenness"}'
```

Nodes with high betweenness centrality are bridges between communities.

## Analytics Pipeline

For a complete analytics pipeline:

1. **Build graph**: Insert nodes and edges
2. **Detect communities**: Run Louvain
3. **Rank importance**: Run PageRank
4. **Find bridges**: Run betweenness centrality
5. **Identify clusters**: Run clustering coefficient
6. **Export results**: Use snapshots and exports
