# RedDB Style

House engineering style for reddb, adapted from TigerBeetle's
[TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md).
We adopt a deliberately-scoped subset — what fits a correctness-first, general-purpose
database engine written in Rust. The rules we consciously **rejected** (and why) are
recorded in [ADR 0056](.red/adr/0056-tigerstyle-house-style.md); read it before assuming a
TigerBeetle rule applies here.

Design goals, in order: **safety, performance, developer experience.** Good style advances
these goals; readability is table stakes, not the end in itself.

These rules apply to **new and changed code**. They are not a mandate to retroactively
rewrite the tree — match surrounding code when editing (see CLAUDE.md §3, Surgical Changes).

## 1. Errors: two kinds of failure

Distinguish them at every call site:

- **Operating errors** — disk, network, corrupt data, resource exhaustion. These are
  *expected* and **must be handled** (propagate a `Result`). An analysis of production
  failures found the majority of catastrophic outages came from mishandled non-fatal errors.
- **Programmer errors** — broken invariants. These are *unexpected*; the only correct
  response is to **crash**. Encode them as `assert!`/`expect`, which downgrades a silent
  correctness bug into a loud liveness bug.

A bare `.unwrap()` blurs the two. **Ban it** — write `.expect("invariant: …")` that states
*why* the value cannot be absent, or propagate the error if it actually can. This is
mechanically enforced as a ratchet (`clippy::unwrap_used`); `expect` with a reason is the
sanctioned replacement.

## 2. Assertions

reddb already keeps assertions live in release (`assert!`, not `debug_assert!`) — that is the
default and it stays. The expensive, valuable part (invariant checks protecting production) is
already paid for; keep it.

- `assert!` (stays in release) is the **default**. Use `debug_assert!` **only** in a
  *measured* hot inner loop where the check is shown to cost.
- **Pair assertions** — enforce a property on at least two code paths (e.g. validate right
  before writing to disk *and* immediately after reading back).
- Assert the **positive space** you expect **and** the **negative space** you don't — bugs
  hide where data crosses the valid/invalid boundary.
- Assert **compile-time** relationships (type sizes, layout, constant invariants) where they
  matter — they check design integrity before the program runs.

No numeric per-function assertion count is mandated; placement and quality matter, not tally.

## 3. Function shape

Centralize control flow: **push `if`s up, push `for`s down.** Keep all branching in the parent
function and move non-branchy fragments to pure leaf helpers. The parent owns state; helpers
compute what changes, they don't reach out and mutate.

There is a real discontinuity between a function that fits on a screen and one you must scroll.
A soft lint (`clippy::too_many_lines`, ratcheted) nudges new functions that run long; treat it
as a prompt to split, not a hard wall — idiomatic `match`/builder code legitimately runs longer
than a Zig 70-line limit would allow.

## 4. Naming (new code)

- `snake_case` for functions, variables, files (clippy enforces this).
- **No abbreviations.** Long-form, descriptive names. Long-form flags in scripts (`--force`).
- **Units/qualifiers last, by descending significance:** `latency_ms_max`, not `max_latency_ms`
  — so `latency_ms_min` lines up beside it and related variables group and align in arithmetic.
- A helper carries its **caller's name** to show the call history: `read_sector` →
  `read_sector_callback`.
- Prefer **nouns over participles**: `replica.pipeline`, not `replica.preparing` — nouns compose
  into derived identifiers (`config.pipeline_max`) and read cleanly in prose.
- **File order:** the important thing first (top-down read). For structs: fields, then nested
  types, then methods.

## 5. Off-by-one discipline

`index` (0-based), `count` (1-based), and `size` (count × unit) are conceptually **distinct
types**, even though they are all integers. Going index → count adds one; count → size
multiplies by the unit. Including the unit in the name (rule 4) is what makes this checkable.

- Show **division intent**: `div_ceil` / `div_floor` / `@divExact`-equivalent, not a bare `/`.
- Prefer **checked/explicit conversions** over silent `as` truncation on lengths and on-disk
  offsets. A cast lint (ratcheted) surfaces truncating casts.

Note: we keep `usize` for indexing and lengths — it is the correct Rust type. TigerBeetle's
"avoid `usize`" rule does **not** apply here (see ADR 0056).

## 6. Performance: design-time first

The biggest performance wins are won in design, before anything can be profiled. "The lack of
back-of-the-envelope performance sketches is the root of all evil."

- Sketch across the four resources — **network, disk, memory, CPU** — and their two
  characteristics, **bandwidth** and **latency**. Optimize the slowest first, after weighting by
  how often it runs (a frequent memory miss can cost as much as one disk fsync).
- Separate the **control plane** from the **data plane**; **batch** the data plane so the CPU
  gets large, predictable chunks of work instead of zig-zagging per event.
- On hot paths, don't allocate per operation — pre-size, pool, and reuse buffers (the
  transferable half of TigerBeetle's static-allocation rule; the wholesale rule is rejected, see
  ADR 0056). The control plane stays idiomatic.

**Requirement:** a PRD or ADR that changes the **data plane** (WAL, pager, query execution)
must carry a one-line back-of-the-envelope sketch (e.g. "N rows × fsync latency = …"). It is
nearly free and it is the single highest-leverage habit here.

## 7. Untrusted input is bounded

Every parse/expansion path over untrusted input (RQL especially) must carry an **explicit depth
cap** — unbounded nesting is a stack-overflow DoS. Recursion is fine *when depth-bounded*; we do
not ban it. Generalize the existing `JSON_LITERAL_MAX_DEPTH` pattern rather than inventing a new
guard per call site.
