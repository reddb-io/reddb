# PRD: Deepen correctness seams for MVCC, events, queues, and catalog

GitHub: https://github.com/reddb-io/reddb/issues/507

## Problem Statement

RedDB has correctness-critical behavior implemented through shallow or partially bypassed
seams. The engine already has the vocabulary for MVCC, WAL, `WITH EVENTS`, queues,
statement execution, catalog discovery, and wire translation, but too much correctness
still depends on callers arranging those pieces in the right order.

The risk is not a missing feature flag. The risk is that user-visible guarantees become
path-dependent:

- MVCC read resolution leaks snapshot visibility, tombstones, `AS OF` behavior, current-row
  fallback, write-set overlay, and index fallback into callers.
- Transaction commit behavior is spread across journals, pending tombstones, pending
  versioned updates, deferred store WAL capture, conflict checks, rollback revival, and
  snapshot publication.
- `WITH EVENTS` atomicity depends on DML wrappers, WAL capture, event shaping, filtering,
  redaction, tenant queue naming, queue backpressure, and DLQ routing lining up.
- Queue delivery and Queue retirement leak dispatch, config/meta persistence, lifecycle
  decisions, DLQ replay, and result shaping.
- The Statement frame is intended to be the single query lifecycle seam, but fast paths,
  prepared/direct paths, and Wire adapter entry points can bypass it.
- Catalog discovery is split between `CollectionDescriptor`, `red.*`, `SHOW`, docs, and
  wire-specific catalog handling.
- Postgres wire catalog handling partially materializes catalog views instead of
  consistently translating native RedDB catalog concepts.

This PRD turns those concerns into an implementation program. It does not claim the
program is already implemented.

## Goals

- Define the deep modules RedDB should converge on for correctness-sensitive behavior.
- Name the first implementation tranche as the MVCC read resolver for table-row visibility.
- Link the existing GitHub follow-ups for the first MVCC tranche instead of creating
  duplicate broad work.
- Keep non-MVCC areas as future split candidates until they have concrete tracer bullets.
- Preserve public behavior compatibility while hardening internal ownership boundaries.
- Give maintainers a repository-local artifact that explains why future slices should route
  through the same seams instead of adding another special case.

## Non-Goals

- This PRD does not implement MVCC, event, queue, statement-frame, catalog, or wire-adapter
  code.
- This PRD does not introduce a public syntax change.
- This PRD does not introduce a public schema change.
- This PRD does not introduce a disk-format change.
- This PRD does not introduce a wire-protocol change.
- This PRD does not require historical secondary indexes, serializable isolation,
  two-phase commit, or a new autovacuum daemon.
- This PRD does not claim full multi-model MVCC adoption beyond the explicitly split
  table-row resolver tranche.

Public compatibility expectation: no public syntax, schema, disk-format, or wire-protocol change is assumed. Any future slice that needs one of those changes must record it explicitly in its own issue or ADR.

## Deep Modules and Intended Interfaces

### MVCC read resolver

The MVCC read resolver is the single visibility decision point for user-data reads. Its
intended interface is conceptually:

```text
resolve_visible(collection, model_kind, logical_id, snapshot, write_set) -> visible item | none
```

It owns snapshot visibility, tombstones, `AS OF` and VCS-pinned reads, current-row
fallback, transaction write-set overlay, committed/aborted xid checks, and index fallback
decisions. Table scans, indexed candidates, logical row lookup, and DML target scans may
choose different candidate sources, but they must not return a row until the resolver has
made the visibility decision.

The first tranche covers table-row visibility only. Other models should keep their current
documented behavior until they adopt the resolver through their own slices.

### Transaction commit unit

The Transaction commit unit is the single ownership boundary for turning staged writes into
durable committed state. Its intended interface is conceptually:

```text
commit(staged_mutations, event_writes, queue_writes, scope, durability) -> committed xid | error
```

It should own conflict validation, pending tombstones, pending versioned updates, deferred
store WAL capture, history-store writes, index deltas, rollback cleanup, snapshot
publication, and acknowledgment ordering.

Autocommit statements and explicit transactions should enter the same unit. The unit must
preserve the commit ordering ratified in ADR 0014: validate, build the batch, append WAL,
make the batch durable according to the configured durability, apply live state
synchronously, publish the xid, then acknowledge the client.

### Event-enabled collection emission

An Event-enabled collection is a collection declared with `WITH EVENTS`. Its emission seam
should own producer-side event work:

```text
emit_collection_events(source_mutation, subscription_snapshot, EffectiveScope) -> queue writes | error
```

It should shape `OperatorEvent` or data-event payloads where appropriate, apply event
filters, apply `REDACT`, choose tenant-scoped queue names, enforce loop prevention, handle
backpressure, and decide whether a failed event route turns the source statement into an
error or a same-batch DLQ write.

