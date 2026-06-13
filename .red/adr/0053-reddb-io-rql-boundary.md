# ADR 0053 — `reddb-io-rql` boundary: language front-end + conformance ownership, executors stay

Status: accepted
Date: 2026-06-09

## Decision

The new **`reddb-io-rql`** crate owns RQL's **language front-end**: lexer →
parser → AST → the seven mode translators (SQL, Gremlin, Cypher, SPARQL, Path,
Natural, vector extensions — all thin, pure translators into the shared SQL
AST) → analyzer → expression typing → optimizer. Text in, typed logical plan
out (~27k LOC). It depends on `reddb-io-types` (ADR 0052) and on nothing else
of the server.

The **physical executors stay in `reddb-server`** (~20k LOC: `executors/`,
`batch/` SIMD, `engine/` — all storage-coupled). The rql crate does *not*
execute queries.

What it does own beyond parsing is the **correctness specification of
execution**: an sqllogictest-format conformance suite (mature Rust harness, as
used by DataFusion/RisingWave) whose truth comes from the **public SQLite
corpus** for the standard-SQL subset and from hand-authored goldens for
RedDB-only surfaces (vector, graph modes, extensions). The suite data lives in
the rql crate; CI runs it against the server engine. Engine-output
characterization snapshots may serve as a cheap regression layer on RedDB-only
surfaces but are never promoted to "truth".

Quality posture: full parser pyramid (proptest AST→print→re-parse round-trip,
cargo-fuzz no-panic, corpus goldens including error-message-with-position
snapshots, differential accept/reject vs `sqlparser-rs`). The aspirational
coverage target remains **~95%+**, and CI now enforces a **90% line-coverage
floor** for the pure `reddb-io-rql` library scope (`--lib`, excluding
`reddb-io-types` from totals).

Sequencing: three phases (types re-home → conformance-suite-before-move → rql
extraction → pyramid), each a PRD drained by `/afk`, queued after the current
drain empties.

## Why

"Winning query experience" splits into two guarantees: the *language* is
bulletproof (everything parses, types check, errors carry positions) and the
*results* are right. The front-end is the only half that is genuinely
storage-agnostic — a clean 27k-LOC extraction with an achievable coverage
target. Moving the executors would drag 20k storage-coupled LOC across a crate
boundary for no testability gain. Owning the conformance suite instead gives
the rql crate authority over result-correctness — the thing users feel —
without owning the machinery that produces results.

## Considered options

- **Whole engine in the crate** (front-end + executors behind storage ports) —
  rejected: maximal churn, the port abstraction would be invented under
  pressure, and SIMD/batch executors are married to the engine.
- **Front-end only, no conformance suite** — rejected: 100%-covered parser
  with unspecified execution correctness doesn't deliver the stated goal.
- **Characterization-only truth** (snapshot current engine output) — rejected
  as primary truth: it freezes today's bugs as "correct". Kept only as a
  regression layer on RedDB-only surfaces.
