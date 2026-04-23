# Git for Data — End-to-End Tutorial

This guide walks through the full lifecycle of a feature branch
that mutates real user data: commit → branch → diverge →
non-fast-forward merge → conflict resolution → time-travel audit
query. Everything runs against RedDB's in-memory mode with the
`red` CLI + `curl`, so you can paste straight into a terminal.

Prerequisite: `cargo build --release --bin red` and the server
started with HTTP enabled:

```bash
./target/release/red server --http --http-bind 127.0.0.1:8080 \
  --path /tmp/git-for-data.rdb
```

All subsequent commands assume `alias red=./target/release/red`
and `alias reddb='curl -s http://127.0.0.1:8080'`.

---

## 1. Seed the database

Two users go in first — on an implicit initial commit:

```bash
reddb -X POST /entities/users \
  -H 'content-type: application/json' \
  -d '{"name":"Alice","role":"viewer","age":33}'

reddb -X POST /entities/users \
  -H 'content-type: application/json' \
  -d '{"name":"Bob","role":"viewer","age":41}'

red vcs commit "seed users" --author alice --email alice@example.com --path /tmp/git-for-data.rdb
```

```
commit 7a1ab1bb5cbab92faedc9673b48d330d93fc6ea8dc42164cc9328ac942cd825d
Height 0
Message: seed users
```

Grab that hash — we'll reference it as `$INIT`:

```bash
INIT=$(red vcs resolve main --path /tmp/git-for-data.rdb --json | jq -r .data.hash)
```

## 2. Open a feature branch

```bash
red vcs branch promote-admins --path /tmp/git-for-data.rdb
red vcs checkout promote-admins --path /tmp/git-for-data.rdb
```

On this branch, promote Alice to admin:

```bash
reddb -X PATCH /entities/users/1 \
  -H 'content-type: application/json' \
  -d '{"role":"admin"}'

red vcs commit "promote alice to admin" \
  --author alice --email alice@example.com \
  --path /tmp/git-for-data.rdb
```

## 3. Diverge on main

Back on main, an unrelated change:

```bash
red vcs checkout main --path /tmp/git-for-data.rdb

reddb -X PATCH /entities/users/2 \
  -H 'content-type: application/json' \
  -d '{"age":42}'

red vcs commit "correct Bob's age" \
  --author alice --email alice@example.com \
  --path /tmp/git-for-data.rdb
```

## 4. Lowest common ancestor

```bash
red vcs lca main promote-admins --path /tmp/git-for-data.rdb
# -> $INIT
```

Both branches share the seed commit — exactly what you'd expect.

## 5. Fast-forward path (when branches have not diverged)

Demonstrate FF separately. Create a no-op branch first:

```bash
red vcs branch trivial --path /tmp/git-for-data.rdb
red vcs checkout trivial --path /tmp/git-for-data.rdb
red vcs commit "trivial bump" \
  --author alice --email alice@example.com \
  --path /tmp/git-for-data.rdb

red vcs checkout main --path /tmp/git-for-data.rdb
red vcs merge trivial --path /tmp/git-for-data.rdb
# -> fast-forward
```

## 6. Non-fast-forward merge

Now merge the divergent branch:

```bash
red vcs merge promote-admins --path /tmp/git-for-data.rdb
```

```
merged (non-ff)
commit <merge-hash>
merge_state ms:<16hex>
```

Because `main` and `promote-admins` touched *different* rows
(Bob's age vs Alice's role), the recursive JSON merger finds no
conflict. `red_conflicts` stays empty.

## 7. Introduce a conflict, then merge again

Rewind and set up a real conflict:

```bash
# undo the merge (soft reset back to init)
red vcs reset $INIT --mode soft --path /tmp/git-for-data.rdb
```

Mutate Alice's `role` on *both* sides:

