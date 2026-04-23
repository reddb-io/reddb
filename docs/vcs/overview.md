# Git for Data — Overview

RedDB ships a first-class version-control layer. Every mutation runs
under MVCC; every commit pins an immutable snapshot of that MVCC
state; every branch, tag, and `HEAD` is a row in a regular
collection. Together they deliver *real* git semantics on top of a
document/table/graph database — no external DAG, no SHA tree
built out-of-band, no custom file format.

## What you get

| Operation | CLI | REST | SQL |
|-----------|-----|------|-----|
| Commit | `red vcs commit` | `POST /vcs/commit` | — |
| Branch | `red vcs branch` | `POST /vcs/branch` | — |
| Tag | `red vcs tag` | `POST /vcs/tag` | — |
| Checkout | `red vcs checkout` | `POST /vcs/checkout` | — |
| Merge | `red vcs merge` | `POST /vcs/merge` | — |
| Cherry-pick | *(runtime only)* | *(runtime only)* | — |
| Revert | *(runtime only)* | *(runtime only)* | — |
| Reset | *(soft/mixed)* | `POST /vcs/reset` | — |
| Log | `red vcs log` | `POST /vcs/log` | — |
| Status | `red vcs status` | `POST /vcs/status` | — |
| Diff | — | `POST /vcs/diff` | — |
| LCA | `red vcs lca` | `GET /vcs/lca` | — |
| Resolve ref/hash | `red vcs resolve` | — | — |
| Time-travel | — | — | `SELECT ... AS OF ...` |

Direct SQL time-travel (`SELECT ... AS OF COMMIT '<hash>'`) is the
killer feature: queries run against any historical point without
copying data or dumping snapshots.

## Why this design

Rather than reinventing git's object tree, RedDB re-uses primitives
the engine already has:

- **MVCC snapshots** are already immutable, hash-ordered, and
  content-addressed by the `xid` that produced them. We simply
  *pin* the `xid` at commit time so `VACUUM` cannot reclaim those
  row versions. No extra storage.
- **Collections** hold commit metadata, refs, worksets, closure
  rows, and conflict markers exactly like any user table — so
  every operation is a plain INSERT/DELETE that ships through WAL,
  CDC, and replication for free.
- **`AS OF` clause** resolves down to the snapshot `xid` pinned by
  a commit, then installs a `CurrentSnapshotGuard` for the scope
  of the query. Every scan transparently honours the time-travel
  target without duplicating the executor path.

Net result: git-like branching and time-travel come in at
~1500 LoC of runtime code plus a thin application/presentation
surface. Durability, replication, RBAC, and auth apply to the VCS
collections without extra work.

## Use cases

- **Auditable data edits** — every edit is a commit. `SELECT ...
  AS OF COMMIT '<hash>'` reproduces the exact state anyone saw,
  forever.
- **Change review before merge** — feature branches collect
  commits; `vcs diff main..feature` surfaces what moves; `vcs
  merge` produces a merge commit or a conflict set.
- **Safe schema refactors** — commit on a branch, validate with
  `AS OF BRANCH 'feature'` queries, merge when green.
- **Write-ahead review for AI agents** — agents mutate on their
  own branch; humans review with `vcs log`, `vcs diff`, and merge
  only the clean bits.
- **Reproducible analytics** — snapshots pin the dataset that
  produced a report. Re-run the report six months later with the
  same `AS OF COMMIT` and get byte-for-byte identical output.

## Guarantees

- **Deterministic commit hash**: `SHA-256("reddb-commit-v1" ||
  root_xid || sorted_parents || author || message || timestamp_ms)`
  so two commits with the same parents + message + timestamp hash
  identically. No custom object IDs.
- **Monotonic height**: each commit's `height = max(parents.height)
  + 1`, giving sub-linear LCA queries via height-ordered BFS.
- **Reference-counted snapshot pin**: a commit holding xid *N*
  survives VACUUM; deleting that commit (via branch delete with
  no other reachable ref) releases the pin and the MVCC garbage
  collector can reclaim the row versions.
- **Content integrity**: commit bodies, parents, author, and
  message are all hashed together. Modifying any field changes
  the hash — tampering is immediately visible to anyone who saved
  the old hash.

## Storage footprint

All VCS state lives in these seven `red_*` collections, created
on first boot:

| Collection | Contents |
|------------|----------|
| `red_commits` | commit rows — hash, root_xid, parents[], height, author, committer, message, timestamp_ms, signature |
| `red_refs` | branches (`refs/heads/*`), tags (`refs/tags/*`), per-connection `HEAD:<conn>` pointers |
| `red_worksets` | per-connection working/staged state — branch, base_commit, working_xid, merge_state_id |
| `red_closure` | commit ancestry index `(height, commit_hash, ancestor_hash)` for fast LCA |
| `red_conflicts` | unresolved merge conflicts — base/ours/theirs JSON + conflicting paths |
| `red_merge_state` | in-progress merge/cherry_pick/revert/rebase metadata |
| `red_remotes` | remote repository configuration (Phase 7) |

Config defaults live in `red_config` under `red.vcs.*` — inherits
the same dot-notation patterns already used by `red.ai`,
`red.storage`, and friends.

## Next steps

- [Architecture](./architecture.md) — MVCC integration, hashing,
  commit lifecycle
- [Commands](./commands.md) — full reference for CLI, REST, SQL
- [Walkthrough](./walkthrough.md) — hands-on tutorial
- [Guides: Git for Data](../guides/git-for-data.md) — end-to-end
  scenario with real data
