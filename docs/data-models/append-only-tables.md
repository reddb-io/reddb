# Append-Only Tables

Append-only tables accept `INSERT` but reject `UPDATE` and `DELETE` at
parse time. Use them for audit logs, event streams, ledger entries,
telemetry — any workload where immutability is a *correctness*
requirement, not a policy suggestion.

## Declaration

Two equivalent forms:

```sql
CREATE TABLE audit_log (
  id        BIGINT,
  actor     TEXT,
  action    TEXT,
  occurred_at BIGINT
) APPEND ONLY;

-- or, embedded in the WITH clause
CREATE TABLE events (
  id     BIGINT,
  kind   TEXT,
  data   JSONB
) WITH (append_only = true);
```

The flag is stored in the `CollectionContract` (see
[Collection Contract ADR](../architecture/collection-contract-adr.md))
and survives restart, backup, and replica sync.

## Semantics

| Statement                      | Behaviour on append-only table |
|--------------------------------|--------------------------------|
| `INSERT ... [RETURNING ...]`   | ✅ Accepted                    |
| `SELECT ...`                   | ✅ Accepted                    |
| `UPDATE ...`                   | ❌ Rejected at parse time      |
| `DELETE ...`                   | ❌ Rejected at parse time      |
| `DROP TABLE ...`               | ✅ Accepted (catalog operation)|

Error message example:

```
Error: table 'audit_log' is APPEND ONLY — UPDATE is rejected.
       Drop the APPEND ONLY clause (ALTER TABLE ... SET APPEND_ONLY = false)
       or insert a new row instead.
```

The guard runs **before** Row-Level Security, so an operator can see a
clear "APPEND ONLY" error even if the table has other RLS policies
attached.

## When to pick append-only vs alternatives

RedDB ships three append-oriented models. Pick based on how your data
shapes:

| Use case                         | Recommended model             |
|----------------------------------|-------------------------------|
| Time-indexed metrics / telemetry | `CREATE TIMESERIES`           |
| High-volume unstructured events  | Log Collections (`/logs/...`) |
| Schema-enforced immutable rows   | `CREATE TABLE ... APPEND ONLY`|

Decision rules:

- **Does the row have a dominant time axis and you need downsampling /
  retention / `time_bucket`?** → Time-series. Chunks, codecs (Delta +
  XOR), temporal index are all tuned for it.
- **Are the records arbitrary-shape text / JSON with no schema and
  you need sub-millisecond append rate?** → Log Collections. The HTTP
  surface `/logs/{name}/append` skips validation entirely.
- **Is it a typed row (audit entry, ledger row, signed fact) that
  still needs SQL joins, indexes, FK-like references?** → Append-only
  table. You keep every relational feature; the engine just blocks
  mutations.

## Interaction with other features

- **RLS**: `APPEND ONLY` runs first. A policy set that normally would
  have allowed UPDATE on your role is ignored — DML rejection is
  unconditional.
- **RETURNING**: `INSERT ... RETURNING` works as usual. `UPDATE
  RETURNING` and `DELETE RETURNING` inherit the APPEND ONLY
  rejection.
- **Partitions** (`PARTITION BY ...`): compatible. Each child
  partition inherits the parent's append-only declaration.
- **Tenant columns** (`TENANT BY ...`): compatible. Append-only
  doesn't constrain which tenant a row belongs to, only whether rows
  can change.
- **Timestamps** (`WITH timestamps = true`): the engine-populated
  `created_at` is written once on insert; `updated_at` is never
  updated in this table (you can still read its initial value).
- **Time-series** and **Queues**: declared append-only implicitly.
  Attempts to UPDATE / DELETE those collections fall through the same
  guard.

## Engine optimisations (future)

The append-only declaration unlocks several optimisations tracked as
follow-ons:

- Aggressive column codecs (Delta-of-Delta, Dictionary) without the
  cost of mid-segment rewrites
- Visibility bitmap can be dropped entirely — row set is monotonic
- LSM-style tiered compaction becomes safe without coordinating
  UPDATE paths
- WAL shipping for replicas is simpler (no UPDATE/DELETE record
  types need to be mirrored)

See the [TimescaleDB / ClickHouse parity plan](../architecture/distributed-roadmap.md)
for the roadmap.

## Altering the flag

`ALTER TABLE ... SET APPEND_ONLY = false` (and `= true`) is on the
roadmap. For now, recreate the table if you need to swap the flag;
`CREATE TABLE ... AS SELECT` and `INSERT INTO new SELECT FROM old` get
you there in three statements.