```bash
# branch
red vcs checkout promote-admins --path /tmp/git-for-data.rdb
reddb -X PATCH /entities/users/1 \
  -H 'content-type: application/json' \
  -d '{"role":"admin"}'
red vcs commit "promote alice to admin" \
  --author alice --email alice@example.com \
  --path /tmp/git-for-data.rdb

# main
red vcs checkout main --path /tmp/git-for-data.rdb
reddb -X PATCH /entities/users/1 \
  -H 'content-type: application/json' \
  -d '{"role":"editor"}'
red vcs commit "promote alice to editor" \
  --author alice --email alice@example.com \
  --path /tmp/git-for-data.rdb

# now merge — the same field moved on both sides
OUTCOME=$(red vcs merge promote-admins --json --path /tmp/git-for-data.rdb)
echo "$OUTCOME" | jq
```

`OUTCOME.data.conflicts` now has one entry, and
`OUTCOME.data.merge_state_id` points at a `ms:<hex>` row.

## 8. Inspect the conflict

```bash
MSID=$(echo "$OUTCOME" | jq -r .data.merge_state_id)

reddb /vcs/conflicts/$MSID | jq
```

```json
[
  {
    "id": "ms:<id>:users/1",
    "collection": "users",
    "entity_id": "1",
    "base":   { "name": "Alice", "role": "viewer", "age": 33 },
    "ours":   { "name": "Alice", "role": "editor", "age": 33 },
    "theirs": { "name": "Alice", "role": "admin", "age": 33 },
    "conflicting_paths": ["role"],
    "merge_state_id": "ms:<id>"
  }
]
```

The base/ours/theirs JSON comes straight from the three MVCC
snapshots the resolver pinned for the merge. `conflicting_paths`
is emitted by `three_way_merge` in
`src/application/merge_json.rs`.

## 9. Resolve the conflict

Decide the final value (say, `admin`), apply it as a normal PATCH,
then clear the marker:

```bash
reddb -X PATCH /entities/users/1 \
  -H 'content-type: application/json' \
  -d '{"role":"admin"}'

# delete the conflict row via the resolve endpoint
reddb -X POST /vcs/resolve-conflict \
  -H 'content-type: application/json' \
  -d "{\"conflict_id\":\"$MSID:users/1\",\"resolved\":{\"role\":\"admin\"}}"
```

> Phase 6.2 will fold the resolve call into an automatic apply
> so you won't need the extra PATCH. Today the runtime records
> the decision; a tiny glue PR ships the apply step.

## 10. Time-travel audit

Six months later, an auditor asks: *"what was Alice's role at
the time of tag `v1.0`?"*

```bash
red vcs tag v1.0 main --path /tmp/git-for-data.rdb

# … many months pass …

# the exact row at that moment:
reddb -X POST /query \
  -H 'content-type: application/json' \
  -d "{\"sql\":\"SELECT role FROM users AS OF TAG 'v1.0' WHERE _entity_id = 1\"}" | jq
```

The scanner installs the MVCC snapshot pinned by the `v1.0` tag
and resolves the row from that snapshot's perspective — even if
Alice's role has changed ten times since.

## 11. What you just exercised

- **Commits as MVCC snapshots** — every `red vcs commit` allocates
  a fresh xid, pins it so VACUUM can't reclaim the row versions,
  and writes a deterministic SHA-256 into `red_commits`.
- **Refs as plain rows** — `refs/heads/main`, `refs/heads/promote-admins`,
  `refs/tags/v1.0`, and `HEAD:<conn>` are all single rows in
  `red_refs`.
- **3-way merge** — on non-fast-forward, the resolver walks every
  user collection at `lca_xid`, `ours_xid`, `theirs_xid`; groups
  entities by id; runs recursive JSON 3-way; emits `red_conflicts`
  rows with full base/ours/theirs bodies for truly conflicting
  paths.
- **Time travel** — the parser captures `AS OF COMMIT/BRANCH/TAG/
  TIMESTAMP/SNAPSHOT`; the runtime resolves it to an xid and
  installs a snapshot guard for the whole statement.

See the [VCS reference](/vcs/commands.md) for the full command
surface and the [architecture doc](/vcs/architecture.md) for the
mechanics.
