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

### SQL: `ALTER TABLE ... SET VERSIONED = <bool>`

Runs the opt-in/out through the standard DDL path. Mirrors the
existing `SET APPEND_ONLY = ...` syntax:

```sql
ALTER TABLE users SET VERSIONED = true;
ALTER TABLE sessions SET VERSIONED = false;
```

Works retroactively: previously-inserted rows become visible via
`AS OF COMMIT` as long as the referenced commit's `xid` is still
pinned and the row versions are still in storage (they are by
default — VACUUM doesn't reclaim them until Phase 7.5 lands).

### `red vcs versioned [list|on|off|check] [collection]`

Manage per-collection opt-in. Collections are non-versioned by
default; only opt-in collections participate in merge, diff, and
AS OF.

```bash
red vcs versioned list                   # every opted-in name
red vcs versioned on users               # opt users in
red vcs versioned off sessions           # opt sessions out
red vcs versioned check users            # versioned? true/false
```

Internal `red_*` collections cannot be opted in (refused with
`cannot version internal collection`).

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

RESTful, collection-centric. Resources live under `/repo/*`
(repository-global state: refs, commits, sessions, merges) and
`/collections/{name}/*` (per-collection aspects). Every response
uses the standard `{ ok, result }` / `{ ok, error }` envelope.

### Cheat sheet

| Method | Path | Purpose |
|--------|------|---------|
| GET    | `/repo` | Repo summary: branch count, tag count, versioned collections, default branch |
| GET    | `/repo/refs[?prefix=...]` | List all refs (branches + tags) |
| GET    | `/repo/refs/heads` | List branches |
| POST   | `/repo/refs/heads` | Create branch (`{name, from?, connection_id?}`) |
| GET    | `/repo/refs/heads/{name}` | Show one branch |
| PUT    | `/repo/refs/heads/{name}` | Move branch to commit (`{commit}` — soft-reset semantics) |
| DELETE | `/repo/refs/heads/{name}` | Delete branch |
| GET    | `/repo/refs/tags` | List tags |
| POST   | `/repo/refs/tags` | Create tag (`{name, target, annotation?}`) |
| GET    | `/repo/refs/tags/{name}` | Show one tag |
| DELETE | `/repo/refs/tags/{name}` | Delete tag |
| GET    | `/repo/commits?branch=&limit=&skip=&from=&to=&no_merges=` | List commits (topo walk) |
| POST   | `/repo/commits` | Create commit from session's workset (`{message, author, connection_id, committer?, allow_empty?}`) |
| GET    | `/repo/commits/{hash}` | Show single commit |
| GET    | `/repo/commits/{a}/diff/{b}[?collection=&summary=true]` | Diff between two commit-ish specs |
| GET    | `/repo/commits/{a}/lca/{b}` | Lowest common ancestor |
| GET    | `/repo/sessions/{conn}` | Working-set status for that connection |
| POST   | `/repo/sessions/{conn}/checkout` | Switch HEAD (`{kind: branch\|tag\|commit, target, force?}`) |
| POST   | `/repo/sessions/{conn}/merge` | Merge into current HEAD (`{from, author, strategy?, message?}`) |
| POST   | `/repo/sessions/{conn}/reset` | Reset HEAD (`{target, mode: soft\|mixed\|hard}`) |
| POST   | `/repo/sessions/{conn}/cherry-pick` | (`{commit, author}`) |
| POST   | `/repo/sessions/{conn}/revert` | (`{commit, author}`) |
| GET    | `/repo/merges/{msid}` | Merge-state summary |
| GET    | `/repo/merges/{msid}/conflicts` | List unresolved conflicts for that merge |
| POST   | `/repo/merges/{msid}/conflicts/{cid}/resolve` | Resolve one conflict (`{value}`) |
| GET    | `/collections/{name}/vcs` | Is this collection versioned? |
| PUT    | `/collections/{name}/vcs` | Opt in or out (`{versioned: true \| false}`) |

### Example: fast-forward merge end-to-end

```bash
# 1. Opt the users collection into VCS (collection-centric)
curl -X PUT http://localhost:8080/collections/users/vcs \
  -H 'content-type: application/json' \
  -d '{"versioned": true}'

# 2. Create a commit from connection 1's workset
curl -X POST http://localhost:8080/repo/commits \
  -H 'content-type: application/json' \
  -d '{
    "connection_id": 1,
    "message": "seed users",
    "author": {"name":"alice","email":"alice@example.com"},
    "allow_empty": true
  }'
# 201 { ok, result: { hash, parents, height, ... } }

# 3. Branch, switch, make more commits
curl -X POST http://localhost:8080/repo/refs/heads \
  -d '{"name":"feature","connection_id":1}'
curl -X POST http://localhost:8080/repo/sessions/1/checkout \
  -d '{"kind":"branch","target":"feature"}'
curl -X POST http://localhost:8080/repo/commits \
  -d '{"connection_id":1,"message":"feat 1","author":{...},"allow_empty":true}'

# 4. Merge back into main
curl -X POST http://localhost:8080/repo/sessions/1/checkout \
  -d '{"kind":"branch","target":"main"}'
curl -X POST http://localhost:8080/repo/sessions/1/merge \
  -d '{"from":"feature","author":{"name":"alice","email":"alice@example.com"}}'
# 200 { ok, result: { fast_forward: true, conflicts: [], merge_commit: {...} } }

# 5. Inspect history
curl 'http://localhost:8080/repo/commits?branch=main&limit=5'
curl http://localhost:8080/repo/commits/<hash>/diff/<other-hash>
curl 'http://localhost:8080/repo/commits/<hash>/lca/<other-hash>'
```

### Status codes

| Code | Meaning |
|------|---------|
| 200  | OK (existing resource or read) |
| 201  | Created (commit, branch, tag) |
| 204  | No content (successful DELETE, successful conflict resolve) |
| 400  | Invalid body / bad request (malformed JSON, missing field, invalid config) |
| 404  | Resource not found (commit hash, branch, tag, collection) |
| 405  | Method not allowed on this path |
| 409  | Conflict (e.g., delete of a protected branch) |
| 500  | Internal (unexpected storage or engine error) |

Every error body:

```json
{ "ok": false, "error": "branch `refs/heads/main` is protected" }
```

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

- **Opt-in gate** (Phase 7): `AS OF` on a user collection that
  has not opted into VCS via `vcs.set_versioned(name, true)`
  raises an error (`AS OF requires a versioned collection`).
  Internal `red_*` collections are append-only and always accept
  `AS OF` without opt-in.
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
