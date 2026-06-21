# House Style (TigerStyle-derived)

RedDB follows a deliberately-scoped, correctness-first house style adapted from
TigerBeetle's [TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md).
This page is a **summary and index** — the source of truth is
[`STYLE.md`](https://github.com/reddb-io/reddb/blob/main/STYLE.md) at the repo
root, and the rationale (including the rules we consciously rejected) is in
**ADR 0056 — Adopt a TigerStyle-derived house style**. Read those before
assuming a TigerBeetle rule applies here.

Design goals, in order: **safety, performance, developer experience.** These
rules apply to **new and changed code**; match surrounding code when editing
existing files (CLAUDE.md §3, Surgical Changes).

## The rules at a glance

- **Two kinds of failure.** *Operating* errors (disk, network, corrupt data,
  exhaustion) are expected and must be handled by propagating a `Result`.
  *Programmer* errors (broken invariants) are unexpected and must crash. A bare
  `.unwrap()` blurs the two and is banned — write `.expect("invariant: …")`
  stating why the value cannot be absent, or propagate the error.
- **Assertions stay live in release.** `assert!` (not `debug_assert!`) is the
  default; use `debug_assert!` only in a *measured* hot loop. Pair assertions
  across at least two code paths, assert both the positive and negative space,
  and use compile-time asserts for type-size / layout invariants.
- **Function shape.** Push `if`s up and `for`s down — keep branching in the
  parent and move non-branchy fragments to pure leaf helpers. A soft
  `too_many_lines` lint nudges long new functions to split; it is a prompt, not
  a hard wall.
- **Naming (new code).** `snake_case`; no abbreviations; units/qualifiers last
  by descending significance (`latency_ms_max`, not `max_latency_ms`); helpers
  carry the caller's name; nouns over participles.
- **Off-by-one discipline.** Treat `index` (0-based), `count` (1-based), and
  `size` (count × unit) as distinct quantities. Show division intent
  (`div_ceil` / `div_floor`) and prefer checked conversions over silent `as`
  truncation on lengths and on-disk offsets.
- **Performance is design-time first.** A PRD or ADR that changes the **data
  plane** (WAL, pager, query execution) must carry a one-line
  back-of-the-envelope sketch across network / disk / memory / CPU. Separate
  control plane from data plane, batch the data plane, and pool/reuse buffers
  on hot paths instead of allocating per operation.
- **Untrusted input is depth-bounded.** Every parse/expansion path over
  untrusted input (RQL especially) carries an explicit depth cap — recursion is
  fine when bounded. Generalize the existing `JSON_LITERAL_MAX_DEPTH` pattern
  rather than inventing a new guard per call site.

## Consciously rejected

ADR 0056 records four TigerBeetle rules RedDB declined, with rationale, so a
future contributor does not reintroduce them:

- **Static allocation / no dynamic allocation after init** — rejected wholesale
  (RedDB has arbitrary collections, variable-size rows, growing B-trees); only
  the hot-path spirit ("don't allocate per operation") is adopted.
- **Zero dependencies** — rejected as a literal rule; the defensible version
  (data-path primitives and wire/file/crypto/JSON formats homed in-house) is
  governed by ADR 0046 / ADR 0054, not re-codified here.
- **Avoid `usize`** — rejected; `usize` is the correct Rust type for indexing
  and lengths, and avoiding it would force truncating casts everywhere.
- **Hard 70-line function limit** — rejected as a hard limit; adopted as a soft
  ratchet (~100–120 lines), since idiomatic Rust `match` / builder code
  legitimately runs longer than a Zig 70-line cap.

## Mechanical enforcement (lint ratchets)

The taste calls are written guidance; the rules with mechanical backing are
enforced as **ratchets** — clean on existing code, enforced on new/changed code
— via `[workspace.lints]` / clippy / CI (`cargo clippy --locked --all -- -D
warnings`):

| Rule | Lint / mechanism |
|---|---|
| Ban bare `.unwrap()` → `.expect("invariant: …")` | `clippy::unwrap_used`; storage-hotspot migration landed separately |
| Long new functions | `clippy::too_many_lines` (soft ratchet) |
| Truncating `as` casts on lengths / offsets | cast lint ratchet |
| Depth-bounded untrusted input | generalized depth cap (RQL parse paths) |

## See also

- `STYLE.md` (repo root) — the day-to-day rules, in full.
- ADR 0056 — adopted subset, rejected rules, and enforcement.
- ADR 0046 / ADR 0054 — the in-house data-path / format dependency stance.
- `AGENTS.md` — points agents at both `STYLE.md` and ADR 0056.
