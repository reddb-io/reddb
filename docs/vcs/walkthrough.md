# Git for Data — Walkthrough

Hands-on tour of the VCS layer. Uses the `red` CLI against an
in-memory database — every command runs in milliseconds.

## Setup

```bash
cargo build --release --bin red
alias red=./target/release/red
```

All commands below use an in-memory database (no `--path` flag).
To persist state between commands, pass `--path /tmp/demo.rdb` on
every invocation.

## 1. First commit

```bash
$ red vcs commit "initial" --author alice --email alice@example.com
commit 7a1ab1bb5cbab92faedc9673b48d330d93fc6ea8dc42164cc9328ac942cd825d
Height 0
Message: initial
```

What happened under the hood:

- `snapshot_manager.begin()` + `commit()` allocated xid 1.
- SHA-256 of `("reddb-commit-v1" || 1 || "" || "alice" ||
  "alice@example.com" || "initial" || now_ms)` → commit hash.
- Row inserted into `red_commits`.
- `refs/heads/main` created, pointing at the new hash.
- `red_worksets` row for connection 1 updated.

## 2. Branch off and add more commits

```bash
$ red vcs branch feature-x
branch refs/heads/feature-x -> 7a1ab1bb...

$ red vcs checkout feature-x
switched to refs/heads/feature-x

$ red vcs commit "feat: step 1" --author alice --email alice@example.com
$ red vcs commit "feat: step 2" --author alice --email alice@example.com

$ red vcs log --branch feature-x --limit 5
commit <hash-3>
Author: alice <alice@example.com>
    feat: step 2

commit <hash-2>
Author: alice <alice@example.com>
    feat: step 1

commit 7a1ab1bb...
Author: alice <alice@example.com>
    initial
```

## 3. Parallel commit on main

```bash
$ red vcs checkout main
switched to refs/heads/main

$ red vcs commit "main: hotfix" --author alice --email alice@example.com

$ red vcs log --limit 2
commit <hotfix-hash>
Author: alice <alice@example.com>
    main: hotfix

commit 7a1ab1bb...
Author: alice <alice@example.com>
    initial
```

`main` and `feature-x` have now diverged.

## 4. Lowest common ancestor

```bash
$ red vcs lca main feature-x
7a1ab1bb5cbab92faedc9673b48d330d93fc6ea8dc42164cc9328ac942cd825d
```

The initial commit is the LCA — exactly what git would report.

## 5. Fast-forward merge (when possible)

```bash
$ red vcs checkout main
$ red vcs branch trivial --from main
$ red vcs checkout trivial
$ red vcs commit "trivial change"

$ red vcs checkout main
$ red vcs merge trivial
fast-forward
```

`main` now points at the `trivial` head — no merge commit because
`main` was an ancestor of `trivial`.

## 6. Non-fast-forward merge

With `main` and `feature-x` divergent:

```bash
$ red vcs checkout main
$ red vcs merge feature-x
merged (non-ff)
commit <merge-hash>
merge_state ms:<16-hex>
```

