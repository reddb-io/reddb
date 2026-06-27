# @reddb-io/mcp

Zero-install [MCP](https://modelcontextprotocol.io) launcher for [RedDB](https://github.com/reddb-io/reddb).

```bash
npx -y @reddb-io/mcp
```

That's the whole thing. The launcher resolves (or downloads) the matching
`red` engine binary from GitHub Releases and spawns `red mcp` over stdio.
Because `npx -y @reddb-io/mcp` always pulls the latest published launcher,
you always run the latest released engine — no local install, no stale
binary.

No tool or knowledge logic is reimplemented in JavaScript. The launcher
only fetches the native binary and execs `red mcp`; the MCP tool surface
and `red://` knowledge resources come from the engine itself.

## Use it as an MCP server

Point your agent host at the launcher as a stdio MCP server:

```json
{
  "mcpServers": {
    "reddb": {
      "command": "npx",
      "args": ["-y", "@reddb-io/mcp"],
      "env": {
        "REDDB_MCP_URI": "${REDDB_MCP_URI:-memory://}"
      }
    }
  }
}
```

The default `memory://` URI starts an embedded ephemeral engine. To use a
file-backed or remote database, export `REDDB_MCP_URI` in the host environment
before your MCP host starts. Keep credentials in the host environment, not in
the committed `.mcp.json`.

Extra arguments are forwarded to `red mcp` after the subcommand, e.g. an
explicit connection URI:

```bash
npx -y @reddb-io/mcp --uri file:///var/lib/reddb/agent.rdb
```

## Binary resolution

Resolution reuses RedDB's internal bin-resolver / asset-fetcher and follows
the same precedence as the SDK (PATH is never consulted, per ADR 0006):

1. `REDDB_BIN` — absolute path to a `red` binary you provide. Returned verbatim.
2. A binary already cached at `<package>/bin/red[.exe]`.
3. Otherwise the matching release asset is downloaded from GitHub Releases.

### Environment overrides

| Variable            | Effect                                              |
| ------------------- | --------------------------------------------------- |
| `REDDB_BIN`         | Use this binary; skip all download logic.           |
| `REDDB_MCP_URI`     | Connection URI forwarded to `red mcp --uri`.        |
| `REDDB_MCP_VERSION` | Pull a specific release tag instead of the default. |
| `REDDB_MCP_REPO`    | Fetch from a fork (default: `reddb-io/reddb`).       |

## License

MIT
