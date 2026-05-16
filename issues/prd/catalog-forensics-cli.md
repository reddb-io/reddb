# PRD: Catalog forensics CLI

GitHub: https://github.com/reddb-io/reddb/issues/468
ADR: docs/adr/0018-tiered-storage-layout.md
Related PRD: issues/prd/tiered-file-layout.md

## Problem

RedDB persists logical catalog state as a sequence of journal snapshots named
`seq-N` inside the support directory. These snapshots cost steady I/O on every
catalog mutation but offer almost no user-facing value today: there is no
supported way to list them, read them, diff one against another, or recover a
prior catalog from them. Operators who notice the files cannot tell whether
they are safe to delete, useful for incident response, or load-bearing for
recovery.

The tiered layout PRD (issues/prd/tiered-file-layout.md) and ADR 0018 already
plan to make `seq-N` snapshots opt-in outside the `max` tier. Before that
change, the snapshots that do exist need first-class read-only tooling so the
forensic value matches the I/O cost, and so the eventual opt-in default is a
real product tradeoff instead of dropping an invisible capability.

## Product Goal

Expose a small, read-only `reddb catalog ...` CLI surface that turns existing
`seq-N` journal snapshots into operational signal: history, point-in-time
inspection, diffs between snapshots, and an explicit, guarded restore path.
Output must be scriptable JSON by default with a human-readable TTY rendering.

## Scope

This PRD covers:

- A new `reddb catalog` CLI command group.
- A read-path module boundary that parses `seq-N` snapshots into an in-memory
  catalog view without mutating engine state.
- A diff module that compares two catalog views and produces a structured
  change set.
- A history module that lists known snapshots with metadata derived from the
  files alone.
- A restore planner that proposes the mutations needed to make the live
  catalog match a chosen snapshot, gated behind explicit confirmation and a
  validation pass that runs before any write.

This PRD does not cover:

- Changing when `seq-N` snapshots are written (owned by the tiered layout PRD).
- Changing the on-disk byte format of snapshots (owned by ADR 0003).
- Online catalog hot-restore without operator confirmation.
- Forensics over WAL, pages, or indexes (separate future PRDs).

## CLI Surface

```
reddb catalog history   [--data-path <path>] [--json|--text] [--limit <n>]
reddb catalog inspect   --seq <N>            [--data-path <path>] [--json|--text]
reddb catalog diff      --from <N> --to <M>  [--data-path <path>] [--json|--text]
reddb catalog restore   --seq <N>            [--data-path <path>] [--dry-run]
                                              [--confirm <token>]
```

Behavior contracts:

- `history`: lists `seq-N` snapshots discovered on disk, newest first. Each
  entry reports `seq`, file path, byte size, modification time, and a content
  fingerprint. Read-only.
- `inspect`: parses one snapshot into a stable, schema-versioned JSON view of
  the catalog at that point in time. Read-only.
- `diff`: computes a structured change set between two snapshots: added,
  removed, and modified catalog entries with field-level deltas where
  meaningful. Read-only.
- `restore`: always runs the restore planner; without `--confirm`, exits after
  printing the plan. `--dry-run` forces plan-only output even if a confirm
  token is supplied. Mutation only happens when planner validation passes and
  an explicit, single-use confirm token is provided.

The default output is JSON. `--text` renders a TTY-friendly view. Exit codes:
`0` success, `2` missing or unreadable snapshots, `3` planner validation
failure, `4` confirm token mismatch.

## Safety Boundaries

- All commands are read-only by default; `restore` is the only mutating verb
  and is gated.
- `restore` must refuse to run when the data directory is open by another
  process holding the engine lock.
- `restore` must validate the proposed plan against the live engine state
  before applying any mutation. Any inconsistency aborts the run with a
  structured error and a non-zero exit code.
- The confirm token must be derived from the target snapshot fingerprint and
  current live fingerprint, so reusing a token across two different runs
  fails.
- No command writes new `seq-N` files; output that augments the on-disk state
  must go through the normal catalog write path inside `restore`.

## Module Boundaries

The implementation slice will introduce focused modules:

- Journal reader: pure parser from `seq-N` bytes to an in-memory catalog view.
- Catalog diff: pure comparison between two catalog views.
- Catalog history: filesystem-only enumeration and metadata extraction.
- Restore planner: derives the mutation list from a target view and the live
  catalog; performs validation; produces a structured plan plus a confirm
  token.
- CLI surface: argument parsing, output formatting, and exit codes.

The CLI surface depends on the other modules but does not embed parsing or
diffing logic. Each module must be testable without spawning a binary.

## Output Expectations

- JSON output is the contract surface. It must be schema-versioned and stable
  enough to script against; breaking changes require an ADR.
- TTY output is convenience-only and is not part of the stability contract.
- Errors are emitted as structured JSON when `--json` is active, including
  exit-code-equivalent error categories.

## Testing Strategy

- Fixture-driven parser tests over recorded `seq-N` blobs, including
  malformed and truncated inputs.
- Pure diff tests covering add, remove, modify, and reordering invariants.
- History enumeration tests over synthetic directory layouts.
- Restore planner tests that assert validation failure modes without
  performing live writes.
- CLI smoke tests over the four verbs that assert exit codes, JSON shapes,
  and that `restore` without `--confirm` never mutates state.
- A focused end-to-end test that performs a guarded restore against a
  temporary engine instance and verifies the live catalog matches the
  selected snapshot.

## Non-goals

- Replacing the eventual opt-in default decision from the tiered layout PRD.
- Adding write verbs beyond `restore`.
- Changing the `seq-N` byte format.
- Hot/online catalog mutation without an operator confirm step.
- Building a generic snapshot-management UI; this is a focused forensics
  surface.

## Acceptance Criteria

- The repository has a durable PRD artifact for the catalog forensics CLI.
- ADR 0018 and the tiered file layout PRD are linked from this PRD and remain
  authoritative for layout decisions.
- Future implementation issues can be validated against this command surface,
  module boundary list, and safety contract.
- This PRD slice does not change runtime behavior.
