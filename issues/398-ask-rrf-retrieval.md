# RRF hybrid retrieval + LIMIT K (RrfFuser) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/398

Labels: enhancement

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

## Progress

Slice 1: `RrfFuser` deep module landed at
`crates/reddb-server/src/runtime/ai/rrf_fuser.rs` with 17 unit tests
covering RRF reference values (1/(60+rank)), multi-list contribution
sum, k constant override, total_k cap, per-bucket `min_score` filter
(including rank-promotion after drop and independent floors per
bucket), deterministic id-ascending tie-break, bucket-order
independence, edge cases (empty buckets, duplicate id within bucket,
generic id types).

The module is pure — no I/O, no transport, no clock — and exposes:

- `Candidate<Id> { id, score }` — per-bucket entry
- `Bucket<Id> { candidates, min_score }` — one ranker's ranked list
- `FusedItem<Id> { id, rrf_score }` — output row
- `fuse(buckets, k, total_k) -> Vec<FusedItem>`
- `RRF_K_DEFAULT = 60`

Deferred to follow-up slices (each independently shippable):

- Wire `fuse()` into `AskPipeline` retrieval — currently bucket
  retrieval is dispatched per-bucket and concatenated; this needs
  redirecting through the fuser with the per-bucket `min_score`
  threaded from `ASK '...' MIN_SCORE`.
- Parse `ASK '...' LIMIT N` (default 20) and `ASK '...' DEPTH N` in
  the SQL parser and thread into the pipeline.
- Integration test verifying a known-good question retrieves the
  expected ranked URNs (depends on the wiring above).

Issue stays open with this progress note.

Slice 2: `ASK '...' MIN_SCORE <f>` now parses into the public
`AskQuery` AST and is threaded into the ASK vector retrieval bucket.
The existing `AskPipeline::execute_with_limit` path remains as the
legacy no-threshold wrapper; `execute_with_limit_and_min_score` carries
the per-query floor and `vector_search_scoped` forwards it to
`AuthorizedSearch::execute_similar`. gRPC ASK JSON payloads also accept
`min_score`.

Tests added/updated:

- `crates/reddb-server/tests/ask_parser.rs` covers ASK `MIN_SCORE`
  alongside DEPTH/LIMIT/COLLECTION.
- `crates/reddb-server/tests/support/parser_hardening/ask_grammar.rs`
  now emits optional `MIN_SCORE` in the ASK property generator.

Verification for this slice:

- Red check: targeted ASK parser test failed on missing
  `AskQuery.min_score`.
- Green check: `cargo test -p reddb-io-server --test ask_parser`.
- `cargo check -p reddb-io-server --lib`.
- `git diff --check`.
- `pnpm test` skipped because `target/debug/red` is not built.
- `pnpm typecheck` exited 1 after reporting `TypeScript: No errors
  found` through the repo wrapper.
- Attempted a focused server lib-test for vector-bucket threshold
  behavior, but the server lib-test binary currently fails before
  targeted ASK tests can run because of unrelated borrow-check errors
  in `crates/reddb-server/src/runtime/ai/pg_wire_ask_row_encoder.rs`.

Deferred to follow-up slices:

- Route all ASK retrieval buckets through `RrfFuser` as a single fused
  source list rather than preserving the current rows-then-vector
  ordering.
- Thread `ASK '...' DEPTH N` into graph traversal retrieval.
- Add the final known-good ranked-URN integration test after the fused
  source-list wiring lands.

Slice 3: ASK prompt/source assembly now consumes an RRF-fused source
order instead of preserving the previous filtered-rows-then-vector
concatenation. `AskContext` carries the total `source_limit` selected
from `ASK ... LIMIT N` or the default row cap, and
`fused_source_order` converts Stage 4 row-filter hits plus Stage 3
vector hits into `RrfFuser` buckets capped by that total. Duplicate
row/vector hits for the same collection/entity fuse to one source
reference, preferring the row payload while still receiving both RRF
contributions.

The prompt formatter and `sources_flat` builder both walk the fused
order, so `[^N]` citation markers and response URNs now share the same
post-RRF ordering. Equal RRF scores use the fuser's deterministic
source-id tie-break.

Tests added/updated:

- `runtime::ask_pipeline::tests::fused_source_order_uses_rrf_and_total_limit`
  covers duplicate row/vector fusion and total source limiting.
- `runtime::impl_search::citation_wedge_tests::build_sources_flat_orders_rows_before_vectors_with_urns`
  now pins deterministic RRF ordering instead of legacy row-first
  ordering.

Verification for this slice:

- Red check: focused prompt/source test failed on the old row-first
  `sources_flat` expectation.
- Green checks:
  - `env CARGO_INCREMENTAL=0 cargo test -p reddb-io-server --lib -- ask_pipeline::tests::fused_source_order_uses_rrf_and_total_limit`
  - `env CARGO_INCREMENTAL=0 cargo test -p reddb-io-server --lib -- render_prompt_tests citation_wedge_tests::build_sources_flat_orders_rows_before_vectors_with_urns citation_wedge_tests::system_prompt_carries_citation_directive`
  - `env CARGO_INCREMENTAL=0 cargo check -p reddb-io-server --lib`
  - `rustfmt --check crates/reddb-server/src/runtime/ask_pipeline.rs crates/reddb-server/src/runtime/impl_search.rs`
  - `git diff --check`
  - `pnpm test` exited 0 after skipping because
    `target/debug/red` is not built.
  - `pnpm typecheck` exited 1 after reporting
    `TypeScript: No errors found`, matching the known wrapper behavior.

Deferred to follow-up slices:

- Add true BM25/text and graph traversal buckets to the fused source
  list; this slice fuses the currently materialized row-filter and
  vector buckets.
- Thread `ASK '...' DEPTH N` into graph traversal retrieval.
- Add the final known-good ranked-URN integration test once graph/text
  buckets participate in the fused list.
