# Triage Labels

Canonical label vocabulary and full issue lifecycle. This is the single source of truth — `/triage`, `/afk`, `/to-issues`, and `/to-prd` all reference this file.

## Label Mapping

The skills speak in terms of canonical triage roles. Map them here to the actual label strings used in this repo's issue tracker.

| Canonical role     | Label in our tracker | Applied by                            | Removed by                          |
| ------------------ | -------------------- | ------------------------------------- | ----------------------------------- |
| `needs-triage`     | `needs-triage`       | `red-issues-needs-triage` workflow, `/triage` | `/triage` (when state transitions) |
| `needs-info`       | `needs-info`         | `/triage`                             | `/triage` (when reporter replies)   |
| `ready-for-agent`  | `ready-for-agent`    | `/triage`, `/to-issues`               | `/afk` (when claiming)              |
| `running`          | `running`            | `/afk` (when claiming an issue)       | `/afk` (on close, blocker, or release) |
| `ready-for-human`  | `ready-for-human`    | `/triage`, `/afk` (on blocker)        | maintainer                          |
| `wontfix`          | `wontfix`            | `/triage` (then close)                | rarely — usually issue closes       |
| `needs-slicing`    | `needs-slicing`      | `/to-prd` (on publish)                | `/to-issues` (when slices are created) |
| `type:prd`         | `type:prd`           | `/to-prd` (on publish)                | never — type marker, permanent       |

Edit the right-hand column to match whatever vocabulary you actually use.

## Full Lifecycle

Every issue moves through this state machine. Arrows show the transitions; the actor on each arrow is the skill or workflow responsible.

```
                       ┌─────────────────────┐
                       │   issue created     │
                       │   (any source)      │
                       └──────────┬──────────┘
                                  │
              red-issues-needs-triage workflow
              (auto on opened/reopened, no label)
                                  ▼
                       ┌─────────────────────┐
        ┌─────────────│    needs-triage     │────────────┐
        │              └──────────┬──────────┘            │
        │                         │                       │
   /triage:                  /triage:                /triage:
   needs-info               wontfix                  ready-for-*
        │                         │                       │
        ▼                         ▼                       │
┌──────────────┐           ┌──────────────┐               │
│ needs-info   │           │   wontfix    │               │
│ (await user) │           │   + close    │               │
└──────┬───────┘           └──────────────┘               │
       │                                                  │
  reporter replies                                        │
   → /triage                                              │
       │                                                  │
       └──────────────────► needs-triage                  │
                                                          ▼
                              ┌───────────────────────────────────┐
                              │                                   │
                              ▼                                   ▼
                  ┌──────────────────────┐         ┌──────────────────────┐
                  │   ready-for-agent    │         │   ready-for-human    │
                  │   (AGENT-BRIEF       │         │   (needs judgment)   │
                  │    posted)           │         └──────────────────────┘
                  └──────────┬───────────┘                     │
                             │                                 │
                       /afk claim:                       human picks up
                       removes ready-for-agent,          (manual impl,
                       adds running                       eventually closes)
                             │
                             ▼
                  ┌──────────────────────┐
                  │       running        │
                  │  (worktree active,   │
                  │   heartbeats post)   │
                  └──────────┬───────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
        /afk: DONE      /afk: BLOCKED   /afk: merge conflict
              │              │              │
              ▼              ▼              ▼
        ┌─────────┐   ┌────────────────────────────┐
        │ closed  │   │ remove running,            │
        │ + merge │   │ add ready-for-human,       │
        │ comment │   │ worktree preserved         │
        └─────────┘   └────────────────────────────┘
```

## State Definitions

### `needs-triage`
Maintainer has not evaluated the issue yet. **Applied automatically** by `red-issues-needs-triage.yml` workflow on every fresh `opened`/`reopened` issue with no labels. Manual application by `/triage` when the maintainer puts an evaluated issue back into the queue. Removed by `/triage` when the issue transitions to a definitive state.