A merge commit with two parents (`main`'s previous tip and
`feature-x`'s tip) was created. `red_merge_state` holds the
metadata for any conflicts that arose.

On an empty database there are no user-data conflicts, so
`outcome.conflicts == []`. When user rows diverge on both sides,
`red_conflicts` gets populated — see §10.

## 7. Cherry-pick a single commit

Apply the effect of one commit onto the current HEAD:

```bash
$ red vcs commit "on-main-a"
$ red vcs commit "on-main-b"
$ red vcs branch pick-src
$ red vcs checkout pick-src
$ red vcs commit "bugfix on pick-src"   # this is the one we want on main

$ red vcs checkout main
$ red vcs resolve pick-src              # show the head of pick-src
<pick-src-head-hash>

# (Currently the CLI doesn't expose cherry-pick directly — use
# the REST surface or the library API.)
```

HTTP equivalent:

```bash
curl -X POST http://localhost:8080/vcs/cherry-pick \
  -H 'content-type: application/json' \
  -d '{"connection_id":1,"commit":"<pick-src-head-hash>",
       "author":{"name":"alice","email":"alice@example.com"}}'
```

The new commit on `main` has a message starting with
`"cherry-pick: "` and records a `cp:<16hex>` merge_state with the
metadata needed to later apply the picked delta to user data
(Phase 6.2 work).

## 8. Revert

Create a commit that inverses the effect of an earlier commit:

```bash
# library / REST call
vcs.revert(connection_id, "<target-hash>", author)
```

The new commit's message is `"Revert \"<original message>\""`;
`rv:<16hex>` merge_state holds the inverted diff.

## 9. Reset

```bash
$ red vcs reset <older-hash> --soft
```

`main` now points at `<older-hash>`. Working-set base_commit moves
with it. Data isn't touched — `SELECT * FROM users` still returns
the tip data (this is Phase 6.2 territory).

`--hard` currently returns `not yet implemented`; use `--soft`
or `--mixed` until the MVCC rewind lands.

## 10. Inspect conflicts

After a non-fast-forward merge that did mutate the same user
entities on both sides:

```bash
$ red vcs log --limit 1 --json | jq '.data[0]'
{"hash":"<merge-hash>", ...}

$ curl -s http://localhost:8080/vcs/conflicts/ms:<id> | jq
[
  {
    "id": "ms:<id>:users/42",
    "collection": "users",
    "entity_id": "42",
    "base":   { "name": "Alice", "role": "viewer" },
    "ours":   { "name": "Alice", "role": "admin" },
    "theirs": { "name": "Alice", "role": "editor" },
    "conflicting_paths": ["role"],
    "merge_state_id": "ms:<id>"
  }
]
```

Each conflict row embeds the full JSON bodies at `base`, `ours`,
and `theirs` snapshots so tooling can render a 3-way merge UI
without re-fetching anything. `conflicting_paths` comes straight
from the recursive `merge_json` crate.

## 11. SQL time-travel

Once user collections have data, `AS OF` resolves any ref / hash
/ timestamp / snapshot to an MVCC xid and installs it for the
scope of the query:

```sql
-- state of users at a specific commit
SELECT * FROM users AS OF COMMIT '7a1ab1bb...' WHERE age > 21;

-- state on another branch (even after checkout to main)
SELECT name FROM users AS OF BRANCH 'staging' LIMIT 5;

-- state at a release tag
SELECT * FROM orders AS OF TAG 'v1.0' WHERE total > 100;

-- state at a point in wall-clock time
SELECT * FROM events AS OF TIMESTAMP 1710000000000;

-- state at a raw MVCC xid (power users)
SELECT * FROM users AS OF SNAPSHOT 42;
```

Everything the engine already does — filters, joins, aggregates,
vector search, graph traversals — runs unchanged. Only the
visibility check shifts.

## 12. Combined flow

A typical feature-branch flow in one script:

```bash
red vcs commit "baseline"
red vcs branch refactor
red vcs checkout refactor

# (mutate user data, create/update rows …)

red vcs commit "refactor: module A"
red vcs commit "refactor: module B"

# review what moved
red vcs log --branch refactor --limit 10
curl -X POST http://localhost:8080/vcs/diff \
  -d '{"from":"main","to":"refactor"}' | jq

# merge back
red vcs checkout main
red vcs merge refactor --no-ff -m "Release: refactor modules A+B"

# tag the merge
red vcs tag v1.0 main

# audit the state at release time six months later
# (works forever as long as the commit hash / tag exists)
# SELECT * FROM users AS OF TAG 'v1.0' WHERE ...
```

## 13. What's still manual

- **Data-level conflict resolution** — `vcs_conflict_resolve` in
  Phase 6 deletes the conflict row; writing the resolved JSON
  into the user collection is Phase 6.2. Until then, resolve
  with a normal INSERT/UPDATE and then call `conflict_resolve`
  to clear the marker.
- **Hard reset** — Phase 6.2. Current workaround: `reset --soft`
  + manual DELETE of the unwanted rows.
- **Remote push/pull** — Phase 7. Today the VCS is single-node;
  replication of commits/refs rides on the existing WAL + CDC
  pipeline, but there's no high-level `vcs push`/`vcs pull` yet.
- **Cherry-pick/revert CLI** — currently runtime + REST only.
  `red vcs cherry-pick` / `red vcs revert` are a small wiring
  exercise away.

See the [architecture doc](./architecture.md) for how each piece
fits together, and the [command reference](./commands.md) for
the full surface.
