---
status: done
tag: AFK
gh: 465
---

## Iter 4 run (2026-05-16)

Created `docs/reference/contract-matrix.md` — sections: SQL commands,
Graph, HTTP endpoints, SDK helpers, Probabilistic, ASK/SEARCH, Data
models. Each row links to a real file path on `main` (verified via
glob over `tests/`, `crates/reddb-server/tests/`, and `drivers/`).
Unsupported/planned rows cite #463/#465/#478/#517 or PSC-xxx
companion ledger.

Links added:
- `docs/_sidebar.md` → Reference section.
- `README.md` → Links section.

Acceptance:
- [x] doc exists with ≥5 rows per section (SQL: 10, Graph: 5,
  HTTP: 7, SDK: 9, Probabilistic: 5, ASK: 4, Data models: 6)
- [x] every Proven row links to a file that exists on main
- [x] Unsupported / planned items cite issue numbers
- [x] README + docs sidebar link to the matrix

Closes #465.

## Blocker

Bash `git` operations require approval in this harness, so iter 4
edits are uncommitted. To land:

  git add docs/reference/contract-matrix.md docs/_sidebar.md README.md
  git mv issues/465-gh-contract-matrix.md issues/done/465-gh-contract-matrix.md
  git commit -m "docs: contract matrix linking public promises to tests (closes #465)"

Files changed:
- docs/reference/contract-matrix.md (new)
- docs/_sidebar.md (+1 line under Reference)
- README.md (+1 line under Links)
- issues/465-gh-contract-matrix.md (this note)

# [AFK] gh-465 iter 4: Contract matrix doc

GitHub: reddb-io/reddb#465

## What's left from #465

> Contract matrix links each public promise to a test, issue, or explicit unsupported note.

## What to build

`docs/reference/contract-matrix.md` — a single doc that lists every public SQL command / HTTP endpoint / SDK helper promise documented in `docs/` and links each to:
- a passing e2e test under `tests/` or `crates/*/tests/`, OR
- an open GitHub issue tracking the gap, OR
- an explicit "Unsupported / planned" note pointing at a roadmap entry.

## Structure

```markdown
# Public Contract Matrix

This matrix is the source of truth for what RedDB promises in public docs. Each row links a promise to its proof (a test) or its disclaimer (an issue / unsupported note).

## SQL commands

| Promise | Doc | Proof / Status |
|---|---|---|
| `INSERT INTO ... RETURNING *` row envelope | docs/data-models/tables.md | tests/e2e_rid_row_envelope.rs |
| ... | | |

## HTTP endpoints

| Endpoint | Doc | Proof / Status |
|---|---|---|
| `GET /backup/status` | docs/api/http.md | crates/reddb-server/tests/handlers_backup.rs (closed #517) |
| ... | | |

## SDK helpers (per language)

| Surface | Languages | Proof / Status |
|---|---|---|
| documents.insert | go, dart, java, dotnet, php, cpp, kotlin, zig | drivers/<lang>/...helpers_test.<ext> (closed #463) |
| ... | | |

## Probabilistic structures

| Promise | Doc | Proof / Status |
|---|---|---|
| HLL approximate count | docs/query/probabilistic-commands.md | tests/e2e_probabilistic_public_contract.rs |
| ... | | |

## Graph

| Promise | Doc | Proof / Status |
|---|---|---|
| GRAPH CENTRALITY pagerank | docs/query/graph-commands.md | tests/... |
| GRAPH CLUSTERING | docs/query/graph-commands.md | **Unsupported / planned** (no e2e test yet — see iter 2 note) |
| ... | | |

## ASK / SEARCH

| Promise | Doc | Proof / Status |
|---|---|---|
| SEARCH CONTEXT bucket coverage | docs/guides/ask-your-database.md | tests/e2e_ask_search_conformance.rs (closed #464) |
| ... | | |
```

## Acceptance for iter 4

- [ ] `docs/reference/contract-matrix.md` exists with at least 5 rows per section
- [ ] Every row links to a real file path (test or doc) that exists on main
- [ ] Unsupported/planned items have an explicit issue number where one exists
- [ ] README or docs/index links to the matrix

## Notes

- Commit with `Closes #465` if the matrix is reasonably populated, else `Refs #465`.
- This is a docs deliverable — no code changes needed.
- Don't try to be exhaustive; aim for representative coverage of the 6 surface families above.
- Don't fabricate test paths — only link tests that actually exist.
