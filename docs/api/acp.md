# ACP (ASK sessions)

RedDB exposes Agent Client Protocol (ACP) v1 and v2 adapters for local,
embedded ASK sessions. An ACP-compatible host starts `red acp` as a child
process, sends prompts over stdin, and renders the streamed bounded reasoning
summary, tool calls, final query, and answer received from stdout.

`red acp` is **not an interactive terminal**. If you run it by hand and see no
prompt, it is waiting for newline-delimited JSON-RPC messages from an ACP
client. The client owns the user interface and the RedDB process is the agent.
This command provides the ACP agent adapter; it does not include an ACP host or
chat UI.
If your editor or agent runner cannot launch a stdio ACP agent, `red acp` is not
the right entry point; use `ASK` or MCP instead.

ACP is only the session and streaming shell. The reasoning loop, prompts, tool
registry, query validation, and authorization are the same ones used by `ASK`
and `reddb.ask` over MCP. ACP v1 is the stable protocol; v2 remains draft and
is enabled only when the client negotiates version 2.

## Choose ASK, MCP, or ACP

| Goal | Use | Who controls the reasoning loop? |
|:-----|:----|:---------------------------------|
| Ask one question from SQL/RQL or an application | `ASK '...'` | RedDB ASK |
| Give RedDB tools to an existing AI agent | `red mcp` | The external agent; it may call `reddb.ask` |
| Present RedDB ASK itself as a conversational agent in an ACP host | `red acp` | RedDB ASK, with the host managing sessions and rendering updates |

If an agent already connects through MCP, use `reddb.ask` when you want the
native ASK harness. Adding ACP is unnecessary unless the host specifically
supports ACP and should treat RedDB itself as the agent.

`red acp` is currently embedded-only. For a remote RedDB server, use
`red mcp --uri ...` and its authenticated `reddb.ask` tool, or call `ASK`
through a server transport.

## Quickstart

### 1. Configure the ASK provider

ACP uses the same provider configuration as `ASK`. The simplest local setup is
to export the default provider and its credential before the ACP host starts
RedDB:

```bash
export REDDB_AI_PROVIDER=openai
export REDDB_OPENAI_API_KEY="..."
```

PowerShell:

```powershell
$env:REDDB_AI_PROVIDER = "openai"
$env:REDDB_OPENAI_API_KEY = "..."
```

Replace `OPENAI` with the selected provider token. Vault-backed credentials and
the normal `ai.*` keys in `red.config` also apply. See
[AI providers](../guides/ai-providers.md) and
[configuration](../getting-started/configuration.md).

### 2. Enable tool-driven investigation when needed

The planner-first ASK loop is currently opt-in. Before starting the ACP host,
enable it in the database when you want visible-catalog discovery, exploratory
queries, one final validation query, and an auditable reasoning trace:

```bash
red query "SET CONFIG ai.ask.planner = true" --path /absolute/path/to/data.rdb
```

