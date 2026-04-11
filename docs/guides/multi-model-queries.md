# Multi-Model Queries — Tables + Graphs + Vectors in One Query

RedDB stores tables, graphs, and vectors in the same database. This guide walks through a realistic scenario where you use all three together and query across them in a single command.

## Scenario

You are building an employee directory for a small company. The data lives in three structures:

| Structure | What it holds |
|:----------|:--------------|
| **Table rows** | Employee records (name, email, department, title) |
| **Graph edges** | Reporting relationships (`REPORTS_TO`) |
| **Vectors** | Skill embeddings auto-generated from job descriptions |

By the end of this guide you will insert employees as rows, link them with graph edges, embed their skills as vectors, and run cross-structure queries that combine all three.

## Step 1: Insert Employees as Rows

Create a `company` collection and add six employees:

```bash
curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Alice Chen","email":"alice@co.com","department":"Engineering","title":"VP of Engineering"}}'

curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Bob Park","email":"bob@co.com","department":"Engineering","title":"Senior ML Engineer"}}'

curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Carol Reyes","email":"carol@co.com","department":"Engineering","title":"Backend Engineer"}}'

curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Dan Okafor","email":"dan@co.com","department":"Product","title":"Product Manager"}}'

curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Eva Lund","email":"eva@co.com","department":"Engineering","title":"ML Engineer"}}'

curl -X POST http://127.0.0.1:8080/collections/company/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Frank Yao","email":"frank@co.com","department":"Engineering","title":"Staff Engineer"}}'
```

Each call returns the new entity ID. The IDs used below assume sequential assignment starting at `1`.

## Step 2: Create Graph Relationships

Add `REPORTS_TO` edges so the org chart looks like this:

```
Alice (VP)
├── Bob (Senior ML Engineer)
│   └── Eva (ML Engineer)
├── Carol (Backend Engineer)
└── Frank (Staff Engineer)

Dan (Product Manager) ── reports to Alice
```

```bash
# Bob reports to Alice
curl -X POST http://127.0.0.1:8080/collections/company/edges \
  -H 'content-type: application/json' \
  -d '{"label":"REPORTS_TO","from":"2","to":"1","properties":{"since":"2024-01"}}'

# Carol reports to Alice
curl -X POST http://127.0.0.1:8080/collections/company/edges \
  -H 'content-type: application/json' \
  -d '{"label":"REPORTS_TO","from":"3","to":"1","properties":{"since":"2024-03"}}'

# Dan reports to Alice
curl -X POST http://127.0.0.1:8080/collections/company/edges \
  -H 'content-type: application/json' \
  -d '{"label":"REPORTS_TO","from":"4","to":"1","properties":{"since":"2024-06"}}'

# Eva reports to Bob
curl -X POST http://127.0.0.1:8080/collections/company/edges \
  -H 'content-type: application/json' \
  -d '{"label":"REPORTS_TO","from":"5","to":"2","properties":{"since":"2024-09"}}'

# Frank reports to Alice
curl -X POST http://127.0.0.1:8080/collections/company/edges \
  -H 'content-type: application/json' \
  -d '{"label":"REPORTS_TO","from":"6","to":"1","properties":{"since":"2024-02"}}'
```

## Step 3: Auto-Embed Job Descriptions

Create a `skills` collection and insert skill descriptions with automatic embedding. RedDB generates the vector from the text using the configured AI provider.

```sql
INSERT INTO skills (employee_id, description) VALUES (1, 'System architecture, distributed systems, team leadership')
  WITH AUTO EMBED (description)

INSERT INTO skills (employee_id, description) VALUES (2, 'Machine learning, deep learning, PyTorch, model training, MLOps')
  WITH AUTO EMBED (description)

INSERT INTO skills (employee_id, description) VALUES (3, 'Rust, Go, API design, microservices, PostgreSQL')
  WITH AUTO EMBED (description)

INSERT INTO skills (employee_id, description) VALUES (4, 'Product strategy, roadmapping, user research, agile')
  WITH AUTO EMBED (description)

INSERT INTO skills (employee_id, description) VALUES (5, 'Machine learning, TensorFlow, computer vision, data pipelines')
  WITH AUTO EMBED (description)

INSERT INTO skills (employee_id, description) VALUES (6, 'System design, Kubernetes, CI/CD, infrastructure as code')
  WITH AUTO EMBED (description)
```

> [!TIP]
> `WITH AUTO EMBED` requires a configured AI provider. Set one up before running these inserts:
> ```bash
> curl -X POST http://127.0.0.1:8080/ai/credentials \
>   -d '{"provider":"groq","api_key":"gsk_xxx","default":true}'
> ```

## Step 4: Cross-Structure Queries

Now that you have rows, edges, and vectors in the same database, you can query across all of them.

### Find everyone in Engineering (table query)

```sql
SELECT * FROM company WHERE department = 'Engineering'
```

This returns Alice, Bob, Carol, Eva, and Frank as table rows.

### Find who reports to Alice (graph traversal)

```sql
GRAPH NEIGHBORHOOD '1' DEPTH 2 DIRECTION incoming
```

Returns Bob, Carol, Dan, Frank (depth 1) and Eva (depth 2, through Bob).

### Find employees with ML skills (semantic search)

```sql
SEARCH SIMILAR TEXT 'machine learning expertise' COLLECTION skills LIMIT 5
```

RedDB embeds the query text automatically and returns the closest skill descriptions. Expect Bob and Eva to rank highest.

### Find everything about Alice across all structures

```sql
SEARCH CONTEXT 'Alice Chen' DEPTH 2
```

Returns Alice's table row, her graph edges (direct reports), any vectors or documents mentioning her, and the connections between those entities.

### Ask a natural-language question combining all data

```sql
ASK 'who are the senior engineers that report to Alice and have ML experience?'
```

RedDB searches context across tables, graphs, and vectors, builds a structured prompt from the results, and sends it to the configured LLM. The answer cites which collections and entities it used.

## Step 5: Expand Results Across Structures

Use `WITH EXPAND GRAPH` to enrich table query results with graph neighbors:

```sql
SELECT * FROM company WHERE department = 'Engineering' WITH EXPAND GRAPH DEPTH 1
```

Instead of returning only the matching rows, the response includes each employee plus their immediate graph neighbors. For Alice this means her row and all direct reports. For Eva this means her row and Bob (her manager).

You can also expand via cross-references:

```sql
SELECT * FROM company WHERE name = 'Alice Chen' WITH EXPAND ALL
```

`EXPAND ALL` adds both graph neighbors and cross-referenced entities from other collections, giving you a complete picture of Alice across every structure in one query.

## What You Built

You now have a multi-model employee directory where:

- **Table rows** hold structured employee data you can filter and sort with SQL.
- **Graph edges** encode reporting relationships you can traverse with `GRAPH NEIGHBORHOOD`, `GRAPH SHORTEST_PATH`, and `MATCH`.
- **Vectors** capture semantic skill profiles you can search with natural language.
- **Cross-structure queries** (`SEARCH CONTEXT`, `ASK`, `WITH EXPAND`) pull results from all three in a single request.

## Next Steps

- [Graph Analytics Guide](/guides/graph-analytics.md) — Run PageRank, community detection, and centrality on your org graph.
- [Building a Vector Search App](/guides/vector-search.md) — Dive deeper into embedding pipelines and hybrid search.
- [Search Commands Reference](/query/search-commands.md) — Full syntax for `SEARCH CONTEXT`, `ASK`, and all search variants.