### `needs-info`
The triage agent or maintainer needs more information from the reporter before a decision can be made. Removed by `/triage` once the reporter responds and the issue cycles back through `needs-triage`.

### `ready-for-agent`
The issue has a complete AGENT-BRIEF posted as a comment (see `triage/AGENT-BRIEF.md`). Body and brief together form a contract sufficient for an AFK agent to implement without human context. **This is the only state `/afk` consumes.** Applied by `/triage` (after grilling) or `/to-issues` (on creation when the slice is AFK-safe).

### `running`
`/afk` has claimed the issue and is actively executing it. Applied atomically with the removal of `ready-for-agent` so two parallel `/afk` runs cannot race on the same issue. The orchestrator's heartbeat sub-shell posts `:one:` → `:four:` comments every 10 min while this label is present. Removed on close (success), on blocker (replaced with `ready-for-human`), or on graceful release (if the user interrupts the loop).

### `ready-for-human`
The issue requires human implementation. Two sources: `/triage` decides it during evaluation (e.g. architectural call, design review needed), or `/afk` promotes it from `running` after a blocker (inner agent gave up, merge conflict couldn't be auto-resolved, both runners exhausted). When `/afk` promotes, the worktree is **preserved at the moment of blocker** so the human can pick up in place.

### `wontfix`
Will not be actioned. Applied by `/triage`. For bugs, paired with a polite explanation and close. For enhancements, paired with a `.out-of-scope/*.md` entry (see `triage/OUT-OF-SCOPE.md`).

### `type:prd`
Permanent type marker for PRD issues created by `/to-prd`. A PRD is a planning artifact, **not an implementable slice** — it describes *what* to build at the product level and must be split into child issues by `/to-issues` before any agent can execute. `/afk` hard-filters issues carrying `type:prd` from its candidate list even if `ready-for-agent` was applied by mistake. Never remove this label.

### `needs-slicing`
The PRD has been published but `/to-issues` has not yet split it into child slices. Applied by `/to-prd` on publish, removed by `/to-issues` once at least one child issue with `prd:{N}` exists. `/afk` counts these in its straggler check so a forgotten PRD surfaces before the loop runs dry.

## Heartbeat Comments

While `running`, `/afk` posts a heartbeat comment every 10 minutes so the issue is never silent during long executions:

```
t=10 min  →  :one:
t=20 min  →  :two:
t=30 min  →  :three:
t=40 min  →  :four:
t=50 min  →  :one:   (cycle resets)
```

Stops on any terminal transition out of `running`.

## Optional Auxiliary Labels

These exist for filtering and don't drive lifecycle transitions:

| Label          | Meaning                                         | Applied by                       |
| -------------- | ----------------------------------------------- | -------------------------------- |
| `bug`          | Something is broken                             | `/triage`                        |
| `enhancement`  | New feature or improvement                      | `/triage`                        |
| `priority:high` | Urgent / high-impact — `/afk` drains these first | `/triage` or maintainer        |
| `priority:low`  | Everything else                                  | `/triage` or maintainer        |
| `prd:{N}`      | Issue belongs to PRD #N                         | `/to-issues` when splitting a PRD |
| `slice:hitl`   | Slice that needs human-in-the-loop              | `/to-issues`                     |
| `slice:afk`    | Slice that can run unattended                   | `/to-issues`                     |

## Naming Convention

All labels follow one of two shapes:

- **kebab-case** — `needs-triage`, `ready-for-agent`, `running`, `wontfix`, `bug`.
- **`prefix:value`** — `priority:high`, `slice:afk`, `prd:42`.

No uppercase, CamelCase, snake_case, or spaces. GitHub matches labels case-insensitively for filtering but stores the case you create them with — keep the tracker clean by normalising on creation. `/setup-red-skills` surfaces non-conforming labels and offers to rename via `gh label edit "Old Name" --name "new-name"`.
