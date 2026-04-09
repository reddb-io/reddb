# Multi-Mode Queries (Gremlin, SPARQL, Natural Language)

RedDB supports multiple query languages beyond SQL. The query engine automatically detects the mode or you can specify it explicitly.

## Query Modes

| Mode | Syntax Prefix | Description |
|:-----|:-------------|:------------|
| SQL | `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `FROM`, `CREATE` | Standard SQL-like syntax |
| Gremlin | `g.V()`, `g.E()` | Apache TinkerPop graph traversal |
| SPARQL | `SELECT ... WHERE { ... }` with triple patterns | W3C semantic query language |
| Natural | Free-form English text | Natural language query processing |

## Gremlin

Use Gremlin-style traversals for graph queries:

```
g.V().hasLabel('person').out('FOLLOWS').values('name')
```

```
g.V('alice').outE('REPORTS_TO').inV().values('name')
```

### Examples

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "g.V().hasLabel('\''person'\'').out('\''FOLLOWS'\'').values('\''name'\'')"}'
```

### Supported Steps

| Step | Description |
|:-----|:------------|
| `g.V()` | All vertices |
| `g.E()` | All edges |
| `.hasLabel(label)` | Filter by label |
| `.has(key, value)` | Filter by property |
| `.out(label)` | Outgoing traversal |
| `.in(label)` | Incoming traversal |
| `.both(label)` | Both directions |
| `.outE(label)` | Outgoing edges |
| `.inE(label)` | Incoming edges |
| `.inV()` | Edge target vertex |
| `.outV()` | Edge source vertex |
| `.values(key)` | Extract property values |
| `.count()` | Count results |
| `.limit(n)` | Limit results |

## SPARQL

Use SPARQL triple patterns for semantic queries:

```sparql
SELECT ?name ?dept WHERE {
  ?person rdf:type :Person .
  ?person :name ?name .
  ?person :department ?dept .
  ?person :reports_to ?manager .
  ?manager :name "Alice" .
}
```

### Example

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT ?name WHERE { ?p rdf:type :Person . ?p :name ?name . }"}'
```

## Natural Language

Write queries in plain English:

```
show me all hosts with critical alerts in the last 24 hours
```

```
find the shortest path between alice and charlie
```

```
which users have the most connections
```

### Example

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query": "find all critical hosts running linux"}'
```

## Mode Detection

The query engine detects the mode automatically:

| Input Pattern | Detected Mode |
|:-------------|:-------------|
| Starts with `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `FROM`, `CREATE`, `DROP`, `ALTER` | SQL |
| Starts with `g.V()` or `g.E()` | Gremlin |
| Contains `WHERE { ... }` with triple patterns | SPARQL |
| Starts with `MATCH`, `PATH`, `GRAPH`, `SEARCH` | SQL (extended) |
| Free-form text | Natural Language |

## Response Format

All modes return the same unified envelope:

```json
{
  "ok": true,
  "mode": "gremlin",
  "statement": "g.V().hasLabel('person').out('FOLLOWS').values('name')",
  "engine": "graph",
  "columns": ["name"],
  "record_count": 3,
  "records": [...]
}
```

The `mode` field tells you which parser was used.

> [!NOTE]
> Natural language queries are best-effort. For production workloads, use explicit SQL, Gremlin, or SPARQL syntax for deterministic results.
