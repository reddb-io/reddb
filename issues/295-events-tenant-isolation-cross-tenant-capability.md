# Events: tenant isolation + cross-tenant capability [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/295

Labels: needs-triage

GitHub issue number: #295

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Force tenant scope em events + adiciona capability `events:cluster_subscribe` para subscription cross-tenant.

End-to-end:
- Engine sempre injeta `tenant` no payload baseado em `EffectiveScope` da mutation.
- Subscription cria queue per-tenant: tenant `acme` cria `acme/users_events`, tenant `globex` cria `globex/users_events`.
- Cross-tenant subscription: `CREATE EVENT SUBSCRIPTION cross_audit ON ALL TENANTS users TO global_audit REQUIRES CAPABILITY 'events:cluster_subscribe'`.
- Sem capability + ALL TENANTS → erro 403.
- Tenant isolation tests: tenant A INSERT nunca aparece em tenant B's queue.

## Acceptance criteria

- [ ] Tenant `acme` INSERT em users → evento em `acme.users_events` (não em `globex.users_events`).
- [ ] Cross-tenant DDL sem capability → 403.
- [ ] Cross-tenant DDL com `cluster:admin + events:cluster_subscribe` → captura tenants.
- [ ] Conformance: 3 casos.

## Blocked by

- #292
