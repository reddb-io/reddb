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

## Opt-in per collection

**User collections are non-versioned by default.** Nothing you
create participates in Git-for-Data until you explicitly mark it:

```rust
// Library API
vcs.set_versioned("users", true)?;
vcs.set_versioned("sessions", false)?;  // default, shown for clarity
```

```bash
# CLI
red vcs versioned on users
red vcs versioned off users
red vcs versioned list
red vcs versioned check users
```

```sql
-- SQL DDL (works retroactively — see below)
ALTER TABLE users SET VERSIONED = true;
ALTER TABLE sessions SET VERSIONED = false;
```

```bash
# REST
curl -X POST http://localhost:8080/vcs/versioned \
  -H 'content-type: application/json' \
  -d '{"collection":"users","enabled":true}'

curl http://localhost:8080/vcs/versioned
```

### Retroactive opt-in

`ALTER TABLE ... SET VERSIONED = true` works on a collection
that already has data and commits. You can flip any existing
collection into VCS after the fact and immediately query earlier
commits via `AS OF COMMIT '<older-hash>'` — as long as those
commits still hold their `xid` pin, the MVCC row versions are
still present and reachable.

```sql
CREATE TABLE products (...);
INSERT INTO products VALUES (...);
-- no opt-in yet

-- make some commits at the VCS layer
-- ... (commits reference the products table even though it's
-- not explicitly opted in — their root_xid is still pinned)

ALTER TABLE products SET VERSIONED = true;

-- now this works:
SELECT * FROM products AS OF COMMIT '<older-hash>';
```

Caveat: once real VACUUM lands (Phase 7.5+), opting in *very*
late may miss intermediate row versions that VACUUM reclaimed
before the opt-in. If you know a collection will ever be
versioned, opt in early.

### What opt-in changes

- **`vcs_diff`** iterates only versioned collections.
- **`vcs_merge` / cherry-pick / revert** materialise conflicts
  only from versioned collections.
- **`AS OF` queries** error with
  `AS OF requires a versioned collection — \`X\` has not opted in`
  when the target table hasn't been flagged.
- **Internal `red_*` collections** are always treated as
  append-only and always accept `AS OF` (they store VCS metadata
  itself; their history is the history of the history).
- **Storage**: versions of rows in non-versioned collections stay
  reclaim-eligible by VACUUM; only versioned collections pin
  row versions behind commit references. This is the main disk-
  cost lever — keep ephemeral data (sessions, caches, queues)
  out of VCS and let VACUUM prune aggressively.

### Default-off rationale

Most transactional data doesn't warrant history:

- Sessions, tokens, and short-TTL caches churn dozens of times
  per row per day — history would multiply their storage cost
  without any audit value.
- Queues and streams are semantically linear; commits don't map
  to them.
- Vector indices and derived aggregates can be rebuilt from
  source data on demand.

Conversely, users / products / audit_log / transactional ledgers
are obvious opt-in candidates — every change should be reviewable
and reproducible.

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
| `red_vcs_settings` | per-collection opt-in flag (`_id = name`, `versioned = true`) |

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
