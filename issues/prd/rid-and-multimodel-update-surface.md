# PRD: Rid and multi-model update surface

Labels: prd, needs-triage

ADR: docs/adr/0019-rid-and-multimodel-update-surface.md

## Problem Statement

RedDB's public mutation and item identity vocabulary still exposes older
engine-oriented naming. Users see underscore-prefixed fields, multiple names for
the same identifier, and graph edge endpoints that do not communicate that they
refer to RedDB IDs. That makes RedDB feel like internal storage details are
leaking into SQL, JSON, events, SDKs, and documentation.

At the same time, RedDB needs a coherent way to mutate rows, documents, KV
values, graph nodes, and graph edges with one database-grade contract. Today
table `UPDATE` exists, but the same ergonomic shape does not extend cleanly to
documents, KV, or graph data. Users also need compact read-modify-write syntax
for counters and numeric values, deterministic batched updates, `RETURNING`,
and a broader math function package for expressions.

The user-facing problem is vocabulary and ergonomics: RedDB should expose one
clear RedDB ID, one generic multi-model noun, and one predictable update
language across the core models without losing database correctness.

## Solution

Implement ADR 0019 as a breaking public-surface cleanup and multi-model update
milestone.

The product surface will use `rid` as the RedDB ID everywhere. The generic
multi-model noun is `item`. Public result envelopes expose `rid`, `collection`,
`kind`, `tenant`, `created_at`, and `updated_at`. `created_at` and `updated_at`
are UTC millisecond timestamps. These fields are reserved top-level fields for
rows, documents, KV entries, graph nodes, and graph edges.

The update language targets item kinds:

- `ROWS` for table rows, with omitted kind still meaning rows.
- `DOCUMENTS` for documents.
- `KV` for key-value entries.
- `NODES` for graph nodes.
- `EDGES` for graph edges.

Compound assignment supports `+=`, `-=`, `*=`, `/=`, and `%=` as syntax sugar
over ordinary expressions. It does not support `++` or `^=`. Compound
assignment is top-level only in this milestone.

The expression language gains a Postgres-compatible math function package with
aliases where they improve ergonomics. Advanced math functions return `Float`.
Errors such as division by zero, modulo by zero, overflow, and invalid domains
abort the statement instead of writing `NULL`, `NaN`, or infinity.

Multi-row updates are atomic. `WHERE`, `RETURNING`, `LIMIT`, and `ORDER BY`
work across supported targets. `ORDER BY` is allowed only with `LIMIT` and gets
an implicit `rid ASC` tie-breaker when `rid` is not already present.

Permissions, RLS, masking, indexes, events, CDC, WAL, replication, and recovery
observe a materialized update, not a symbolic increment operation.

## User Stories

