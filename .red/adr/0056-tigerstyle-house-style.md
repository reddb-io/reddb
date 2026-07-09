# ADR 0056 — Adopt a TigerStyle-derived house style

Status: accepted
Date: 2026-06-19

## Decision

We adopt a deliberately-scoped subset of TigerBeetle's
[TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md) as
reddb's house style. The day-to-day rules live in [`STYLE.md`](../../STYLE.md) at the repo
root and are registered in `CLAUDE.md`. The rules that have real mechanical backing are
enforced through `[workspace.lints]` / clippy / CI as **ratchets** (clean on existing code,
enforced on new/changed code); the taste calls are written guidance only.

This ADR exists primarily to record **what we rejected and why** — `STYLE.md` says what to do,
not what we consciously chose *not* to do. A future contributor will reasonably ask why reddb
declined TigerBeetle's signature rules; the answer is below.

## Context

reddb is a correctness-first, general-purpose database engine written in Rust. TigerStyle was
written for TigerBeetle: a fixed-schema accounting ledger in Zig with bounded state. Much of its
wisdom is language- and domain-agnostic and transfers directly; a few of its signature rules are
specific to Zig or to a fixed-schema ledger and would fight Rust and our domain for no gain.

Baseline at adoption: reddb already matched TigerStyle on the hard parts — `max_width = 100`,
and ~15,600 release-live `assert!` (vs only ~60 `debug_assert!`). It diverged on ~4,900
non-test `.unwrap()` calls concentrated in the storage engine, inconsistent depth-bounding of
untrusted input, and the absence of any `[workspace.lints]`.

## Adopted (see STYLE.md for the rules)

1. **Two kinds of failure.** Operating errors are handled; programmer errors crash. Bare
   `.unwrap()` is banned in favour of `.expect("invariant: …")`, enforced as a ratchet.
2. **Assertions stay live in release** (already true) plus quality patterns: pair assertions,
   positive + negative space, compile-time asserts. No numeric per-function count.
3. **Function shape** — push `if`s up, `for`s down; soft `too_many_lines` ratchet (~100–120),
   not a hard limit.
4. **Naming (new code)** — units last by descending significance, no abbreviations,
   helper-carries-caller's-name, nouns over participles, file order.
5. **Off-by-one discipline** — index/count/size as distinct quantities; explicit division
   intent; checked casts over silent `as` (cast lint ratchet).
6. **Performance is design-time** — back-of-the-envelope sketches across network/disk/memory/CPU;
   control vs data plane; batch the data plane; pre-size/pool buffers on hot paths. A data-plane
   PRD/ADR must carry a one-line sketch.
7. **Untrusted input is depth-bounded** — generalize `JSON_LITERAL_MAX_DEPTH`; recursion allowed
   when bounded.

## Rejected, and why

- **"All memory statically allocated at startup; no dynamic allocation after init."** Rejected
  wholesale. It works for a fixed-schema ledger with bounded state; reddb has arbitrary
  collections, variable-size rows, RQL query plans, and a B-tree that grows. We adopt only its
  *spirit* on hot paths (don't allocate per operation — pool and reuse). Adopting it broadly would
  fight the borrow checker and the domain for no real gain.

- **"Zero dependencies."** Rejected as a literal rule — we are not reimplementing tokio, axum,
  hyper, or rustls. reddb already practices the defensible version: data-path primitives and
  wire/file/crypto/JSON formats are homed in-house (see ADR 0046, ADR 0054) while infrastructure
  rides vetted crates. We deliberately did **not** re-codify a dependency policy here; it is
  governed by those ADRs.

- **"Use explicitly-sized types like `u32`; avoid `usize`."** Rejected. In Rust, `usize` is the
  correct type for indexing and lengths (`Vec::len` → `usize`; slice indexing requires it);
  avoiding it means truncating casts everywhere for no safety gain. We keep the durable kernel —
  the index/count/size discipline and explicit casts — which is what actually prevents on-disk
  offset corruption.

- **Hard 70-line function limit.** Rejected as a hard limit; adopted as a soft ratchet at a
  Rust-realistic threshold (~100–120). 70 lines is calibrated for Zig; idiomatic Rust `match` and
  builder code runs legitimately longer, so a hard 70 would rain false positives. The intent
  ("doesn't fit on a screen") is preserved by the soft lint plus the function-shape principle.

- **"No recursion."** Reframed, not adopted literally. The real safety property is *bounded depth
  on untrusted input*, not the absence of recursion. Recursive-descent parsing is natural; we
  require an explicit depth cap instead of converting it to explicit-stack iteration.

  *Amended 2026-07-08 (ADR 0073 §6):* in the **storage/recovery data plane** the reframing is
  hardened back to the literal rule — recursion is prohibited there (explicit stacks with
  budget-sized capacity), because a stack overflow during recovery is a data-loss event, not a
  crash. Parser/planner/query surfaces keep the depth-bounded rule above unchanged.

## Enforcement

CI already runs `cargo clippy --locked --all -- -D warnings`, so any `warn`-level lint becomes a
hard global error. New lints (`unwrap_used`, `too_many_lines`, cast lints) must therefore land as
**ratchets**: a `[workspace.lints]` deny plus crate-level `#![allow(...)]` on legacy crates,
peeled back over time, so the gate bites on new/changed code without failing CI on the existing
tree. The lint slices and the storage-hotspot migration are tracked under PRD #1252.

## Consequences

- A citable house style and decision record; reviewers and AFK agents have one reference.
- The rejections survive — future contributors understand they were trade-offs, not oversights.
- The mechanical gates roll out incrementally via ratchets (PRD #1252 children), so adoption does
  not require a tree-wide rewrite up front.
