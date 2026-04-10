# Cycle Detection

Detect cycles (circular paths) in the graph. Useful for dependency analysis, deadlock detection, and data integrity checks.

## Usage

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/cycles \
  -H 'content-type: application/json' \
  -d '{
    "max_length": 10,
    "max_cycles": 50
  }'
```

## Parameters

| Parameter | Default | Description |
|:----------|:--------|:------------|
| `max_length` | `10` | Maximum cycle length to search |
| `max_cycles` | `100` | Maximum number of cycles to return |

## Response

```json
{
  "ok": true,
  "cycles": [
    {
      "nodes": ["alice", "bob", "charlie", "alice"],
      "length": 3,
      "edges": [
        {"from": "alice", "to": "bob", "label": "FOLLOWS"},
        {"from": "bob", "to": "charlie", "label": "FOLLOWS"},
        {"from": "charlie", "to": "alice", "label": "FOLLOWS"}
      ]
    }
  ],
  "cycle_count": 1
}
```

## Use Cases

| Scenario | What Cycles Mean |
|:---------|:----------------|
| Dependency graphs | Circular dependencies (usually a problem) |
| Workflow engines | Potential infinite loops |
| Network topology | Redundant paths (often desirable) |
| Org charts | Reporting loops (data error) |

## Topological Sort

For DAGs (Directed Acyclic Graphs), compute a valid ordering:

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/topological-sort \
  -H 'content-type: application/json' \
  -d '{}'
```

Topological sort fails if the graph contains cycles. Use cycle detection first to verify.

## Clustering Coefficient

Measures how tightly connected a node's neighbors are to each other:

```bash
curl -X POST http://127.0.0.1:8080/graph/analytics/clustering \
  -H 'content-type: application/json' \
  -d '{}'
```

A high clustering coefficient means neighbors tend to also be connected to each other, forming tight clusters.