The caller should not hand-assemble queue records. The caller should ask the event seam for
the queue writes that belong in the same commit unit as the source mutation.

### Queue lifecycle

Queue lifecycle should be split into explicit Queue delivery, Pending delivery, and Queue
retirement decisions.

The intended Queue delivery interface is conceptually:

```text
deliver(queue, group, consumer, count, EffectiveScope) -> pending deliveries
```

It owns FANOUT versus WORK semantics, consumer-group selection, attempt counters,
visibility timeout state, config/meta persistence, and result shaping.

The intended Queue retirement interface is conceptually:

```text
retire(queue, pending_delivery, decision, EffectiveScope) -> queue state delta
```

It owns ACK, NACK, claim/timeout, DLQ routing, replay, drop, and physical deletion decisions.
Callers should not directly mutate queue internals to approximate those lifecycle steps.

### Statement frame lifecycle

The Statement frame is the per-query lifecycle wrapper. Its intended interface is the one
entry point every transport and fast path crosses:

```text
execute_statement_frame(request, EffectiveScope, transport_metadata) -> result
```

It owns parsing, scope resolution, authorization context, prepared/direct execution
normalization, result-cache decisions, timing, diagnostics, and dispatch into query or
mutation execution. HTTP, gRPC, MCP, CLI, embedded, and Postgres Wire adapter paths should
normalize their transport-specific payloads and then enter the same frame.

### Catalog discovery

Catalog discovery should be rooted in `CollectionDescriptor` and native RedDB catalog
concepts:

```text
catalog_snapshot(EffectiveScope) -> CollectionDescriptor list
```

It owns tenant filtering, internal-collection filtering, `red.*` virtual table rows,
`SHOW COLLECTIONS`, typed `SHOW` commands, model names, and stability guarantees. Docs
should describe `CollectionDescriptor`, `red.*`, and `SHOW` as views over one catalog
discovery source, not separate inventories.

### Wire catalog translation

Wire catalog translation belongs in each Wire adapter, following ADR 0010. Its intended
interface is conceptually:

```text
translate_wire_catalog_query(protocol_query, EffectiveScope) -> native RedDB catalog query
```

Postgres-specific concepts such as `pg_class`, `pg_attribute`, OIDs, and `relkind` remain
inside the Postgres Wire adapter. The engine should serve native RedDB concepts such as
`red.collections` and `CollectionDescriptor`; adapters translate protocol-specific catalog
requests into those concepts.

## Maintainer Outcomes

1. A maintainer changing table scans has one place to ask whether a candidate row is
   visible: the MVCC read resolver.
2. A maintainer adding an indexed lookup cannot accidentally create a second visibility
   system because every indexed candidate is rechecked by the resolver.
3. A maintainer changing transaction commit ordering can reason about one Transaction
   commit unit instead of scattered journal, WAL, tombstone, history, and publish calls.
4. A maintainer extending `WITH EVENTS` can generate same-transaction queue writes without
   duplicating event payload and redaction logic in each DML wrapper.
5. A maintainer changing queue ACK, NACK, DLQ, or replay behavior can do it through Queue
   retirement instead of editing dispatch, persistence, and result shaping separately.
6. A maintainer adding a new transport can enter the Statement frame and inherit the same
   authorization, result-cache, timing, and execution lifecycle.
7. A maintainer adding a catalog column can update `CollectionDescriptor`/Catalog discovery
   and then let native `red.*`, `SHOW`, and Wire adapter translations project it.

## Implementation Decisions

- The first implementation tranche is the MVCC read resolver for table-row visibility.
- The tranche is deliberately narrow: rows first, resolver first, then callers.
- Existing GitHub issues #508-#514 cover the first tranche and should remain the linked
  implementation path.
- Non-MVCC seams stay as future split candidates until each has its own tracer bullet and
  acceptance criteria.
- Internal module names should use the domain vocabulary in this PRD: MVCC read resolver,
  Transaction commit unit, Event-enabled collection, Queue delivery, Pending delivery,
  Queue retirement, Statement frame, CollectionDescriptor, Catalog discovery, Wire adapter,
  EffectiveScope, and OperatorEvent.
- Compatibility is conservative. No public syntax, schema, disk-format, or wire-protocol
  change is assumed by this PRD.
- Existing behavior must be preserved unless a child issue explicitly changes behavior and
  documents the migration.
- Deep modules should expose small interfaces and own the messy correctness details behind
  those interfaces.

## Testing Decisions

- Prefer integration-style tests that assert public behavior rather than private structure.
- For the MVCC read resolver tranche, table scan, indexed lookup, logical lookup, DML target
  scan, and `AS OF` tests should agree on the same visible rows for the same snapshot.
