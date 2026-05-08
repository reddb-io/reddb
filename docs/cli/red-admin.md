# `red admin`

`red admin` exposes operator-oriented catalog commands over the HTTP `/query`
endpoint. The collection, index, policy, and stats commands are wrappers around
the read-only `red.*` relations:

- `red.collections`
- `red.columns`
- `red.indices`
- `red.policies`
- `red.stats`

Common flags:

| Flag | Description |
|------|-------------|
| `--bind <addr>` | HTTP server address. Defaults to `127.0.0.1:8080`. |
| `--token <tok>` | Bearer token for protected deployments. `$RED_ADMIN_TOKEN` is also honored. |
| `--json` | Emit JSON arrays for tabular results. |
| `--csv` | Emit CSV for tabular results. |
| `--limit <n>` | Add a `LIMIT` to list, stats, and passthrough query commands when the SQL has no limit. |
| `--no-color` | Disable ANSI color in human output. |

## Collections

List user-visible collections:

```bash
red admin collections list --bind 127.0.0.1:8080
```

Filter by logical model:

```bash
red admin collections list --type table
```

Include runtime-owned collections:

```bash
red admin collections list --include-internal
```

Show one collection with schema, indices, policies, and stats sections:

```bash
red admin collections show users
```

Read stats for all collections or one collection:

```bash
red admin collections stats
red admin collections stats users
```

## Indices

List all visible index metadata:

```bash
red admin indices list
```

Filter to one collection:

```bash
red admin indices list --collection users
```

## Policies

List policy metadata:

```bash
red admin policies list
red admin policies list --collection users
```

## Passthrough Query

Run an explicit SQL query against the same `/query` path:

```bash
red admin query "SELECT * FROM red.collections WHERE internal = false LIMIT 20"
```

JSON output is intentionally a bare array so shell tooling can consume it
directly:

```bash
red admin collections list --json | jq '.[].name'
```
