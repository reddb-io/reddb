# `storage/transaction` — live snapshot and visibility support

This module is the live home for transaction isolation types, snapshot
allocation, and MVCC visibility. It is not the retired transaction
coordinator.

## Module layout

- `mod.rs` — live `IsolationLevel` plus exports for snapshot and visibility.
- `snapshot.rs` — `SnapshotManager`, xid allocation, transaction contexts,
  savepoint sub-xids, commit/abort bookkeeping.
- `visibility.rs` — pure MVCC visibility predicate used by the runtime.
- `coordinator.rs`, `lock.rs`, `savepoint.rs`, `log.rs` — retired
  scaffolding, gated out of the normal build with notes pointing at ADR 0065.

## Invariants

### 1. SnapshotManager is the live transaction authority

The SQL runtime creates `TxnContext` values from `SnapshotManager`. That
manager allocates transaction xids, publishes snapshots, records committed
and aborted xids, and tracks the savepoint sub-xids attached to an open
transaction.

Runtime transaction control flows through `QueryExpr::TransactionControl` in
`src/runtime/impl_core.rs`; production code must not instantiate the retired
`coordinator::TransactionManager`.

### 2. IsolationLevel lives with the live transaction module

`storage::transaction::IsolationLevel` is the runtime isolation enum. The SQL
parser still produces the RQL AST enum inside `BEGIN ISOLATION LEVEL ...`;
runtime dispatch converts that parsed value into the live enum before storing
it in `TxnContext`.

### 3. Commit conflicts are optimistic first-committer-wins checks

The live engine does not promote the retired pessimistic coordinator. Writes
are checked at commit time by runtime FCW conflict checks such as
`check_table_row_write_conflicts`, and retryable serialization conflicts are
reported from that path.

Because the transaction protocol is optimistic, it has no transaction lock
waits and no transaction deadlock detector. The dispatch-time intent-lock
adapter under `runtime::locking` is separate runtime infrastructure for
collection/DDL coordination, not the TM coordinator described by ADR 0065.

### 4. Savepoints are sub-xids

`SAVEPOINT`, `RELEASE SAVEPOINT`, and `ROLLBACK TO SAVEPOINT` are live. A
savepoint allocates a sub-xid under the current `TxnContext`; rolling back to
a savepoint aborts that sub-xid and any nested sub-xids, then revives their
tombstones. Releasing a savepoint pops it without aborting its writes.

### 5. Autocommit statements still use the live runtime path

Statements outside an explicit transaction behave as autocommit operations.
They use the same runtime execution machinery and conflict/durability
boundaries as explicit transactions; they do not route through the retired
coordinator.

## Retired scaffolding

ADR 0065 retires, but does not delete, the older coordinator stack:

- `coordinator.rs` — dormant `TransactionManager` / `MvccCoordinator`.
- `lock.rs` — transaction lock manager with wait-for graph and deadlock
  detection.
- `savepoint.rs` — duplicate coordinator savepoint manager.
- `log.rs` — duplicate coordinator transaction log.

Those files are excluded from the normal build in `mod.rs`. Their embedded
tests retire with them.

## Anti-patterns to avoid

- Importing or ungating `coordinator::TransactionManager` for production
  runtime work.
- Reintroducing a transaction lock manager to implement isolation semantics
  without reopening ADR 0065.
- Adding another xid allocator for live MVCC state. Use `SnapshotManager`.

## See also

- ADR 0065: `.red/adr/0065-transaction-manager-v2-rewrite.md`
- Live runtime transaction control: `src/runtime/impl_core.rs`
- Live snapshot context lifecycle: `src/runtime/mvcc_lifecycle.rs`
- Visibility predicate: `src/storage/transaction/visibility.rs`
