# RRF hybrid retrieval + LIMIT K (RrfFuser) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/398

Labels: needs-triage

GitHub issue number: #398

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Replaces the current bucket-by-bucket retrieval with hybrid retrieval fused via Reciprocal Rank Fusion.

Per-bucket top-K retrieval (BM25 over text, vector similarity, graph traversal at `DEPTH N`) feeds a `RrfFuser` deep module that produces a single flat ranked list. Total source budget is controlled by `ASK '...' LIMIT N` (default 20).

`RrfFuser` is pure: takes ranked lists, fuses with standard RRF (`k=60`), prunes to total K, returns flat array with stable URN attachment. `MIN_SCORE` filters per-bucket before fusion.

## Acceptance criteria

- [ ] `RrfFuser` deep module with unit tests against published RRF reference values.
- [ ] `ASK '...' LIMIT 20` enforced as default; per-query override works.
- [ ] `ASK '...' MIN_SCORE 0.7` filters low-confidence hits per bucket.
- [ ] `ASK '...' DEPTH 2` controls graph traversal depth.
- [ ] Tie-break is deterministic (same scores → same order across calls).
- [ ] Integration test verifying a known-good question retrieves the expected ranked URNs.

## Blocked by

- #394