1. As a RedDB query author, I want every persisted item to expose `rid`, so that I have one stable RedDB ID name across rows, documents, KV, graph nodes, and graph edges.
2. As a RedDB query author, I want `rid` to mean RedDB ID in SQL, JSON, wire payloads, SDKs, events, and docs, so that I do not have to learn several names for the same concept.
3. As a Rust contributor, I want the identifier type to be named `Rid`, so that code vocabulary matches product vocabulary.
4. As a documentation reader, I want docs to say RedDB ID and item, so that the product language does not expose older generic storage terminology.
5. As a RedDB user, I want `rid` to be globally unique within a database, so that I can identify an item without pairing it with a collection name.
6. As a RedDB user, I want `collection` and `kind` in result envelopes, so that every item result is self-describing.
7. As a RedDB user, I want `kind` to use item kinds like `row`, `document`, `kv`, `node`, and `edge`, so that collection model and item shape are not confused.
8. As a RedDB user, I want `tenant` in the public item envelope, so that tenant scope is visible where the caller has access to it.
9. As a RedDB user, I want `created_at` and `updated_at` in UTC milliseconds, so that lifecycle timestamps round-trip cleanly through SQL and JSON transports.
10. As a RedDB user, I want `created_at` to be immutable, so that creation time remains trustworthy.
11. As a RedDB user, I want `updated_at` to advance only on real mutations, so that no-op writes do not create misleading history.
12. As a RedDB user, I want `SELECT *` and `RETURNING *` to include the public item envelope, so that envelope fields are not hidden magic.
13. As a schema author, I want reserved system field conflicts to fail clearly, so that RedDB does not silently rename or corrupt my existing fields during upgrade.
14. As a graph user, I want edge endpoints to be `from_rid` and `to_rid`, so that it is obvious they refer to RedDB IDs.
15. As a graph user, I want graph node `label` to remain graph vocabulary, so that node lookup and graph examples stay natural.
16. As a graph user, I want graph edge `label` to remain graph vocabulary, so that relationship types stay natural.
17. As a graph user, I want `rid`, `label`, `from_rid`, and `to_rid` to be immutable in the first update version, so that graph identity and topology are not accidentally rewritten.
18. As a graph user, I want edge `weight` to be mutable, so that weighted relationships can be adjusted.
19. As a graph user, I want node `node_type` to be mutable, so that node classification can be corrected without rewriting the item.
20. As a table user, I want omitted update kind to mean rows, so that existing ordinary table update syntax remains concise under the new target model.
21. As a document user, I want to update documents with an explicit `DOCUMENTS` target, so that document mutations are not disguised as table updates.
22. As a KV user, I want to update KV entries with an explicit `KV` target, so that counters and keyed values can be changed atomically.
23. As a graph user, I want to update graph nodes with an explicit `NODES` target, so that node properties can be changed through the query engine.
24. As a graph user, I want to update graph edges with an explicit `EDGES` target, so that edge properties and weights can be changed through the query engine.
25. As a RedDB user, I want collection model validation for update targets, so that an update cannot silently mutate an incompatible model.
26. As a multi-model user, I want explicit item-kind targets to work in generic or mixed collections, so that mixed collections remain useful without cross-kind ambiguity.
27. As a RedDB user, I want one update statement to target one item kind, so that permissions, RLS, and result shape stay predictable.
28. As a counter user, I want `+=` on numeric fields, so that I can write compact atomic increments.
29. As a counter user, I want `-=` on numeric fields, so that I can write compact atomic decrements.
30. As a numeric data user, I want `*=`, `/=`, and `%=`, so that common arithmetic updates are concise.
31. As a RedDB user, I want `++` excluded from the first version, so that SQL update syntax stays assignment-oriented.
32. As a RedDB user, I want `^=` excluded until exponentiation and bitwise semantics are decided, so that the operator does not mean two different things.
33. As a RedDB user, I want compound assignment to require an existing non-null numeric field, so that accidental initialization or null propagation does not corrupt data.
34. As a RedDB user, I want missing, null, and non-numeric compound assignment inputs to fail the whole statement, so that partial numeric updates are not silently wrong.
35. As a RedDB user, I want all assignments in one update statement to read the pre-image, so that update semantics match SQL expectations.
36. As a document user, I want nested mutation paths kept out of the first version, so that the initial contract avoids unclear behavior around missing parents and path conflicts.
37. As a query author, I want `ABS`, `ROUND`, `FLOOR`, and `CEIL`, so that common numeric expressions work in updates and reads.
38. As a query author, I want `SQRT`, `POWER`, `EXP`, `LN`, `LOG`, and `LOG10`, so that common scientific and scoring formulas work in RedDB expressions.
39. As a query author, I want `SIN`, `COS`, `TAN`, `ASIN`, `ACOS`, `ATAN`, and `ATAN2`, so that trigonometric formulas are available.
40. As a query author, I want `RADIANS`, `DEGREES`, and `PI`, so that angle conversions and constants are built in.
41. As a query author, I want Postgres-compatible function names as canonical names, so that existing SQL knowledge transfers to RedDB.
42. As a query author, I want aliases like `POW`, `ARCSIN`, `ARCCOS`, and `ARCTAN`, so that common programmer spelling also works.
43. As a database user, I want division by zero to be an error, so that updates do not silently write bad values.
44. As a database user, I want invalid math domains to be errors, so that RedDB does not store `NaN`, infinity, or misleading nulls.
45. As a database user, I want integer and decimal overflow to be errors, so that numeric correctness is preserved.
46. As a database user, I want a multi-row update to be atomic, so that if one candidate fails no previous candidate remains changed.
47. As a concurrent writer, I want compound updates not to lose increments, so that two increments never commit as one increment.
48. As a transaction user, I want RedDB either to serialize conflicting read-modify-write updates or reject one, so that correctness is explicit.
49. As a document user, I want `WHERE` on document updates to see top-level body fields plus the public envelope, so that document filtering matches read behavior.
50. As a KV user, I want `WHERE` on KV updates to see `key`, `value`, exposed metadata, and the public envelope, so that keyed updates are straightforward.
51. As a graph node user, I want `WHERE` on node updates to see node fields, node properties, and the public envelope, so that graph node filtering is expressive.
52. As a graph edge user, I want `WHERE` on edge updates to see edge fields, edge properties, and the public envelope, so that graph edge filtering is expressive.
53. As a RedDB user, I want `RETURNING` after multi-model updates, so that I can observe the post-image in the same atomic statement.
54. As a RedDB user, I want `LIMIT` on multi-model updates, so that I can batch migrations and repairs.
55. As a RedDB user, I want `ORDER BY` with `LIMIT`, so that batches are deterministic and controllable.
56. As a RedDB user, I want `ORDER BY` without `LIMIT` rejected in update statements, so that meaningless ordering does not complicate locking and execution.
57. As a RedDB user, I want update `ORDER BY` to accept top-level fields in the first version, so that deterministic batching is available without broad expression-ordering complexity.
58. As a RedDB user, I want `rid ASC` to be the implicit tie-breaker for ordered update batches, so that repeated batches are stable.
59. As an operator, I want permissions to use the explicit update target, so that row, document, KV, node, and edge writes are separately controllable.
60. As a security engineer, I want RLS and masking to apply to multi-model updates and `RETURNING`, so that new update syntax cannot bypass policy.
61. As an indexing user, I want affected indexes maintained as part of the same atomic statement, so that reads are immediately consistent with writes.
62. As an event user, I want multi-model updates to emit ordinary update events, so that subscriptions and CDC consumers do not need a special increment protocol.
63. As a replication operator, I want WAL and recovery to persist materialized updates, so that replay is deterministic and does not re-evaluate expressions.
64. As an SDK user, I want SDK result types to expose `rid` rather than older identifier names, so that client code matches the new vocabulary.
65. As a docs reader, I want all examples to use `rid`, `item`, item `kind`, `from_rid`, and `to_rid`, so that the docs teach one language.
66. As a release maintainer, I want this to be clearly marked as breaking, so that users know they must migrate schemas, queries, payloads, and code.

