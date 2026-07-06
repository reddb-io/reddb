# ADR 0071 — EXPLAIN never commits: execute-and-rollback for DML

Status: accepted
Date: 2026-07-05

## Context

Reviewing sqlite-utils (2026-07-05) surfaced the value of a cheap "what would
this change?" preview before destructive operations. The CLI side is easy
(`--dry-run` printing the resolved action), but the engine surface raised a
real fork: introduce a new `DRY RUN <stmt>` verb, or extend `EXPLAIN`?

Two facts decided the frame. First, RQL already treats `EXPLAIN` as an
effect-preview for mutating statements — `ExplainAlter` and `ExplainMigration`
exist today (`reddb-rql/src/core.rs`); a `DRY RUN` verb would be a second name
for the same idiom. Second, Postgres has a famous footgun here: `EXPLAIN
ANALYZE` on DML **executes and commits** the write. Nobody has ever wanted
that behavior; it survives in PG for implementation reasons, not user ones.

RedDB's MVCC (snapshot isolation + first-committer-wins, #1383) makes the
honest alternative structurally cheap: run the statement for real inside a
transaction that is guaranteed to abort, and report actual affected counts.
The snapshot and the write-set discard already exist; a never-commits wrapper
is policy, not new machinery.

## Decision

**1. No `DRY RUN` verb — the `EXPLAIN` family extends to DML.** Following the
`ExplainAlter`/`ExplainMigration` precedent, `EXPLAIN` accepts mutating
statements (`UPDATE`, `DELETE`, `INSERT`, …).

**2. Two tiers.** `EXPLAIN <dml>` = plan only, nothing executes — cheap and
deterministic. `EXPLAIN ANALYZE <dml>` = executes inside an SI transaction
that **always aborts**, reporting real affected counts ("would update 1,342
rows"), timing, and plan. The user chooses between cheap estimate and paid
truth with syntax everyone already knows.

**3. Invariant: RedDB never commits under EXPLAIN.** A deliberate, pinned
divergence from Postgres (joins the #1100 pinned-divergence list). Sold as
safety, not incompatibility: under `EXPLAIN`, no statement can ever mutate
durable state.

**4. Effects fire only at commit — now a testable contract.** The rollback is
only clean if every observable side channel (subscriptions, queue emissions,
triggers, notifications) fires at commit, never during execution. Any channel
violating this is a bug to fix, not an exception to document.

## Rejected, and why

- **A separate `DRY RUN <stmt>` verb.** Rejected — duplicates the existing
  `EXPLAIN`-on-mutating-statement idiom; two names for one concept, and every
  driver/doc/tutorial would have to explain the difference forever.
- **PG-compatible commit semantics for `EXPLAIN ANALYZE` DML.** Rejected —
  compatibility with a footgun is not compatibility worth having. The
  divergence is easy to defend and easy to document.
- **Single tier (EXPLAIN always executes-and-aborts).** Rejected — loses the
  cheap plan-only preview and turns `EXPLAIN` on a large `UPDATE` into a
  surprise full execution.
- **Planner-estimate counts instead of real execution.** Rejected — without
  mature histograms the estimates are guesses, and a preview feature that
  reports wrong numbers destroys the trust it exists to build. (The plan-only
  tier still shows estimates, labeled as such.)

## Consequences

- The divergence is pinned in the RQL conformance divergence list (#1100) and
  documented loudly for PG-wire clients, whose `EXPLAIN ANALYZE` habits carry
  PG expectations.
- The executor needs a guaranteed-abort transaction wrapper for the ANALYZE
  tier — abort on success, abort on error, no code path commits.
- "Effects fire only at commit" becomes an asserted invariant with tests
  across subscription/queue/trigger channels, valuable well beyond EXPLAIN.
- The CLI `--dry-run` flag can desugar to `EXPLAIN` when the command is
  statement-shaped, keeping one preview mechanism end to end.
- `EXPLAIN ANALYZE` holds a real SI transaction for the statement's duration;
  long DML previews cost what long DML costs (minus the commit).

## Related

- #1100 — RQL conformance suite and pinned standard-SQL divergences
- #1383 — versioned multi-model MVCC (SI + first-committer-wins substrate)
- `.red/context/data-model.md` — Query glossary: **EXPLAIN family**
- sqlite-utils review, 2026-07-05 — the session that surfaced the fork
