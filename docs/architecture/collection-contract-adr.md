# ADR: Collection Contract as the Authoritative Logical Catalog

Status: Proposed

## Context

RedDB currently exposes a rich logical surface for schema definition and multi-model collections, but the engine does not yet have a single persisted contract that authoritatively defines what a collection is.

Today, logical truth is split across multiple layers:

- SQL parsing captures rich DDL intent such as columns, types, nullability, defaults, TTL, context index hints, and index declarations.
- Runtime DDL creates and drops physical collections, but most logical schema details are advisory or transient.
- Catalog views infer model and schema mode from observed entities rather than from declared metadata.
- The schema subsystem already contains strong building blocks such as `SchemaRegistry`, `TableDef`, `ColumnDef`, and coercion utilities, but they are not yet the canonical source for runtime collection semantics.
- Documentation describes typed schemas, schema registry persistence, and coercion semantics that are only partially enforced in the live write path.

This creates four problems:

1. The catalog is observational instead of contractual.
2. DDL semantics are richer than runtime guarantees.
3. Persisted metadata does not fully represent the declared logical model.
4. Different write paths may observe different operational semantics.

For a database engine, this is the wrong center of gravity. The logical contract of a collection must be explicit, persisted, queryable, and consistently applied across all APIs and write paths.

## Decision

RedDB will introduce a persisted `CollectionContract` and make it the authoritative logical definition for every collection.

The contract will sit above the unified physical store and below user-facing APIs. It will become the source of truth for:

- declared collection model
- declared schema mode
- declared columns and constraints
- declared indexes and context indexing intent
- collection-level default TTL
- collection versioning and lifecycle metadata
- declared vs observed consistency reporting

The existing schema subsystem will be reused where possible instead of creating a second schema system.

## Goals

- Establish a single logical source of truth per collection.
- Persist collection declarations across restarts.
- Make `CREATE TABLE`, `ALTER TABLE`, and `DROP TABLE` meaningful in the logical plane.
- Align catalog output with declared state first and observed state second.
- Enable safe later work on coercion, validation, and relational guarantees.
- Preserve the existing unified storage engine as the physical substrate.

## Non-Goals

- Full relational parity in the first implementation.
- Automatic physical rewrite of existing data during every schema change.
- Immediate enforcement of foreign keys or advanced transactional constraints.
- A full optimizer rewrite in the first delivery.

## Core Concepts

### Collection Contract

A `CollectionContract` is the persisted, authoritative logical definition of a collection.

Recommended minimum shape:

```rust
pub struct CollectionContract {
    pub name: String,
    pub declared_model: CollectionModel,
    pub schema_mode: SchemaMode,
    pub origin: ContractOrigin,
    pub version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub default_ttl_ms: Option<u64>,
    pub context_index_fields: Vec<String>,
    pub table_def: Option<TableDef>,
    pub declared_indexes: Vec<DeclaredIndexContract>,
}
```

Notes:

- `table_def` is optional to support non-tabular models without forcing table semantics everywhere.
- `declared_indexes` should represent logical intent, not only operational artifacts.
- `origin` distinguishes explicit DDL-backed contracts from implicit contracts created by first-write behavior.

### Contract Origin

```rust
pub enum ContractOrigin {
    Explicit,
    Implicit,
    Migrated,
}
```

Meaning:

- `Explicit`: created by DDL or explicit admin API.
- `Implicit`: created automatically when a collection is materialized by insert-first behavior.
- `Migrated`: bootstrapped from legacy persisted state during upgrade.

### Declared vs Observed State

Every collection should expose both declared and observed information.

Declared state:

- model from contract
- schema mode from contract
- columns and constraints from contract
- declared indexes from contract

Observed state:

- actual stored entity kinds
- actual operational indexes and artifacts
- operational readiness and staleness

Declared state is authoritative. Observed state is diagnostic.

### Collection Models

`CollectionModel` remains the user-facing logical model classification.

Expected initial set:

- `Table`
- `Document`
- `Graph`
- `Vector`
- `Mixed`
- `TimeSeries`
- `Queue`

Rules:

- A declared model must come from the contract when present.
- Model inference remains available only as a fallback for legacy or implicit collections.
- A collection created through DDL must retain its declared model even when empty.

### Schema Modes

`SchemaMode` must describe runtime write semantics, not aspiration.

Definitions:

