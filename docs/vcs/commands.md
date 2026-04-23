# Git for Data — Command Reference

Exhaustive reference for every VCS entry point: CLI, REST, SQL.
See [overview](./overview.md) for concepts and [walkthrough](./walkthrough.md)
for a tour.

---

## CLI: `red vcs <subcommand>`

Global flags accepted by every subcommand:

| Flag | Meaning |
|------|---------|
| `--path`, `-d` | Persistent database file. Omit for in-memory. |
| `--connection`, `-c` | Connection id for workset scoping (default `1`). |
| `--json`, `-j` / `--output json` | Machine-readable output. |
| `--author`, `--email` | Author identity (commit / merge / cherry-pick / revert). Default `reddb@localhost`. |

### `red vcs commit <message>`

Create a commit from the current HEAD / workset. No data yet
required — empty commits are accepted (`allow_empty: true`).

```bash
red vcs commit "initial" --author alice --email alice@example.com
red vcs commit "checkpoint" -m "monthly snapshot" --json
```

**JSON output**

```json
{"ok":true,"command":"vcs.commit","data":{
  "hash":"7a1ab1bb...",
  "height":0,
  "parents":[]
}}
```

### `red vcs branch <name> [--from <ref|hash>]`

Create a branch. Short names become `refs/heads/<name>`.

```bash
red vcs branch feature-x                     # from current HEAD
red vcs branch release --from main           # from main
red vcs branch hotfix --from 7a1ab1bb        # from commit hash
```

### `red vcs branches`

List every `refs/heads/*` ref with its target commit.

### `red vcs tag <name> [target]`

Create a tag. Short names become `refs/tags/<name>`.

```bash
red vcs tag v1.0 main
red vcs tag release-2026-04 --from 7a1ab1bb
```

### `red vcs tags`

List every `refs/tags/*`.

### `red vcs checkout <target>`

Switch the connection's HEAD to `target`. Accepts:

- Short branch name (`main`)
- `refs/heads/<name>` or `refs/tags/<name>`
- 64-hex commit hash (detached HEAD)

```bash
red vcs checkout feature-x
red vcs checkout refs/tags/v1.0
red vcs checkout 7a1ab1bb...                 # detached
```

### `red vcs merge <branch> [--ff-only|--no-ff] [-m msg]`

Merge `branch` into the current HEAD.

- Default strategy: fast-forward when possible, merge commit
  otherwise.
- `--ff-only`: fail if not fast-forward.
- `--no-ff`: always create a merge commit.

### `red vcs reset <target> [--soft|--mixed|--hard]`

Move the branch ref + workset base to `target`.

- `--soft` / `--mixed`: move only the ref + workset. Data
  untouched.
- `--hard`: returns `not yet implemented` — Phase 6.2 work.

### `red vcs log [--branch <ref>] [--limit N]`

Walk commit history in reverse topological order from `--branch`
(or HEAD if omitted).

```bash
red vcs log --branch main --limit 10
red vcs log --json | jq '.data'
```

### `red vcs status`

Show HEAD ref, HEAD commit, detached state for the current
connection.

### `red vcs lca <a> <b>`

Print the lowest common ancestor commit hash of two refs / hashes.
Prints `(no common ancestor)` on disjoint histories.

```bash
red vcs lca main feature-x
red vcs lca 7a1a... e3f...
```

### `red vcs resolve <spec>`

Resolve any of these to a 64-hex commit hash:

- Full commit hash (returns as-is after existence check)
- Full ref (`refs/heads/main`, `refs/tags/v1.0`)
- Short branch name (`main`)
- Short commit hash prefix (≥ 4 chars, unique match)

```bash
red vcs resolve main
red vcs resolve 7a1a
red vcs resolve refs/tags/v1.0
```

---

## REST API

Every endpoint accepts and returns JSON with the `{ ok, result }`
/ `{ ok, error }` envelope used by the rest of RedDB's HTTP
surface.

### `POST /vcs/commit`

```json
{
  "connection_id": 1,
  "message": "initial",
  "author": { "name": "alice", "email": "alice@example.com" },
  "committer": null,
  "amend": false,
  "allow_empty": true
}
```

Returns `{ ok, result: Commit }` where `Commit` has `hash`,
`root_xid`, `parents`, `height`, `author`, `committer`, `message`,
`timestamp_ms`, `signature?`.

### `POST /vcs/branch`

```json
{ "name": "feature", "from": "main", "connection_id": 1 }
```

Returns `{ ok, result: Ref }` (`name`, `kind`, `target`, `protected`).

### `GET /vcs/branches`

Returns `{ ok, result: Ref[] }` filtered by prefix `refs/heads/`.

### `DELETE /vcs/branches/<name>`

Remove a branch. Refuses protected branches with a 500 error.

### `POST /vcs/tag`

```json
{ "name": "v1.0", "target": "main", "annotation": null }
```

### `GET /vcs/tags`

Returns `{ ok, result: Ref[] }` filtered by prefix `refs/tags/`.

### `POST /vcs/checkout`

```json
{
  "connection_id": 1,
  "kind": "branch",
  "target": "feature",
  "force": false
}
```

