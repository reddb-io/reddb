# Git for Data — Architecture

This document describes how RedDB's VCS sits on top of MVCC. Read
the [overview](./overview.md) first if you haven't.

## Layers

```
┌──────────────────────────────────────────────────────────────┐
│ Presentation                                                 │
│   REST handlers  (src/server/handlers_vcs.rs)                │
│   CLI subcmds    (src/bin/red.rs::run_vcs_command)           │
│   SQL AS OF      (src/storage/query/parser/table.rs)         │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ Application                                                  │
│   VcsUseCases      (src/application/vcs.rs)                  │
│   JSON parsers     (src/application/vcs_payload.rs)          │
│   3-way merger     (src/application/merge_json.rs)           │
│   Collection names (src/application/vcs_collections.rs)      │
│   RuntimeVcsPort   (src/application/ports.rs)                │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ Runtime                                                      │
│   impl RuntimeVcsPort for RedDBRuntime                       │
│       (src/runtime/impl_vcs.rs)                              │
│   AS OF resolver / snapshot install                          │
│       (src/runtime/impl_core.rs::execute_query)              │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ Storage                                                      │
│   UnifiedStore (TableRow inserts to red_*)                   │
│   SnapshotManager + pin/unpin                                │
│       (src/storage/transaction/snapshot.rs)                  │
└──────────────────────────────────────────────────────────────┘
```

## Commit lifecycle

```
     begin()          commit()        pin(xid)         save_commit()
xid ──────▶ active ──────▶ committed ──────▶ pinned ──────▶ durable
            │                │                │                │
            │                │                │                └─ row in red_commits
            │                │                └─ SnapshotManager.pinned[xid]++
            │                └─ visible to every future snapshot
            └─ snapshot_manager tracks in-progress set

                    │
                    ▼  vcs_commit(...) returns
```

1. `snapshot_manager.begin()` — allocate a fresh monotonic `Xid`.
2. `snapshot_manager.commit(xid)` — immediately mark committed so
   every future snapshot can see rows stamped with this xid.
3. `snapshot_manager.pin(xid)` — ref-count the xid so
   `prune_aborted` skips it. Until *all* references drop (branch
   delete, commit unreachable), the versions survive.
4. `save_commit(...)` — write the canonical `red_commits` row with
   `_id = sha256(...)`, `root_xid = xid`, `parents`, `height`,
   `author`, `committer`, `message`, `timestamp_ms`.
5. `save_ref(branch, hash)` — advance the current branch ref to
   the new commit hash (upsert semantics).
6. `upsert_workset(conn, branch, base = hash, working_xid = xid)`
   — update per-connection working set.

### Commit hash

```
sha256(
  b"reddb-commit-v1\n"          ||
  root_xid.to_be_bytes()        ||
  for p in sorted(parents):
    b"\np=" || p                 ||
  b"\na="  || author.name       ||
  b"\n"    || author.email      ||
  b"\nm="  || message            ||
  b"\nt="  || timestamp_ms.to_be_bytes()
)
```

Sorted parents mean octopus merges produce a stable hash regardless
of the order user-side tooling lists branches. The `reddb-commit-v1`
prefix namespaces the digest so an attacker who has a collision in
a different protocol can't transplant it here.

### Height

`height = max(parents.height) + 1`, or `0` when no parents. Used
for fast LCA queries: iterate `red_closure` in ascending height
and short-circuit on the first common ancestor.

## Per-collection opt-in (Phase 7)

```
red_vcs_settings
  ┌─────────────────────────────────────────────────┐
  │ _id = "users",     versioned = true,  ts_ms = … │
  │ _id = "products",  versioned = true,  ts_ms = … │
  └─────────────────────────────────────────────────┘
       ▲
       │  set_versioned(name, true)  ──▶ delete-then-insert
       │  set_versioned(name, false) ──▶ delete
       │
       ├── ALTER TABLE name SET VERSIONED = true  (SQL DDL)
       ├── POST /vcs/versioned {"collection","enabled"}  (REST)
       ├── red vcs versioned on name  (CLI)
       └── vcs.set_versioned(name, true)  (library)
```

`is_versioned(store, name)` is a cheap query on
`red_vcs_settings` (presence of a row with `versioned = true`).
Gates three decisions:

- `materialize_merge_conflicts` iterates only versioned
  collections when scanning the base/ours/theirs snapshots.
- `vcs_diff` filters collections by the flag before the full scan.
- `execute_query` refuses `AS OF` against a non-versioned user
  collection with a clear error message pointing the caller at
  `vcs.set_versioned(...)`.

Internal `red_*` collections bypass the gate — they are
append-only VCS metadata and always accept `AS OF`. The
`set_versioned_flag` writer refuses to opt one of them in.

### Retroactive semantics

Opting in flips the gate only. No data rewrite, no catalog
migration, no index rebuild. A collection that already has
commits pinning xids *before* the opt-in becomes queryable at
those commits the moment the flag lands — the xids are still
pinned, the MVCC row versions are still in storage, only the
`is_versioned` check was saying "no". SQL:

```sql
-- commits exist, table has data, but the flag was off
ALTER TABLE users SET VERSIONED = true;
-- now this works:
SELECT * FROM users AS OF COMMIT '<older-hash>';
```

This stays true until real VACUUM lands (Phase 7.5) and begins
reclaiming non-pinned-collection versions. At that point, very
late opt-ins may miss intermediate row versions the GC already
removed; opt in early when you know a collection matters.