- Add conformance coverage only when the underlying slice implements behavior. This PRD
  itself is documentation and should not add implementation-only assertions.
- For the Transaction commit unit, future tests should cover all-or-nothing commit batches,
  rollback cleanup, conflict failures, crash/restart boundaries, and acknowledgment ordering.
- For Event-enabled collection slices, future tests should prove source mutation and event
  queue writes are same-commit-unit work, including backpressure and DLQ routing decisions.
- For Queue delivery and Queue retirement slices, future tests should cover FANOUT, WORK,
  pending delivery persistence, ACK/NACK, timeout/claim, DLQ replay, and result shape.
- For Statement frame slices, future tests should exercise direct, prepared, fast-path, HTTP,
  gRPC, MCP, CLI, embedded, and Wire adapter entry points crossing the same lifecycle seam.
- For Catalog discovery and Wire catalog translation, future tests should assert that native
  `red.*`/`SHOW` results and protocol-specific catalog projections derive from the same
  `CollectionDescriptor` snapshot.

## Tracer-Bullet Issue Map

The first implementation tranche is the MVCC read resolver slice:

| Issue | Status verified on 2026-05-15 | Role |
|:------|:-------------------------------|:-----|
| [#508](https://github.com/reddb-io/reddb/issues/508) PRD: MVCC read resolver for table-row visibility | Open | Defines the resolver contract and first row-visibility scope. |
| [#509](https://github.com/reddb-io/reddb/issues/509) Table scan uses MVCC read resolver | Open | Routes full table scans through the resolver. |
| [#510](https://github.com/reddb-io/reddb/issues/510) Indexed table candidates recheck through MVCC read resolver | Open | Prevents indexes from becoming a weaker visibility system. |
| [#511](https://github.com/reddb-io/reddb/issues/511) Logical table-row lookup resolves through MVCC read resolver | Open | Routes direct logical row lookup through the resolver. |
| [#512](https://github.com/reddb-io/reddb/issues/512) DML target scans use MVCC read resolver | Open | Makes mutation target discovery use the same visibility rule. |
| [#513](https://github.com/reddb-io/reddb/issues/513) AS OF table reads route through MVCC read resolver | Open | Keeps historical table reads on the same seam. |
| [#514](https://github.com/reddb-io/reddb/issues/514) MVCC read resolver conformance pack and seam documentation | Open | Pins the tranche with conformance tests and developer docs. |

These issues already exist on GitHub and cover the first MVCC tranche. Do not create
duplicates for that tranche.

## Future Split Candidates

These areas are intentionally not implementation claims. They are candidates for later
small issues once the MVCC tranche is underway:

- Transaction commit unit tracer bullet for autocommit and explicit transaction writes
  entering the same commit path.
- Transaction commit unit crash/restart conformance around deferred WAL capture,
  history-store writes, and snapshot publication.
- Event-enabled collection emission seam for DML wrappers producing event queue writes
  through one emitter.
- Event-enabled collection backpressure/DLQ tranche that proves source mutation and DLQ
  route are one commit-unit decision.
- Queue delivery tranche for FANOUT and WORK selection through one delivery module.
- Pending delivery and Queue retirement tranche for ACK, NACK, timeout/claim, DLQ replay,
  and result shaping.
- Statement frame lifecycle tranche for prepared/direct/fast paths crossing the same frame.
- Wire adapter entry tranche proving HTTP, gRPC, MCP, CLI, embedded, and Postgres wire
  normalize requests before execution.
- Catalog discovery tranche for `CollectionDescriptor`, `red.*`, and `SHOW` sharing one
  scoped snapshot.
- Wire catalog translation tranche for Postgres catalog probes translating to native RedDB
  catalog queries inside the adapter.

## Out of Scope

- Implementing MVCC read resolution, transaction commit, event emission, queue lifecycle,
  statement-frame, catalog, or wire-adapter changes in this PRD.
- Creating duplicate GitHub issues for #508-#514.
- Broad implementation issues for non-MVCC areas before they have concrete split criteria.
- Changing public syntax, public schema, disk format, or wire protocol.
- Claiming stronger transaction guarantees than ADR 0014 records.
- Moving Postgres-specific catalog concepts into the engine.

## References

- [ADR 0010 - Wire adapters translate, never duplicate](../adr/0010-wire-adapters-translate-never-duplicate.md)
- [ADR 0014 - MVCC history store and transaction recovery](../adr/0014-mvcc-history-store-and-transaction-recovery.md)
- [ADR 0015 - WITH EVENTS Dual-Write Window](../adr/0015-events-dual-write-window.md)
- `CONTEXT.md` domain glossary