- `Dynamic`: no declared field contract is required; fields may vary freely.
- `SemiStructured`: declared fields have rules, but extra fields may still be accepted.
- `Strict`: declared fields and constraints are enforced by the write path.

Rules:

- `Strict` must not be emitted unless the runtime actively enforces strict semantics.
- Auto-created collections should default to `Dynamic`.
- Legacy inferred collections may temporarily expose inferred schema mode, but only when no contract exists.

## Layer Responsibilities

### Parser and AST

Responsibilities:

- parse DDL into a complete logical intent
- preserve all schema-bearing information without loss

The parser is not a source of truth. It is an intent capture layer.

### Runtime DDL

Responsibilities:

- translate parsed DDL into contract mutations
- create or remove physical collections as needed
- persist contract changes durably
- emit clear errors when requested mutations violate current contract rules

`CREATE TABLE`, `ALTER TABLE`, and `DROP TABLE` must no longer be merely advisory in the logical plane.

### Schema Subsystem

Responsibilities:

- own tabular column and constraint structures
- validate internal consistency of declared table definitions
- provide coercion and validation utilities to the write path

The existing `SchemaRegistry` and `TableDef` should be promoted into runtime infrastructure, not left as an isolated library.

### Unified Store

Responsibilities:

- remain the physical entity substrate
- remain collection-oriented at the storage level
- avoid becoming the owner of higher-level schema semantics

The unified store should not become the logical source of truth for collection contracts.

### Catalog

Responsibilities:

- render declared and observed state together
- compute drift and attention based on the gap between contract and operational reality
- expose contract presence and provenance

The catalog should stop inferring primary logical truth from data when a contract exists.

### Write Path

Responsibilities:

- resolve collection contract before mutating data
- apply defaults and coercion
- enforce mode-specific rules
- mark operational artifacts stale when fast paths skip maintenance work

All write channels must converge on the same contract-aware behavior.

## Persistence Strategy

The contract must be persisted alongside existing physical metadata rather than only in ephemeral runtime structures.

Recommended approach:

- extend `PhysicalMetadataFile` with collection contract storage
- serialize contract state in both JSON and binary metadata paths where applicable
- bootstrap contract state from legacy metadata or inferred collections during upgrade
- preserve backward compatibility for databases without contract metadata

Recommended additions to physical metadata:

```rust
pub struct PhysicalMetadataFile {
    pub manifest: SchemaManifest,
    pub catalog: CatalogSnapshot,
    pub collection_contracts: Vec<CollectionContract>,
    // existing fields remain
}
```

Rules:

- contract data must survive restart
- an empty collection must still be representable
- contract updates must be durable independently of data inference

## Behavioral Changes

### CREATE TABLE

`CREATE TABLE` must:

- create the physical collection if it does not exist
- create and persist an explicit collection contract
- persist default TTL when declared
- persist context index intent when declared
- persist column definitions and table constraints
- fail clearly when a conflicting contract already exists

### ALTER TABLE

`ALTER TABLE` must:

- mutate the persisted contract
- increment contract version
- preserve non-destructive semantics for existing data unless a future migration layer rewrites rows explicitly
- be reflected immediately by catalog and describe operations

`ALTER TABLE` does not need to rewrite existing rows in the first delivery, but it must stop being a metadata no-op.

### DROP TABLE

`DROP TABLE` must:

- remove the contract
- remove the physical collection
- clean operational artifacts and dependent state
- invalidate caches and indexes tied to the collection

### Implicit Collection Creation

Insert-first behavior may remain, but it must now create an implicit contract.

Recommended implicit defaults:

- model inferred from inserted entity kind
- `schema_mode = Dynamic`
- empty `table_def`
- no declared indexes
- no context index fields unless configured through a separate admin surface

This preserves RedDB's flexible onboarding without sacrificing a unified catalog model.

## Catalog Semantics

Catalog entries should expose at least:

- `contract_present`
- `contract_origin`
- `declared_model`
- `observed_model`
- `declared_schema_mode`
- `operational_schema_mode` only if still needed for diagnostics
- `declared_indexes`
- `operational_indexes`
- `resources_in_sync`
- staleness or lag indicators for deferred maintenance paths

The displayed `model` and `schema_mode` should resolve as:

1. contract value when contract exists
2. inferred fallback only when contract does not exist

## Write Path Enforcement Roadmap

Contract-aware enforcement should be delivered in stages.

Stage 1:

- apply declared defaults
- validate `NOT NULL`
- coerce declared field types where supported