## Implementation Decisions

- Implement ADR 0019 as the source of truth for this PRD. The PRD should not redefine the contract if the ADR is amended later; instead, the PRD should be updated to point at the accepted ADR revision.
- Introduce a `Rid` domain type as the canonical code-level RedDB ID vocabulary. Existing internals that still use older generic terms can migrate in phases, but new public code and new code paths should use `Rid`.
- Use `item` as the generic product noun in public docs, errors, API descriptions, and new code. Existing internal storage language can migrate gradually where a mechanical rename would create risk.
- Replace public identifier field names with `rid`. Remove the older public identifier aliases rather than keeping compatibility shims.
- Expose the public item envelope consistently across SQL results, JSON responses, SDK return shapes, MCP tool responses, gRPC messages, events, CDC payloads, and docs.
- Reserve the top-level system field names `rid`, `collection`, `kind`, `tenant`, `created_at`, and `updated_at` across the first supported item kinds.
- Add startup or upgrade validation that detects existing top-level conflicts with reserved field names and fails with a clear error. Do not silently rename user data.
- Preserve the distinction between collection model and item kind. Collection models describe the collection. Item kinds describe the items returned by query and update surfaces.
- Rename graph edge endpoint vocabulary to `from_rid` and `to_rid` across graph insert, query, update, docs, wire payloads, SDKs, and events.
- Add update target parsing for `ROWS`, `DOCUMENTS`, `KV`, `NODES`, and `EDGES`. Omitted target remains rows.
- Validate update targets against collection model before mutation. Generic or mixed collections may accept explicit item-kind targets.
- Represent compound assignments in the query layer as ordinary expression assignments against the pre-image. Storage, WAL, events, replication, and recovery should receive materialized post-images.
- Keep compound assignment top-level only in this milestone.
- Add the Postgres-compatible math function package through the existing scalar expression machinery rather than creating a separate update-only function path.
- Treat division by zero, modulo by zero, overflow, invalid math domains, `NaN`, and infinity as statement errors.
- Execute multi-row updates atomically. Candidate selection, expression evaluation, authorization, RLS, index planning, and materialized write preparation need to happen before durable mutation is committed.
- Use existing transaction, MVCC, and locking semantics to prevent lost read-modify-write updates. The first version does not need a special lock-free counter path.
- Extend update planning to support item-kind-specific scans, target validation, `WHERE`, `RETURNING`, `LIMIT`, and ordered batch selection.
- Restrict update `ORDER BY` to top-level fields in the first version and require `LIMIT`.
- Add an implicit `rid ASC` tie-breaker when ordered update batches omit `rid`.
- Apply authorization and RLS using the explicit update target. Masking and projection rules still apply to `RETURNING`.
- Keep indexes, events, CDC, WAL, replication, and recovery on the normal update path after expressions are materialized.
- Update the public SQL reference, data-model docs, graph docs, events docs, SDK docs, and API docs in the same epic so the breaking vocabulary change is coherent.
- Stage the implementation in phases: `Rid` vocabulary and public envelope first, reserved-name validation second, graph endpoint rename third, math functions fourth, compound assignment fifth, and multi-model update execution last.

