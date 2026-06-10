# ADR 0052 — `reddb-io-types`: neutral keystone crate for the logical type system

Status: accepted
Date: 2026-06-09

## Decision

The logical type system — `Value`, `DataType`, `SqlTypeName`, and the coercion
rules, today in `reddb-server/src/storage/schema/types.rs` — moves to a new
**neutral crate `reddb-io-types`** that sits below every authority crate.
`reddb-io-file`, `reddb-io-wire`, the planned `reddb-io-rql` (ADR 0053) and the
planned `reddb-io-crypto` (#1053) all depend on it; **nothing depends back on
`reddb-server`**.

The move is a **byte-faithful re-home**: the server keeps a re-export shim
(`storage::schema` → `pub use reddb_io_types::*`) so the ~180 call-sites across
every subsystem (storage 88, runtime 60, wire, application, presentation, grpc,
replication…) stay untouched on day one.

## Why

Extracting any further authority crate (rql, crypto) without first re-homing
the type vocabulary would force that crate to depend on `reddb-server` for
`Value`/`DataType` — a cycle that makes the "authority crate" a fiction and
kills the *server-shrinks-to-glue* end state. The logical type system is the
single most cross-cutting vocabulary in the repo; only a neutral home keeps the
crate graph acyclic.

## Considered options

- **Fold into `reddb-io-file`** (already owns the *physical* type encoding:
  `PhysicalSqlTypeName`, `BTreeValueCell`, `ValueFlag`) — rejected: makes file a
  hub every other crate must traverse, conflating logical vocabulary with
  on-disk layout authority.
- **Let `reddb-io-rql` own it** (query is the heaviest consumer) — rejected for
  the same hub reason, mirrored.
- **Leave it in the server** — rejected: every future authority crate leaks a
  dependency back into the server.

The physical encoding stays in `reddb-io-file`; this ADR moves only the logical
vocabulary.