`kind` is one of `branch` / `commit` / `tag`.

### `POST /vcs/merge`

```json
{
  "connection_id": 1,
  "from": "feature",
  "author": { "name": "alice", "email": "..." },
  "strategy": "auto",
  "message": null,
  "abort_on_conflict": false
}
```

`strategy` one of `auto` / `ff-only` / `no-ff`.

Returns `MergeOutcome`:

```json
{
  "fast_forward": true,
  "conflicts": [],
  "merge_commit": { "hash": "...", ... },
  "merge_state_id": null
}
```

### `POST /vcs/reset`

```json
{
  "connection_id": 1,
  "target": "7a1a...",
  "mode": "soft"
}
```

`mode` one of `soft` / `mixed` / `hard`.

### `POST /vcs/log`

```json
{
  "connection_id": 1,
  "to": "main",
  "from": null,
  "limit": 20,
  "skip": 0,
  "no_merges": false
}
```

Returns `Commit[]`.

### `POST /vcs/diff`

```json
{
  "from": "main",
  "to": "feature",
  "collection": null,
  "summary_only": false
}
```

Returns:

```json
{
  "from": "<hash>",
  "to": "<hash>",
  "added": 2,
  "removed": 1,
  "modified": 0,
  "entries": [
    { "collection": "users", "entity_id": "42",
      "change": "added", "after": ... },
    ...
  ]
}
```

### `POST /vcs/status`

```json
{ "connection_id": 1 }
```

Returns `Status`:

```json
{
  "connection_id": 1,
  "head_ref": "refs/heads/main",
  "head_commit": "<hash>",
  "detached": false,
  "staged_changes": 0,
  "working_changes": 0,
  "unresolved_conflicts": 0,
  "merge_state_id": null
}
```

### `GET /vcs/lca?a=<spec>&b=<spec>`

Returns `{ ok, result: { lca: "<hash>" | null } }`.

### `GET /vcs/conflicts/<merge_state_id>`

Returns `Conflict[]` with `id`, `collection`, `entity_id`, `base`,
`ours`, `theirs`, `conflicting_paths`, `merge_state_id`.

---

## SQL: `AS OF` time-travel clause

Extends the `FROM` clause of any SELECT. Goes after the table
name and alias, before `WHERE`:

```sql
SELECT * FROM users AS OF COMMIT '7a1ab1bb...' WHERE age > 21;

SELECT name FROM users AS OF BRANCH 'staging' LIMIT 5;

SELECT * FROM orders AS OF TAG 'v1.0' WHERE total > 100;

SELECT * FROM events AS OF TIMESTAMP 1710000000000;

SELECT * FROM t AS OF SNAPSHOT 42;
```

Grammar:

```
<table_query>  ::= SELECT ... FROM <table> [<alias>] [<as_of>]
                   [WHERE ...] [GROUP BY ...] [ORDER BY ...]
                   [LIMIT ...] [OFFSET ...] [WITH EXPAND ...]

<as_of>        ::= AS OF <spec>
<spec>         ::= COMMIT    <string-literal>
                 | BRANCH    <string-literal>
                 | TAG       <string-literal>
                 | TIMESTAMP <integer>    ; unix epoch ms
                 | SNAPSHOT  <integer>    ; raw MVCC xid
```

### Behaviour

- The clause is resolved to an MVCC `xid` before execution and a
  `CurrentSnapshotGuard` is installed for the scope of the
  statement. Nested subqueries that don't declare their own
  `AS OF` inherit the installed snapshot — they see the same
  historical state.
- Unknown commits / branches / tags raise `NotFound`.
- Malformed `AS OF` (e.g. `AS OF COMMIT 42` instead of a string
  literal) raises a parse error.
- `AS OF SNAPSHOT <xid>` never fails the resolver — the raw xid
  is used directly. Use this only when you already know the xid
  (diagnostics, advanced tooling).
- Writes inside a statement with `AS OF` are **not** prevented;
  if you UPDATE a row while viewing a past snapshot, the write
  stamps the current xid and is visible to every future
  snapshot. Mixing time-travel and mutation is intentionally
  left to the caller — use sessions or explicit transactions to
  separate them.

### Interaction with MVCC

`AS OF` reuses the regular visibility check
`Snapshot::sees(xmin, xmax)`. Rows with `xmin == 0` (pre-MVCC
bootstrap data) are visible at every past snapshot. Rows whose
`xmin` is aborted (rolled-back transaction) stay invisible
regardless of `AS OF` — the abort set is consulted in the same
way as live queries.

---

## Error taxonomy

All VCS entry points surface errors through `RedDBError`:

| Variant | When |
|---------|------|
| `NotFound` | unknown commit / branch / tag / ref |
| `InvalidConfig` | malformed input (empty commitish, root commit as cherry-pick target, duplicate branch, etc.) |
| `ReadOnly` | attempt to delete a protected branch |
| `Internal` | storage failure (propagated from the unified store) |

Phase-gated stubs (currently `reset --hard`, any data-apply path
in `cherry_pick`/`revert` body merge, `conflict_resolve` apply)
return `Internal("vcs: <feature> not yet implemented")` — callers
can detect by prefix.
