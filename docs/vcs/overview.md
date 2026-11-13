# Git for Data — Overview

See [model validation](model-validation.md) for the current test inventory and
the remaining work across collection models.

> **Current limitation:** checkout changes VCS metadata but does not isolate
> reads or writes by branch. Non-fast-forward merge records metadata and
> conflicts, but does not apply merged data. Conflict resolution returns an
> error and retains the conflict until data application is implemented.
> Merges, cherry-picks and reverts that require data application return an error.
> Models with state outside the adopted MVCC entity history reject these
> operations conservatively, including when their entity diff appears empty.
> Graph entities inserted before versioning was enabled, or by older builds
> without an MVCC creation xid, can appear in earlier `AS OF` snapshots.
> Native entity and collection reads apply the connection's MVCC snapshot;
> this does not provide branch isolation.
> Do not rely on these operations for isolated
> agent changes or reviewed publication of graph data.

RedDB persists commits, branches, tags and `HEAD` in regular collections.
Commits pin MVCC snapshots for the read and write paths that retain history.
The operation inventory below includes metadata operations whose data semantics
are still incomplete; it is not a claim of full Git semantics for collections.

## What you get

| Operation | CLI | REST | SQL |
|-----------|-----|------|-----|
| Repo summary | — | `GET /repo` | — |
| Commit | `red vcs commit` | `POST /repo/commits` | — |
| Log | `red vcs log` | `GET /repo/commits?branch=&limit=` | — |
| Show commit | — | `GET /repo/commits/{hash}` | — |
| Branch create | `red vcs branch` | `POST /repo/refs/heads` | — |
| Branch list | `red vcs branches` | `GET /repo/refs/heads` | — |
| Branch move | — | `PUT /repo/refs/heads/{name}` | — |
| Branch delete | — | `DELETE /repo/refs/heads/{name}` | — |
| Tag create/list/delete | `red vcs tag` / `tags` | `POST/GET/DELETE /repo/refs/tags[/{name}]` | — |
| Checkout | `red vcs checkout` | `POST /repo/sessions/{conn}/checkout` | — |
| Merge | `red vcs merge` | `POST /repo/sessions/{conn}/merge` | — |
| Cherry-pick | — | `POST /repo/sessions/{conn}/cherry-pick` | — |
| Revert | — | `POST /repo/sessions/{conn}/revert` | — |
| Reset (soft/mixed) | `red vcs reset` | `POST /repo/sessions/{conn}/reset` | — |
| Status | `red vcs status` | `GET /repo/sessions/{conn}` | — |
| Diff | — | `GET /repo/commits/{a}/diff/{b}` | — |
| LCA | `red vcs lca` | `GET /repo/commits/{a}/lca/{b}` | — |
| Merge conflicts | — | `GET /repo/merges/{msid}/conflicts` | — |
| Resolve conflict | — | `POST /repo/merges/{msid}/conflicts/{cid}/resolve` | — |
| Opt-in collection | `red vcs versioned on X` | `PUT /collections/{name}/vcs` | `ALTER TABLE ... SET VERSIONED = true` |
| Check opt-in | `red vcs versioned check X` | `GET /collections/{name}/vcs` | — |
| Resolve ref/hash | `red vcs resolve` | — | — |
| Time-travel | — | — | `SELECT ... AS OF COMMIT\|BRANCH\|TAG\|TIMESTAMP\|SNAPSHOT ...` |

SQL time-travel (`SELECT ... AS OF COMMIT '<hash>'`) reads retained entity
versions through the commit's captured snapshot. Support depends on the
collection's write and read paths; see the model validation inventory.

## Why this design

RedDB uses MVCC snapshots and internal collections:

- **Snapshot retention** pins the earliest horizon needed by each commit,
  including transactions excluded while still active. Retaining older entity
  versions consumes storage; an xid is a transaction identifier, not a content hash.
- **Metadata collections** hold commits, refs, worksets and conflicts through
  the store's persistence paths. This alone does not make a sequence of metadata
  writes an atomic publication of user data.
- **`AS OF`** resolves a commit's xid and captured exclusions and installs the
  resulting snapshot for supported query paths.

## Use and implementation limits

Explicit commits can retain historical data for the adopted MVCC paths, and
diffs expose changes between those snapshots. Writes do not automatically create
VCS commits. Branch-isolated agent edits, reviewed merge publication and schema
versioning remain implementation work. Historical data also does not guarantee
identical results from volatile functions or changing query definitions.

## Guarantees

- **Deterministic commit hash**: the hash includes `root_xid`, sorted parents,
  author, message and timestamp, with the `reddb-commit-v1` prefix. Identical
  values for all hash inputs yield the same hash.
- **Monotonic height**: each commit's `height = max(parents.height)
  + 1`, giving sub-linear LCA queries via height-ordered BFS.
- **Snapshot pins** are restored when the database reopens. Deleting a branch
  removes its reference; it does not delete the commit records or reclaim their
  retained history.
- **Hash scope** covers the listed commit metadata, not the entity bodies or
  every stored commit field. It is not a cryptographic integrity proof of the
  historical dataset.

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
# REST — collection-centric PUT on the collection's /vcs aspect
curl -X PUT http://localhost:5000/collections/users/vcs \
  -H 'content-type: application/json' \
  -d '{"versioned": true}'

# Read current state
curl http://localhost:5000/collections/users/vcs
# → { "ok": true, "result": { "collection": "users", "versioned": true } }
```

### Retroactive opt-in

`ALTER TABLE ... SET VERSIONED = true` can enable an existing collection.
Earlier commits are meaningful only to the extent that the collection already
retained MVCC versions with creation and deletion xids. Enabling the flag does
not reconstruct overwritten data or date an older unstamped insertion.

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

Opt in before writing data whose history must be retained. Versions already
overwritten or reclaimed cannot be recovered by changing the flag.

### What opt-in changes

- **`vcs_diff`** iterates only versioned collections.
- **`vcs_merge` / cherry-pick / revert** materialise conflicts
  only from versioned collections.
- **`AS OF` queries** error with
  `AS OF requires a versioned collection — \`X\` has not opted in`
  when the target table hasn't been flagged.
- **Internal `red_*` collections** are excluded from user opt-in and diffs.
  Their mutable metadata is not a historical copy of the catalog.
- **Storage**: adopted write paths preserve old physical versions. Commit
  pins constrain reclamation through the shared snapshot manager; they are
  not independent per-collection garbage-collection horizons.

### Default-off rationale

Most transactional data doesn't warrant history:

- Sessions, tokens, and short-TTL caches churn dozens of times
  per row per day — history would multiply their storage cost
  without any audit value.
- Queue consumption is operational. Versioning payloads is useful for agent
  branches, but delivery leases must stay local to each writer epoch.
- Vector indices and derived aggregates can be rebuilt from
  source data on demand.

Conversely, users / products / audit_log / transactional ledgers
are obvious opt-in candidates — every change should be reviewable
and reproducible.

## Storage footprint

VCS metadata lives in these `red_*` collections, created
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
| `red_vcs_settings` | per-collection opt-in flag (`id = name`, `versioned = true`) |

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
