# Using RedDB with AI Agents (MCP)

This guide shows how to connect AI agents to RedDB using the Model Context Protocol (MCP).

## What is MCP?

MCP (Model Context Protocol) is an open standard for connecting AI models to external tools and data sources. RedDB's MCP server exposes 29 tools that AI agents can call directly.

## Setup

### 1. Start the MCP Server

```bash
red mcp --path ./data/agent.rdb
```

### 2. Configure Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `~/.config/Claude/claude_desktop_config.json` (Linux):

```json
{
  "mcpServers": {
    "reddb": {
      "command": "red",
      "args": ["mcp", "--path", "/path/to/data/agent.rdb"]
    }
  }
}
```

### 3. Configure Cursor

Add to `.cursor/mcp.json` in your project:

```json
{
  "mcpServers": {
    "reddb": {
      "command": "red",
      "args": ["mcp", "--path", "./data/agent.rdb"]
    }
  }
}
```

## What Can Agents Do?

Once connected, AI agents can:

### Store and Query Data

- Create collections and insert rows, documents, nodes, edges, vectors
- Run SQL queries and universal queries
- Scan and paginate through collections

### Search

- Vector similarity search across embeddings
- Full-text search with fuzzy matching

### Analyze Graphs

- Traverse networks
- Find shortest paths
- Compute centrality scores
- Detect communities
- Find cycles

### Manage Auth

- Bootstrap admin users
- Create users and API keys
- Manage roles

## Example Conversation

**User**: "Store information about our team members and their projects"

**Agent** (using MCP tools):

1. Calls `reddb_insert_row` to store team members
2. Calls `reddb_insert_node` to create graph nodes for each person
3. Calls `reddb_insert_edge` to link people to their projects
4. Calls `reddb_query` to verify: `"SELECT * FROM team"`

**User**: "Who is the most connected person?"

**Agent**:

1. Calls `reddb_graph_centrality` with `algorithm: "degree"`
2. Returns the person with the highest degree centrality

**User**: "Find similar documents to our product roadmap"

**Agent**:

1. Calls `reddb_search_text` with the query
2. Returns ranked results with similarity scores

## Available Tools

See [MCP API Reference](/api/mcp.md) for the complete list of 29 tools with their schemas.

> [!TIP]
> The MCP server runs in embedded mode, giving agents the same low-latency access as the Rust API. Each tool call is a direct function call, not a network request.
