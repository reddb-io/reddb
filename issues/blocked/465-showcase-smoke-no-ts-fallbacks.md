# Showcase smoke: run `pnpm insights …` end-to-end without TS-side fallbacks [AFK]

Labels: test, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

The capstone verification slice. With every other slice from this PRD landed, drive the `ex-grimms-fairy-tales` showcase against a freshly-built `red` binary + the updated SDK and confirm:

- Every `pnpm insights …` command runs to completion.
- No row carries the literal string `<json N bytes>` anywhere in the output.
- The cosine-similarity command (`pnpm insights cosine`) uses native `VECTOR SEARCH`, not the TS-side fallback. The "TS impl" caveat in the showcase README can be removed.
- Curated graph queries use multi-node MATCH with edge-label filters and return the expected subgraph (not the union of single-node matches).
- Probabilistic estimate reads (`SELECT CARDINALITY`, `SELECT FREQ('x')`, `SELECT CONTAINS('x')`) run against the engine and not via TS-side workarounds.
- `db.queue`, `db.kv.get`, `db.exists`, `db.list` calls in the showcase exist instead of raw `db.query` workarounds.
- `db.insert` / `db.bulkInsert` calls in the showcase consume `r.id` / `r.ids` and the sequential-id calibration logic is deleted.

This issue lives in the host repo (this one), not in the showcase. The deliverable is a CI step or a documented manual run that exercises the showcase against the engine + SDK built from `main` and asserts the criteria above.

## Acceptance criteria

- [ ] Documented procedure (script or workflow file) that builds `red` + SDK from `main`, points `REDDB_BIN` at the local build, and runs the full showcase.
- [ ] The procedure asserts: zero occurrences of `<json N bytes>` in output; cosine fallback removed; multi-node MATCH used; probabilistic reads via SELECT; SDK helpers used; sequential-id calibration removed.
- [ ] If any assertion fails, the failure points at the responsible slice (#446-#464).
- [ ] Showcase README updated to remove the TS-impl caveats and reflect the working multi-model surface.

## Blocked by

- #446 (multi-node MATCH)
- #454 (native vector)
- #456 (probabilistic SQL reads)
- #457 (graph traversal edge filter)
- #458 (insert id wire)
- #460 (KV)
- #461 (queue)
- #464 (SDK insert types)

## Progress note - 2026-05-13

The declared RedDB-side dependencies are now present in `issues/done/`:

- `issues/done/446-match-edge-expansion.md`
- `issues/done/454-native-vector-collection.md`
- `issues/done/456-probabilistic-sql-read-forms.md`
- `issues/done/457-graph-traversal-edge-filter-label-source.md`
- `issues/done/458-stdio-insert-returns-id.md`
- `issues/done/460-sdk-kv-colon-and-get.md`
- `issues/done/461-sdk-queue-client.md`
- `issues/done/464-sdk-insert-id-types.md`

This verification slice is blocked on the external showcase checkout state.
`../ex-grimms-fairy-tales` exists, but it has a large dirty worktree,
including a modified `README.md` and many deleted/added input files. The
acceptance criteria require updating the showcase README, so completing this
issue would require editing over existing non-RedDB-repo local changes.

Observed blocker:

```text
git -C ../ex-grimms-fairy-tales status --short --branch
## main...origin/main
 M README.md
 D input/BOOKS.txt
 D input/ONTOLOGY.md
 D input/SCHEMA.md
 D input/TALES.json
 D input/ontology.json
 D input/tales/...
?? .github/
?? docs/
?? input/1-bronze/
?? input/2-silver/
?? input/3-gold/
?? package.json
?? pnpm-lock.yaml
?? scripts/
```

Next unblock step: provide a clean showcase checkout/worktree, or explicitly
approve editing the existing dirty `../ex-grimms-fairy-tales` checkout. After
that, add the host-repo smoke procedure/CI entry, run `pnpm insights ...`
against the freshly built local `red` binary and SDK, assert the no-fallback
conditions, and update the showcase README.