Stage 2:

- enforce unknown-field policy by schema mode
- `Strict`: reject undeclared fields
- `SemiStructured`: allow undeclared fields but still enforce declared ones
- `Dynamic`: preserve current flexible behavior

Stage 3:

- enforce collection model restrictions
- tabular collections reject graph-only or vector-only payload shapes unless explicitly mixed

Stage 4:

- enforce `PRIMARY KEY` and `UNIQUE`
- introduce explicit error semantics for collisions and unsupported guarantees

## Bulk Ingest and Deferred Maintenance

Fast ingest paths may continue to skip expensive maintenance work, but the engine must make this explicit.

Required behavior:

- mark affected collection artifacts as stale or lagging
- expose lag in catalog and readiness endpoints
- provide an explicit rebuild or catch-up mechanism
- never silently present stale operational state as fully current

This is essential for `ASK`, context search, fulltext, metadata filters, and other derived read surfaces.

## Migration Strategy

The migration path from current behavior should be:

1. add contract persistence structures
2. bootstrap contracts from legacy collections and metadata
3. distinguish migrated contracts from explicit contracts
4. update DDL paths to mutate contracts
5. update catalog rendering to prefer contracts
6. add contract-aware write enforcement

Legacy collections without explicit DDL should remain usable.

## Risks

### Risk: Dual Schema Systems

If a new contract structure is created without reusing `SchemaRegistry` and `TableDef`, RedDB will deepen the existing split instead of resolving it.

Mitigation:

- reuse existing schema types wherever possible
- keep one canonical tabular schema representation

### Risk: Partial Enforcement with Strict Labels

If the catalog reports `Strict` before the write path enforces strict rules, RedDB will continue to misrepresent behavior.

Mitigation:

- gate strict mode on actual enforcement readiness

### Risk: Divergent Semantics by API Surface

If SQL becomes contract-aware but HTTP or gRPC writes remain contract-blind, behavior will stay inconsistent.

Mitigation:

- enforce rules in shared runtime/entity service paths, not only in SQL handlers

### Risk: Incomplete Drop Cleanup

If contract removal is implemented without derived artifact cleanup, RedDB will preserve stale state and drift.

Mitigation:

- define a single orchestrated collection lifecycle operation

## Acceptance Criteria

The first major milestone is complete when all of the following are true:

- a collection contract is persisted and survives restart
- an empty DDL-created collection appears correctly in catalog and describe surfaces
- `CREATE TABLE` and `ALTER TABLE` update persisted contract state
- catalog model and schema mode come from contract when available
- auto-created collections receive implicit contracts
- drift between declared and operational artifacts is visible
- docs no longer claim guarantees that do not exist

## Immediate Implementation Backlog

### Phase A: Foundation

- add `CollectionContract` and `ContractOrigin`
- extend physical metadata persistence to store contracts
- bootstrap migrated contracts for legacy databases
- add contract lookup helpers in the runtime-facing database facade

### Phase B: DDL Integration

- map `CreateTableQuery` into `TableDef` and `CollectionContract`
- update `CREATE TABLE` to persist explicit contracts
- update `ALTER TABLE` to mutate contract versions and column definitions
- update `DROP TABLE` to remove contracts

### Phase C: Catalog Integration

- change catalog rendering to prefer declared over inferred state
- expose `contract_present`, `contract_origin`, `declared_model`, and `observed_model`
- distinguish inferred legacy collections from explicit contract collections

### Phase D: Write Path Enforcement

- resolve contracts in shared entity creation paths
- apply defaults, null checks, and coercion
- enforce schema mode semantics
- mark derived artifacts stale on deferred maintenance paths

### Phase E: Lifecycle Cleanup

- implement collection-scoped cleanup for context index, metadata index, fulltext, refs, btree, and dependent caches
- ensure drop and recreate with the same name does not inherit stale operational state

## What Will Not Change

- The unified store remains the physical data substrate.
- Multi-model behavior remains a product goal.
- Insert-first onboarding remains supported through implicit contracts.
- Operational artifact readiness remains an important catalog concept.

## Summary

RedDB should move from an inferred, partially advisory logical layer to an explicit, persisted, contract-driven collection architecture.

The engine already has most of the building blocks needed for this shift. The correct next step is not a storage rewrite. It is the consolidation of the logical control plane around a persisted `CollectionContract` that connects DDL, catalog, persistence, and write enforcement.
