# `SELECT *` default returns user columns only; `SELECT * WITH METADATA` opt-in [HITL]

Labels: enhancement, needs-triage

## HITL instruction

This issue requires human review before implementation. The change alters the default behavior of `SELECT *` and may break callers that depend on the current metadata leakage.

## Parent

#445

## What to build

`SELECT *` today returns ~8 internal columns alongside the user-declared ones: `red_entity_id`, `red_collection`, `red_kind`, `red_sequence_id`, `red_capabilities`, `red_entity_type`, `created_at`, `updated_at`. This is useful for debugging but noisy for user-facing tables.

Change the default `SELECT *` to return only the user-declared columns. Add `SELECT * WITH METADATA` (or similar opt-in spelling) that includes the `red_*` / `created_at` / `updated_at` columns.

## Why HITL

This is a breaking change for any caller — internal or external — that depends on `red_entity_id` or other metadata columns being present in a `SELECT *`. Needs an architectural decision on:

- Should the change land behind a session flag (`SET include_metadata = on`) for a grace period before flipping the default?
- Should the opt-in spelling be `SELECT * WITH METADATA`, `SELECT ALL *`, or a pragma-style toggle?
- What is the migration path for internal tests and tools that currently rely on `red_*` columns being present?

## Acceptance criteria (after design call)

- [ ] Default `SELECT *` returns only user-declared columns.
- [ ] Opt-in form returns the full set including `red_*` and timestamps.
- [ ] Migration path is documented and applied to existing internal callers.
- [ ] Tests cover both forms; existing tests are updated rather than weakened.

## Blocked by

None - can start immediately, but blocked on design call before implementation.
