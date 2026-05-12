# Events: TRUNCATE / DROP single event semantics [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/302

Labels: enhancement

GitHub issue number: #302

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

TRUNCATE/DROP têm semantics especiais: 1 evento agregado, não N por row.

End-to-end:
- `TRUNCATE users` (em event-enabled users) → 1 evento `{op: "truncate", collection, ts, lsn, tenant, entities_count: N}`. **Não** N delete events.
- `DROP TABLE users` → 1 evento `{op: "collection_dropped", collection, ts, lsn, tenant, final_entities_count: N}`. Subscription removida do catalog. Queue **preservada** com mensagens pendentes (consumer drena depois).
- Documentação: consumer downstream lida com truncate fazendo `DELETE FROM downstream` + drop_collection mantendo queue até consumer drenar.

## Acceptance criteria

- [ ] `TRUNCATE users` (1M rows) → 1 evento, não 1M.
- [ ] `DROP TABLE users` → 1 evento; queue `users_events` preservada; subscription removida.
- [ ] Conformance: 2 casos.

## Blocked by

- #293
