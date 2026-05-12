# Document or eliminate the 101-id offset for user entities [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/421

Labels: needs-triage

GitHub issue number: #421

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement — UX / docs

## Symptom

First user-inserted entity gets `_entity_id 102`. The first 101 ids are reserved for collection metadata (column descriptors etc.). Stable across `memory://` and `file://`, but undocumented. Users discover empirically.

## What to do (pick one or combine)

- Document the offset in `docs/data-models/graphs.md` and `docs/engine/file-format.md` with a clear "first user id is 102" note.
- Expose a `_first_user_id` collection property the SDK can read.
- Best: allocate internal entities in a separate id space so user ids start at 1.

## Acceptance criteria

- [ ] User-facing docs explain the id space (current behavior or new behavior — must be coherent).
- [ ] If behavior changes (option C), `_entity_id` starts at 1 for user inserts; migration path for existing files documented.
- [ ] Acceptance tests pin the first user id behavior.