With the setting left at its default `false`, ACP still works, but ordinary
prompts use the grounded retrieval-RAG ASK path instead of the bounded tool
loop. See [ASK](../query/search-commands.md#ask) for the two execution paths
and their result contracts.

### 3. Choose the embedded database

For useful database questions, point the adapter at a single-file RedDB
database:

```bash
red acp --path /absolute/path/to/data.rdb
```

Omitting `--path` creates an empty in-memory database. That is useful for
client integration tests, but it has no application data to investigate.

Prefer an absolute path in host configuration because the host chooses the
child process working directory. The ACP adapter opens the file as an embedded
RedDB runtime; it does not connect to a separately running RedDB server.

### 4. Configure the ACP host

ACP host configuration formats differ, but every host needs the same process
mapping:

| Setting | Value |
|:--------|:------|
| Transport | stdio |
| Executable | `red` |
| Arguments | `acp --path /absolute/path/to/data.rdb` |
| Environment | Provider selection and credential variables, unless inherited |

A conceptual host entry looks like this; translate the field names to the
host's own configuration format:

```json
{
  "name": "reddb",
  "transport": "stdio",
  "command": "red",
  "args": ["acp", "--path", "/absolute/path/to/data.rdb"],
  "env": {
    "REDDB_AI_PROVIDER": "openai"
  }
}
```

This belongs in the host's ACP agent configuration, not its MCP server
configuration. ACP and MCP both use stdio here, but they implement different
protocols and are not interchangeable.

Do not put an API key in a checked-in configuration file. Let the child
process inherit credentials from the host environment or use the RedDB vault.

The host must negotiate a supported protocol version, create a session, and
then send text prompts. For example:

> Which recent activities belong to passport 123, and which contracted
> systems can that person access?

With planner-first ASK enabled, RedDB streams a bounded planner summary,
completed catalog/query tool calls, the final validation query, and the
grounded answer. This is an auditable action summary, not private model
chain-of-thought. The host decides how to display those events.

## Wire-level diagnostic

You normally should not write ACP frames yourself. When implementing or
debugging a host, send one JSON-RPC 2.0 object per line and read one response or
notification per line. A minimal v1 exchange is:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/absolute/working/directory","mcpServers":[]}}
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"<session-id-from-response-2>","prompt":[{"type":"text","text":"Which collections contain passport_number?"}]}}
{"jsonrpc":"2.0","id":4,"method":"session/close","params":{"sessionId":"<session-id-from-response-2>"}}
```

Wait for each response before using values from it. `cwd` must be absolute. A
v2 `initialize` request must also include client information:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": 2,
    "info": { "name": "my-acp-host", "version": "1.0.0" }
  }
}
```

The client must verify the negotiated version returned by `initialize`.

## Protocol lifecycle

The implemented v1 methods are:

- `initialize`
- `session/new`
- `session/prompt`
- `session/cancel`
- `session/close`
- `session/update` notifications from RedDB to the client

In v1, RedDB reports the bounded planner summary as `agent_thought_chunk`, each
catalog/query/final-query step as a completed ACP tool call, and the answer as
`agent_message_chunk`. The prompt response ends with `end_turn` or `cancelled`.

The implemented v2 baseline adds `session/list` and `session/resume`. A v2
`session/prompt` response acknowledges insertion and returns `messageId` before
ASK finishes. User, thought, tool, and answer updates use stable entity IDs and
v2 upsert semantics. Foreground work transitions through `state_update` from
`running` to `idle`, whose `stopReason` reports `end_turn`, `cancelled`, or
`error`.

Sessions are connection-local. v2 list and resume work within the current ACP
process; conversation replay cursors and persistence across process restarts
are not advertised or accepted.

## Authorization and snapshots

An ACP session captures the authenticated principal, role, and tenant that
created it. Every planner tool and final SQL query reinstalls that identity.
The database recomputes visible collections and captures a fresh MVCC snapshot
for every prompt, so policy changes and newly committed data are not hidden by
a stale session snapshot.

The `red acp` CLI is currently an embedded adapter and advertises no ACP
authentication methods. Its security boundary is the local process, operating
system permissions, and access to the database file. Remote RedDB
authentication is not emulated or bypassed. Use authenticated MCP or another
server transport when users must be isolated by remote credentials.

## Current capability boundary

This adapter deliberately advertises only implemented capabilities:

- text and baseline `resource_link` prompt blocks;
- cooperative cancellation through the engine query cancellation token;
- closeable, connection-local sessions, including the v2 list/resume baseline;
- no external MCP server attachment;
- no image, audio, embedded-resource, history replay, persistent-session, or
  remote mode.

For ACP v1, `session/new` must send `"mcpServers": []`. ACP v2 may omit the
field or send an empty array. Non-empty external MCP server lists are rejected.
Clients must not send capabilities RedDB did not advertise. Unsupported session
or content features fail with an explicit JSON-RPC error.
