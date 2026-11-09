# ACP (ASK sessions)

RedDB exposes a stable Agent Client Protocol v1 adapter for local and embedded
ASK sessions. ACP is the session and streaming shell; the reasoning loop,
prompts, tool registry, query validation, and authorization remain the same
ones used by `ASK` and `reddb.ask` over MCP.

## Start the agent

```bash
# In-memory database
red acp

# Single-file embedded database
red acp --path ./data/reddb.rdb
```

ACP communicates over stdio as newline-delimited JSON-RPC 2.0. The implemented
v1 methods are:

- `initialize`
- `session/new`
- `session/prompt`
- `session/cancel`
- `session/close`
- `session/update` notifications from RedDB to the client

Each prompt is sent to the native ASK harness. RedDB reports the bounded
planner summary as `agent_thought_chunk`, each catalog/query/final-query step as
a completed ACP tool call, and the natural-language answer as
`agent_message_chunk`. The prompt response ends with `end_turn` or `cancelled`.

## Authorization and snapshots

An ACP session captures the authenticated principal, role, and tenant that
created it. Every planner tool and final SQL query reinstalls that identity.
The database recomputes visible collections and captures a fresh MVCC snapshot
for every prompt, so policy changes and newly committed data are not hidden by
a stale session snapshot.

The `red acp` CLI is currently an embedded adapter and advertises no ACP
authentication methods. Its security boundary is the local process and access
to the database file. Remote RedDB authentication is not emulated or bypassed.

## Current capability boundary

This first adapter deliberately advertises only implemented capabilities:

- text and baseline `resource_link` prompt blocks;
- cooperative cancellation through the engine query cancellation token;
- closeable, connection-local sessions;
- no external MCP server attachment;
- no image, audio, embedded-resource, persistent-session, or remote mode.

Clients must not send capabilities RedDB did not advertise. Unsupported session
or content features fail with an explicit JSON-RPC error.
