# ADR 0010 — Wire adapters translate, never duplicate

**Status:** Accepted
**Date:** 2026-05-08
**Related:** PRD #237 (table metadata & introspection)

## Context

RedDB exposes a Postgres-wire listener (`wire/postgres/`) so that JDBC/Prisma/SQLAlchemy/Hibernate/BI tools (Metabase, DBeaver, Superset) can connect without a custom driver. Most of these clients automatically probe `pg_class`, `pg_namespace`, `pg_attribute`, `pg_index` on connect — not because a human typed `\dt`, but because the driver does schema discovery silently to populate autocomplete / type registries / migration plans.

Without a response to those probes, real-world clients either fail to connect or assume an empty schema. So RedDB needs *some* answer for `SELECT FROM pg_class` reaching the engine.

The native introspection surface (per CONTEXT.md) is the `red.*` schema: `red.collections`, `red.indices`, `red.stats`, etc. `SHOW COLLECTIONS` (and typed variants `SHOW TABLES` / `SHOW QUEUES` / etc) are the canonical UX. There is no `\dt`-style shorthand — RedDB's introspection language is its own.

The question: where does the translation between PG-internal introspection (`pg_class`, etc) and the RedDB-native surface (`red.collections`, etc) live?

## Decision

**Translation lives in the wire adapter, never in the engine.**

Concretely:

- The engine (planner, executor, catalog, storage) knows **only** the RedDB-native surface: `red.collections`, `red.indices`, `red.stats`. Postgres-specific concepts (`relkind`, OIDs, `attnotnull`, `pg_class` columns) **do not appear** anywhere outside `wire/postgres/`.
- A new module `wire/postgres/translator.rs` interprets incoming queries that touch `pg_*` tables, rewrites them into equivalent queries against `red.*`, and forwards the rewritten query to the engine.
- A PG-wire client that issues `SELECT * FROM red.collections` directly bypasses translation and hits the native path — no double-handling.
- Future wire adapters (MongoDB, MySQL) follow the same pattern: their introspection vocabularies (`listCollections`, `SHOW DATABASES`) translate to native commands inside the respective `wire/*/translator.rs`.

## Considered alternatives

**Inline `pg_catalog` views in the engine.** Treat `pg_catalog.pg_class` as a first-class virtual table alongside `red.collections`, served directly from the catalog snapshot. Simpler short-term — one set of executor logic.

Rejected because:
- Postgres concepts (`relkind`, `pg_*` column shapes, OIDs) leak into the engine's domain model and into the glossary, training material, and tests. Future maintainers see `relkind` and ask why a non-Postgres engine is talking about it.
- Adding MongoDB-wire later means duplicating the same pattern with `listCollections` artifacts in the engine. Cumulative pollution.
- Engineering parity becomes harder — every catalog change requires considering "does this also need a `pg_class` mirror update?"

**Drop `pg_catalog` entirely.** Treat Postgres-wire compat as best-effort and let ORMs/BI tools see empty schema.

Rejected because Postgres-wire compat is a stated product capability (per `docs/api/postgres-wire.md`). Without `pg_class` answers, common BI tools and ORMs visibly fail or render no schema, breaking the promise.

**Stub `pg_class` returning empty.** Engine answers `SELECT FROM pg_class` with zero rows so handshake succeeds.

Rejected because tools then "connect successfully" but show an empty database — worst UX of the three.

## Consequences

- Each new ORM/BI tool that hits the wire may surface a new `pg_*` query pattern not yet covered by `translator.rs`. Translation coverage is incremental, driven by real-world clients. Conformance/snapshot tests pin every supported pattern.
- The `wire/postgres/translator.rs` module is non-trivial — initial estimate ~500–1500 LOC for the seven-view minimum (`pg_class`, `pg_namespace`, `pg_tables`, `pg_attribute`, `pg_index`, `pg_constraint`, `pg_database`).
- `red.*` is the only canonical surface in docs, training, and conformance tests. PG concepts appear only in `docs/api/postgres-wire.md` and the PG translator README — never in the data-model docs.
- A future `wire/mongo/` (if pursued) reuses the same architecture; the engine never grows knowledge of Mongo-specific shapes.

## Cross-references

- CONTEXT.md (`Wire adapter` term)
- `docs/architecture/wire-adapters.md` (architectural overview, translation table)
- `docs/api/postgres-wire.md` (PG-specific user-facing doc)
- `crates/reddb-server/src/wire/postgres/` (existing listener; `translator.rs` to be added)
