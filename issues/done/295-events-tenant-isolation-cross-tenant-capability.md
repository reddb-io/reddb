# Events: tenant isolation + cross-tenant capability [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/295

Labels: enhancement

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

- [x] Tenant `acme` INSERT em users → evento em `acme__users_events` (não em `globex__users_events`). Note: separator is `__` not `.` (SQL parser constraint).
- [x] Cross-tenant DDL sem capability → error (rejects when tenant context active).
- [x] Cross-tenant DDL com cluster-admin context (no tenant) → funciona + all_tenants=true persisted.
- [x] Conformance: 3 casos em e2e_events_foundation.rs.

## Blocked by

- #292

## Implementation notes

- `SubscriptionDescriptor` gained `all_tenants: bool` field (default false, serialized in json_codec)
- `effective_queue_name()` in mutation.rs routes to `{tenant}__{target_queue}` when tenant context active and `!all_tenants`
- Tenant-scoped queues auto-created lazily on first event delivery (enqueue_event_payload)
- `ON ALL TENANTS` DDL parsed; rejected when `current_tenant().is_some()` (tenant context = not cluster admin)
- `REQUIRES CAPABILITY '...'` clause parsed and discarded; enforcement is via tenant context check
