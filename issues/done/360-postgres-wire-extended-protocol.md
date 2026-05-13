# Postgres-wire: Parse/Bind/Describe/Execute/Close extended protocol [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/360

Labels: hitl

GitHub issue number: #360

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Implement the Postgres-wire extended query protocol: `Parse`, `Bind`, `Describe`, `Execute`, `Close`. Removes the limitation noted in `docs/api/postgres-wire.md:141-142`. Standard Postgres clients (`psycopg`, `pgx`, `pg-promise`, JDBC) using prepared statements work without falling back to simple-query mode.

Parameter parsing maps Postgres OIDs to engine `Value` types. Reuses the same binder as other transports — no duplicate logic.

HITL because protocol decisions (OID coverage, error code mapping, statement/portal lifecycle, parameter format codes binary vs text) merit explicit review before merging.

## Acceptance criteria

- [ ] `Parse` / `Bind` / `Describe` / `Execute` / `Close` implemented per Postgres protocol spec.
- [ ] OID mapping documented for: int2/int4/int8, text/varchar, bool, float4/float8, bytea, json/jsonb, timestamp(tz), uuid, vector (extension OID — to decide).
- [ ] Both binary and text parameter format codes supported.
- [ ] Integration tests against `psycopg` (Python), `pgx` (Go), and JDBC issuing prepared SELECT, INSERT, and vector queries.
- [ ] Tests assert clients did NOT fall back to simple-query mode.
- [ ] `docs/api/postgres-wire.md` updated to remove the unimplemented-extended-protocol caveat.

## Blocked by

- #352
- #353