## AS OF resolution

```
SELECT ... AS OF COMMIT '<hash>' WHERE ...
      │
      ├─ parser:  TableQuery.as_of = AsOfClause::Commit(<hash>)
      │
      ├─ execute_query peek_top_level_as_of(sql):
      │     Some(AsOfSpec::Commit(<hash>))
      │
      ├─ runtime.vcs_resolve_as_of(spec) -> Xid
      │     load red_commits row, return commit.root_xid
      │
      ├─ Snapshot { xid, in_progress: {} }  installed via
      │   CurrentSnapshotGuard::install(...)
      │
      └─ scans honour visibility:
          entity_visible_under_current_snapshot(entity)
          ↓
          snapshot.sees(xmin, xmax) && !is_aborted(xmin)
```

The guard restores the previous snapshot on every return path, so
nested `SELECT` subqueries don't bleed time-travel context out of
their declared scope.

Supported spec variants:

| Spec | Resolution |
|------|------------|
| `AS OF COMMIT '<64-hex>'` | load commit row, return `root_xid` |
| `AS OF BRANCH '<name>'` | resolve ref → commit → `root_xid` |
| `AS OF TAG '<name>'` | resolve ref → commit → `root_xid` |
| `AS OF TIMESTAMP <ms>` | scan commits where `timestamp_ms <= ms`, pick greatest |
| `AS OF SNAPSHOT <xid>` | raw xid, no lookup |

## Merge algorithm

```
vcs_merge(input):
  ours   = HEAD(input.connection_id)
  theirs = resolve_commitish(input.from)
  lca    = vcs_lca(ours, theirs)

  if theirs.ancestors.contains(ours):
    # fast-forward — just move the branch ref
    save_ref(branch, theirs)
    return MergeOutcome { fast_forward: true, ... }

  if strategy == FastForwardOnly:
    return Err("not a fast-forward")

  merge_commit = new commit with parents = [ours, theirs]
  save_commit(merge_commit)
  save_ref(branch, merge_commit)

  conflicts = materialize_merge_conflicts(lca, ours, theirs, merge_state_id)
  save_merge_state(merge_state_id, kind = merge, base = lca,
                   ours, theirs, conflicts_count = conflicts.len())
  return MergeOutcome { merge_commit, conflicts, merge_state_id, ... }
```

### materialize_merge_conflicts

1. Resolve `base_xid`, `ours_xid`, `theirs_xid` via
   `vcs_resolve_as_of(AsOfSpec::Commit(h))`.
2. For every user collection (`coll` not starting with `red_`):
   - `query_all` entities
   - For each version, check visibility at the three snapshots,
     discard aborted writers
   - Group by `entity_id` → per-snapshot JSON body
3. For every entity id present in any of the three maps:
   - Skip if only one side changed vs base
   - Skip if `ours == theirs` (concurrent agreement)
   - Run `three_way_merge(base, ours, theirs)` from
     `application::merge_json`
   - Clean merge → skip (Phase 6.2 will stage for apply)
   - Conflicting → insert row in `red_conflicts` with
     `conflicting_paths`, `base_json`, `ours_json`, `theirs_json`,
     `merge_state_id`

### three_way_merge

Recursive JSON merger in `src/application/merge_json.rs`. Decision
table per node:

| ours | theirs | action |
|------|--------|--------|
| `== theirs` | — | take ours |
| `== base` | — | take theirs |
| — | `== base` | take ours |
| object | object | recurse per key union |
| array | array | length-aware elementwise |
| primitive | primitive (different) | conflict |
| type mismatch | — | conflict |

Array rules:

- Both sides keep the length → elementwise 3-way
- One side changed length, other unchanged → take the changed side
- Both sides changed length differently → whole-array conflict

Deletion + modification → conflict; deletion on both sides →
clean (both removed).

## Cherry-pick / revert

Cherry-pick = 3-way merge with `base = parent(src)`, `ours = HEAD`,
`theirs = src`. The message is `"cherry-pick: <original message>"`;
the commit has `parents = [HEAD]`.

Revert flips the roles: `base = src`, `ours = HEAD`, `theirs =
parent(src)`. Message is `"Revert \"<original message>\""`.

Root and merge commits are rejected for cherry-pick (ambiguous
parent).

## Reset

Reset modes:

| Mode | Semantics |
|------|-----------|
| `Soft` | move HEAD + workset.base_commit. Data untouched. |
| `Mixed` | same as Soft (working-set materialisation lives in Phase 6.2). |
| `Hard` | returns `Phase 6.2` error — MVCC rewind of user data is not yet implemented. |

## Closure index

`red_closure` rows are `(height, commit_hash, ancestor_hash)`. Phase
6 emits closure entries on commit; Phase 8 adds the optimised
`vcs_lca` that iterates closures in parallel height-ascending order
to find the first common ancestor in `O(log N)` ancestor steps.

Until then, `vcs_lca` uses a naive BFS from each side (fine for
histories under ~10k commits).

## Reference points

- `src/application/vcs.rs` — domain types + use-case trait usage
- `src/application/merge_json.rs` — standalone 3-way merger
- `src/application/vcs_collections.rs` — every `red_*` name in one
  constants module
- `src/runtime/impl_vcs.rs` — full persistence layer
- `src/runtime/impl_core.rs` — `peek_top_level_as_of` +
  snapshot guard install
- `src/storage/transaction/snapshot.rs` — pin/unpin + VACUUM
  integration
