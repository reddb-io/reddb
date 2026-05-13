# MCP (AI Agent Integration)

RedDB includes a built-in [Model Context Protocol](https://modelcontextprotocol.io/) (MCP) server that exposes 30 tools for AI agents. This allows LLMs and agent frameworks to interact with RedDB directly.

## Starting the MCP Server

```bash
red mcp --path ./data/reddb.rdb
```

The MCP server communicates over stdio using the MCP protocol, compatible with Claude Desktop, Cursor, and other MCP clients.

## Available Tools

### Query & Collections

| Tool | Description |
|:-----|:------------|
| `reddb_query` | Execute a SQL or universal query (SELECT, INSERT, UPDATE, DELETE, graph) |
| `reddb_collections` | List all collections in the database |
| `reddb_scan` | Scan entities from a collection with pagination |
| `reddb_create_collection` | Create a new collection |
| `reddb_drop_collection` | Drop a collection |

### Entity CRUD

| Tool | Description |
|:-----|:------------|
| `reddb_insert_row` | Insert a table row |
| `reddb_insert_node` | Insert a graph node |
| `reddb_insert_edge` | Insert a graph edge between two nodes |
| `reddb_insert_vector` | Insert a vector embedding |
| `reddb_insert_document` | Insert a JSON document |
| `reddb_kv_get` | Get a value by key |
| `reddb_kv_set` | Set a key-value pair |
| `reddb_update` | Update entities matching a filter |
| `reddb_delete` | Delete an entity by ID |

### Search

| Tool | Description |
|:-----|:------------|
| `reddb.ask` | Grounded question-answering with citation markers and `sources_flat` URNs |
| `reddb_search_vector` | Similarity search by vector |
| `reddb_search_text` | Full-text search across collections |

### Graph Analytics

| Tool | Description |
|:-----|:------------|
| `reddb_graph_traverse` | BFS/DFS traversal from a source node |
| `reddb_graph_shortest_path` | Find shortest path between two nodes |
| `reddb_graph_centrality` | Compute centrality (degree, closeness, betweenness, eigenvector, pagerank) |
| `reddb_graph_community` | Detect communities (louvain, label_propagation) |
| `reddb_graph_components` | Find connected components |
| `reddb_graph_cycles` | Detect cycles |
| `reddb_graph_clustering` | Compute clustering coefficient |

### Auth

| Tool | Description |
|:-----|:------------|
| `reddb_auth_bootstrap` | Bootstrap the first admin user |
| `reddb_auth_create_user` | Create a new user with role |
| `reddb_auth_login` | Login and get session token |
| `reddb_auth_create_api_key` | Create a persistent API key |
| `reddb_auth_list_users` | List all users and roles |

### Health

| Tool | Description |
|:-----|:------------|
| `reddb_health` | Check database health and runtime stats |

## Tool Examples

### Execute a Query

```json
{
  "tool": "reddb_query",
  "arguments": {
    "sql": "SELECT * FROM users WHERE age > 21 ORDER BY name LIMIT 10"
  }
}
```

#### Parameterized Queries (recommended for user input)

Always pass user-provided values via `params` with `$1`, `$2`, … placeholders
instead of interpolating them into the SQL string. Interpolation is unsafe
and brittle; the parameterized form is type-checked and immune to injection.

```json
{
  "tool": "reddb_query",
  "arguments": {
    "sql": "SELECT * FROM users WHERE id = $1 AND name = $2",
    "params": [42, "Alice"]
  }
}
```

Vector binding for similarity search:

```json
{
  "tool": "reddb_query",
  "arguments": {
    "sql": "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 5",
    "params": [[0.12, 0.34, 0.56]]
  }
}
```

`params` accepts a JSON array of `null`, booleans, numbers, strings, and
number arrays (which bind as vectors). Indices are 1-based in SQL (`$1`)
and 0-based in the array. Arity and gap errors surface as MCP tool errors.

### Grounded ASK

Use `reddb.ask` when an agent needs an answer grounded in RedDB data. The
tool returns the canonical ASK envelope: `answer`, `sources_flat`,
`citations`, `validation`, `cache_hit`, provider/model metadata, token counts,
and `cost_usd`. Every factual claim in `answer` is expected to cite a marker
like `[^1]`; that marker maps to `sources_flat[0].urn`. The citation contract
is [ADR 0013](../adr/0013-ask-grounding-citations.md), tracked from
[#392](https://github.com/reddb-io/reddb/issues/392).

```json
{
  "tool": "reddb.ask",
  "arguments": {
    "question": "Which incident mentions passport PT-002?",
    "options": {
      "strict": true,
      "limit": 5,
      "min_score": 0.2,
      "depth": 1
    }
  }
}
```

Provider, model, determinism, and cache controls mirror the SQL ASK clauses:

```json
{
  "tool": "reddb.ask",
  "arguments": {
    "question": "Summarize source-backed escalations for FDD-12313",
    "options": {
      "using": "ollama",
      "model": "mock-ask",
      "temperature": 0,
      "seed": 42,
      "cache": { "ttl": "5m" }
    }
  }
}
```

`cache` and `"nocache": true` are mutually exclusive and mirror
`ASK ... CACHE TTL` / `NOCACHE`. MCP progressive-response streaming is not
enabled yet; `reddb.ask` returns the full non-streaming ASK response as one
tool result.

### Insert a Row

```json
{
  "tool": "reddb_insert_row",
  "arguments": {
    "collection": "users",
    "data": {
      "name": "Alice",
      "email": "alice@example.com",
      "age": 30
    }
  }
}
```

### Insert a Graph Node

```json
{
  "tool": "reddb_insert_node",
  "arguments": {
    "collection": "network",
    "label": "web-server-01",
    "node_type": "host",
    "properties": {
      "ip": "10.0.0.1",
      "datacenter": "us-east"
    }
  }
}
```

### Vector Similarity Search

```json
{
  "tool": "reddb_search_vector",
  "arguments": {
    "collection": "embeddings",
    "vector": [0.12, 0.91, 0.44, 0.33],
    "k": 5,
    "min_score": 0.7
  }
}
```

### Graph Traversal

```json
{
  "tool": "reddb_graph_traverse",
  "arguments": {
    "source": "web-server-01",
    "direction": "outgoing",
    "max_depth": 3,
    "strategy": "bfs"
  }
}
```

### Find Shortest Path

```json
{
  "tool": "reddb_graph_shortest_path",
  "arguments": {
    "source": "alice",
    "target": "charlie",
    "algorithm": "dijkstra"
  }
}
```

### Community Detection

```json
{
  "tool": "reddb_graph_community",
  "arguments": {
    "algorithm": "louvain",
    "max_iterations": 100
  }
}
```

## Claude Desktop Configuration

Add RedDB to your Claude Desktop `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "reddb": {
      "command": "red",
      "args": ["mcp", "--path", "/path/to/data/reddb.rdb"]
    }
  }
}
```

## Cursor Configuration

Add to `.cursor/mcp.json` in your project:

```json
{
  "mcpServers": {
    "reddb": {
      "command": "red",
      "args": ["mcp", "--path", "./data/reddb.rdb"]
    }
  }
}
```

> [!TIP]
> The MCP server runs in embedded mode, so it has the same low-latency access as the Rust API. No network hop is needed.