Likely deep modules:

- **Rid vocabulary and envelope module.** Owns RedDB ID naming, item envelope construction, and public field emission across result surfaces.
- **Reserved field validator.** Owns validation of user schemas and top-level item properties against reserved system names.
- **Item-kind target resolver.** Owns parsing and validation of update targets against collection models and mixed collections.
- **Compound assignment lowering.** Owns lowering `field op= expr` into an expression assignment evaluated against a pre-image.
- **Math scalar function package.** Owns catalog signatures, coercion, evaluation, domain checks, and alias resolution for the numeric functions.
- **Atomic multi-model update executor.** Owns candidate collection, pre-image evaluation, policy checks, ordered batching, post-image construction, and commit integration.
- **Ordered update batch selector.** Owns `ORDER BY` plus `LIMIT` semantics and implicit `rid` tie-breaking.
- **Graph endpoint vocabulary adapter.** Owns the transition from old graph endpoint names to `from_rid` and `to_rid` throughout public surfaces.

## Testing Decisions

Good tests for this PRD should assert externally observable behavior. A query,
HTTP request, gRPC request, SDK call, or event payload should expose the new
vocabulary and update semantics. Tests should avoid pinning internal AST
layout, planner details, or storage implementation details unless the module is
itself a pure semantic module.

Modules and behavior to test:

- **Rid and envelope behavior.** Query results, `SELECT *`, `RETURNING *`,
  events, CDC payloads, and API responses expose `rid`, `collection`, `kind`,
  `tenant`, `created_at`, and `updated_at`.
- **Reserved field validation.** Creating or opening data with top-level
  `rid`, `collection`, `kind`, `tenant`, `created_at`, or `updated_at` as user
  fields fails with an explicit conflict.
- **Graph endpoint vocabulary.** Graph edge inserts, reads, updates, and
  `RETURNING` use `from_rid` and `to_rid`.
- **Item kind vocabulary.** Rows return `kind = row`, documents return
  `kind = document`, KV entries return `kind = kv`, graph nodes return
  `kind = node`, and graph edges return `kind = edge`.
- **Update target parsing.** Positive parser tests cover omitted rows, explicit
  `ROWS`, `DOCUMENTS`, `KV`, `NODES`, and `EDGES`. Negative parser tests cover
  unsupported cross-kind update forms.
