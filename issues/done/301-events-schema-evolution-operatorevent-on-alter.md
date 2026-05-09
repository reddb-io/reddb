# Events: schema evolution OperatorEvent on ALTER [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/301

Labels: needs-triage

GitHub issue number: #301

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#284

## What to build

Quando `ALTER TABLE` adiciona/remove colunas em event-enabled collection, emite OperatorEvent (audit log) alertando que payload pode mudar shape.

End-to-end:
- `ALTER TABLE users ADD COLUMN phone TEXT` em users com subscriptions → OperatorEvent `subscription_schema_change` com:
  - `collection`, `subscription_names`, `fields_added`, `fields_removed`, `lsn`.
- Audit log via existing AuditLogger.
- Default behavior (per grilling decision a): payload subsequente inclui field novo. Sem opt-in versioning ainda.

## Acceptance criteria

- [x] `ALTER TABLE users ADD COLUMN x` em event-enabled users → OperatorEvent emitida.
- [x] OperatorEvent contém collection, subscriptions afetadas, diff de fields.
- [x] DROP COLUMN também emite.
- [x] ALTER que não toca columns (ex: ENABLE RLS) não emite.
- [x] Conformance: 2 casos.

## Blocked by

- #292 (done)

## Delivery notes

- `OperatorEvent::SubscriptionSchemaChange` added to `telemetry/operator_event.rs`
- Emitted via `emit_global()` in `execute_alter_table` (impl_ddl.rs) when ADD/DROP COLUMN ops run on a collection with enabled subscriptions
- Non-column ops (ENABLE RLS, etc.) are silent
- 2 conformance tests added: `alter_add_column_on_event_enabled_table_succeeds`, `alter_drop_column_and_rls_on_event_enabled_table_succeeds`
- `cargo check` clean, both tests pass
