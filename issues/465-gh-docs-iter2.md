---
status: open
tag: AFK
gh: 465
---

# [AFK] gh-465 iter 2: Driver READMEs + docs/query/*.md cross-check

GitHub: reddb-io/reddb#465

## Iter 1 (already landed on main, commit 4b4d93c2)

- README.md: document INSERT JSON literal, KV PUT, HYPERTABLE forms aligned with parser.
- docs/data-models/{partition-ttl,timeseries}.md: hypertable corrections.
- docs/guides/{logs-quickstart,using-reddb-for-logs}.md: flagged unsupported forms.
- docs/reference/{sql-1-0-x,limitations}.md: examples + status table.

## Iter 2 — what's still missing per #465 acceptance

- [ ] Driver READMEs (crates/reddb-client/README.md, crates/reddb-client-connector/README.md, drivers/*/README.md) match helper conformance results.
- [ ] docs/query/*.md cross-checked against parser; remove examples not accepted by parser.
- [ ] Graph commands flagged: documented-but-untested (GRAPH HITS, TOPOLOGICAL_SORT, PROPERTIES, PATH). Mark as planned where unsupported.

## Iter 2 run (2026-05-16)

Cross-checked `docs/query/*.md` against parser sources under
`crates/reddb-server/src/storage/query/parser/`. Surgical edits only.

Files touched:
- `docs/query/graph-commands.md`
  - GRAPH HITS: parser does not list HITS among graph subcommands
    (only NEIGHBORHOOD, SHORTEST_PATH, TRAVERSE, CENTRALITY,
    COMMUNITY, COMPONENTS, CYCLES, CLUSTERING, TOPOLOGICAL_SORT,
    PROPERTIES). Replaced the example with a NOTE pointing to
    `GRAPH CENTRALITY ALGORITHM pagerank`. Ref
    `parser/graph_commands.rs:25-33`.
  - GRAPH CYCLES: parser only consumes `MAX_LENGTH`. Dropped
    `MAX_CYCLES 50` from the example and added a NOTE. Ref
    `graph_commands.rs:243-252`.
- `docs/query/search-commands.md`
  - SEARCH SIMILAR parameter table said `IN collection` (required)
    and `K n`. Parser requires `COLLECTION col` and uses `LIMIT`
    (with `$N` placeholder). Rewrote the table. Ref
    `parser/search_commands.rs:43-119`.
  - SEARCH IVF: no `Token::Ivf` and no parse arm — SQL form is not
    accepted today. Replaced example with NOTE pointing users to
    `SEARCH SIMILAR` / `VECTOR SEARCH` (the runtime picks IVF
    automatically when present).

Spot-checked, no change needed:
- `docs/query/insert.md` — already uses parameterized form.
- `docs/query/probabilistic-commands.md` — HLL/SKETCH/FILTER
  forms match `tests/e2e_probabilistic_public_contract.rs`.
- `crates/reddb-client/README.md` — Rust helper examples match
  current `Reddb` / `documents()` / `kv_collection` / `queue` APIs.
- `crates/reddb-client-connector/README.md` — internal-only.
- `drivers/go/README.md` — sampled; Value mapping table matches
  wire codec.

## Still open after iter 2

- [ ] Full audit of `drivers/{cpp,dart,dotnet,java,js,js-client,
  kotlin,php,python,python-asyncio,zig}/README.md` vs helper
  conformance fixtures.
- [ ] Contract matrix doc linking each public promise to a test
  (larger structural deliverable).
- [ ] Test-coverage audit for `GRAPH CLUSTERING`,
  `TOPOLOGICAL_SORT`, `PROPERTIES`, `PATH FROM` (parser accepts
  them, but #465 wants behaviour pinned to tests).

## Blocker

Bash `git` operations still require approval. To land:
  `git add docs/query/graph-commands.md docs/query/search-commands.md issues/465-gh-docs-iter2.md`
  Suggested message:
  `docs: align graph + search command examples with parser (refs #465)`

Issue stays **open** — iter 2 closes parser drift under
`docs/query/*.md`; driver-README sweep + contract matrix remain.

## Out of scope this iter

- Contract matrix doc (larger structural deliverable, separate issue).
- Adding new test coverage for graph commands (test audit, not docs fix).

## Notes

- Commit with `Refs #465`.
- `CARGO_TARGET_DIR=.target-gh465-iter2`.
- Be surgical — read parser tests under `tests/` to confirm each form before "fixing" any doc example.