- **Collection model validation.** Runtime tests assert that incompatible
  update targets fail before mutation and that graph collections accept both
  nodes and edges.
- **Compound assignment lowering.** Runtime tests assert `+=`, `-=`, `*=`,
  `/=`, and `%=` produce the same post-image as explicit expression updates.
- **Compound assignment invalid inputs.** Runtime tests assert missing fields,
  null fields, non-numeric fields, division by zero, modulo by zero, and
  overflow abort the whole statement.
- **Pre-image semantics.** A multi-assignment update proves every expression
  reads the original item state, not earlier assignments in the same `SET`
  list.
- **Math functions.** Function tests cover canonical names, aliases, return
  types, domain errors, and invalid floating output rejection.
- **Atomicity.** Multi-row update tests include a later candidate that fails and
  assert earlier candidates were not changed.
- **Concurrency.** MVCC or locking tests cover two concurrent read-modify-write
  updates and assert no lost update outcome is possible.
- **Documents.** Integration tests update document top-level fields, filter by
  document top-level fields, return post-images, and reject nested path updates
  in this milestone.
- **KV.** Integration tests update numeric `value`, filter by `key`, return
  post-images, and reject missing or non-numeric values for compound
  assignment.
- **Graph nodes.** Integration tests update mutable node fields/properties and
  reject immutable `rid` and `label` mutations.
- **Graph edges.** Integration tests update `weight` and edge properties while
  rejecting `rid`, `label`, `from_rid`, and `to_rid` mutations.
- **Ordered batches.** Tests cover `ORDER BY` with `LIMIT`, rejection of
  `ORDER BY` without `LIMIT`, top-level-only order fields, and implicit `rid`
  tie-break behavior.
- **Permissions and RLS.** Existing IAM/RLS-style tests should be extended so
  rows, documents, KV, nodes, and edges use their explicit update target for
  authorization.
- **Events and indexes.** Existing update event and index-recheck tests should
  be mirrored for the new item targets where the storage model supports it.
- **Wire and SDK surfaces.** Driver and transport tests assert `rid` and the new
  item envelope are used consistently.

Prior art in the codebase includes statement execution contract tests,
multi-model persistence tests, graph behavior tests, KV command tests, event
foundation tests, MVCC conflict tests, append-only tests, RLS/IAM tests, parser
unit tests, and transport integration tests. New tests should extend those
families rather than inventing a separate harness where possible.

## Out of Scope

- Nested update paths such as document body paths, KV object paths, or graph
  property subpaths.
- `++` increment syntax.
- `^=` compound assignment.
- Bitwise operators.
- A cross-kind `UPDATE FROM ANY` mutation statement.
- Updating graph topology through `from_rid` or `to_rid`.
- Updating graph labels in the first version.
- Approximate or lock-free counter optimization beyond existing transaction,
  MVCC, and lock semantics.
- Keeping compatibility aliases for the older public identifier names.
- Silently migrating user fields that conflict with reserved system fields.
- Full migration tooling for external client code. The PRD requires clear
  breaking docs and failures, not automatic rewrite tooling.
- New item kinds beyond rows, documents, KV, graph nodes, and graph edges.
- `ORDER BY` expressions or functions in update statements.
- `ORDER BY` in update statements without `LIMIT`.
- Decimal-exact implementations of trigonometric or logarithmic functions.

## Further Notes

This PRD intentionally treats the vocabulary cleanup and the multi-model update
surface as one epic. The update syntax should not be built on top of the older
identifier vocabulary and then renamed later. The `rid` and item envelope work
should land first so every new update test and doc example uses the final
language.

The work is breaking by design. ADR 0019 is an explicit exception to the normal
additive-only posture for stable public surfaces because the goal is to remove
confusing old names rather than accumulate more aliases.

The strongest implementation risk is not parsing. The hard parts are atomic
multi-model update execution, reserved-name upgrade failures, RLS and masking
coverage, WAL/recovery materialization, index maintenance, and making every
public transport agree on the same new vocabulary.
