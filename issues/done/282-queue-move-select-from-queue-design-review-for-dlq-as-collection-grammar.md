# QUEUE MOVE + SELECT FROM QUEUE: design review for DLQ-as-collection grammar [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/282

Labels: enhancement

GitHub issue number: #282

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## What to build

The landing docs at `rdb-lair/apps/landing/src/lib/data/data-types.ts` previously advertised two verbs that don't exist in the engine: `QUEUE MOVE FROM <a> TO <b> [WHERE …]` and `SELECT … FROM QUEUE <name>`. The landing copy is being corrected to match the real grammar (see reddb-io/red-lair#69), but the original UX (DLQ-as-queryable-collection + bulk replay back to live) is genuinely useful and worth designing properly.

This is a design / HITL issue because both verbs touch transaction semantics that need a design call before implementation:

1. **`QUEUE MOVE FROM <src> TO <dst> [WHERE …]`** — atomicity boundary between two queues. Is MOVE a single WAL record, a transactional pair (POP src + PUSH dst), or a cursor-driven streaming operation when WHERE matches many rows? What happens on partial failure mid-stream? Does WHERE see the same view of `src` for the whole operation (snapshot) or row-by-row?

2. **`SELECT … FROM QUEUE <name>` (read-only)** — exposing queue rows through the normal SELECT planner. Schema of the synthetic columns (`id`, `payload`, `priority`, `attempts`, `last_error`, `enqueued_at`?). Index/scan strategy. Does it consume the row, peek it, or expose a separate read-only projection? Interaction with consumer-group state.

The output of this issue is a design doc / ADR plus a follow-up implementation issue. No engine code lands here.

## Acceptance criteria

- [ ] Decision recorded (ADR or PRD) on MOVE atomicity boundary and WHERE evaluation semantics.
- [ ] Decision recorded on `SELECT FROM QUEUE` projection schema and whether it shares the SELECT planner or stays as a queue-only read path.
- [ ] DLQ-replay UX described end-to-end with the decided grammar (so landing docs can flip back if/when this lands).
- [ ] Follow-up implementation issue opened with parser + AST + executor scope.
- [ ] Note in the design doc on whether existing `QUEUE PEEK` covers enough of the use case to defer this indefinitely.

## Blocked by

None - can start immediately.
