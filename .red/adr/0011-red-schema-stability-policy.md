# ADR 0011 — `red.*` schema stability policy

**Status:** Accepted
**Date:** 2026-05-08
**Related:** ADR 0010 (wire adapters), PRD #237 (table metadata & introspection)

## Context

The `red.*` virtual schema (`red.collections`, `red.indices`, `red.stats`, etc) is the canonical RedDB-native introspection surface. Operators, dashboards (Grafana, internal panels), SRE scripts, and CLI tools query it. Some clients query it programmatically over the lifetime of a deployment.

When RedDB evolves between versions, the engine may want to rename a column (e.g., `attention` → `quarantined`), remove an obsolete one, or add new fields. Without a stated policy, every change is a breaking change in disguise — a query that worked in 3.1 can silently start returning empty / errored / drifted data in 3.2.

## Decision

`red.*` is **additive-only by default**, with a structured deprecation path for removals and renames.

Concrete rules:

1. **Adding a column** is permitted in any minor release. Existing queries are not affected.
2. **Renaming a column** requires:
   - The new name is added in release `N`. Both names work in parallel.
   - The old name emits a runtime deprecation warning when queried (logged to `tracing` and to the `Deprecation` HTTP response header for HTTP callers).
   - The `CHANGELOG.md` entry for release `N` documents the rename and the removal target version.
   - Earliest removal: release `N+1` if `N+1` is a major release; otherwise the next major after `N`.
3. **Removing a column** follows the same path as a rename — at least one minor release of warning before the column disappears.
4. **Columns prefixed with `_experimental_`** are exempt from the policy. They may change or disappear at any time. The prefix is the contract — clients consuming them accept the risk.
5. **The conformance corpus** (`tests/conformance/`) contains a positive case for every stable column in `red.*`. A change that drops or renames a column without following this policy fails CI.
6. **`docs/reference/red-schema.md`** is the source of truth for which columns are stable, which are experimental, and what the deprecation timeline is for any pending removals.

## Considered alternatives

**No commitment ("internal detail").** Honest but hostile to automation. SRE scripts and dashboards that depend on `red.*` would break silently across upgrades. Rejected.

**Versioned views (`red.v1.collections`, `red.v2.collections`).** Robust but heavy — every change forces N parallel views with synchronization. BigQuery does this for `INFORMATION_SCHEMA`; Postgres does not. The maintenance cost outweighs the benefit for our scale. Rejected.

**Strict additive-only forever.** No removals ever. Schema accumulates legacy fields permanently (`entities` and `row_count` coexisting). Simpler short-term but compounds. Rejected.

## Consequences

- Conformance corpus grows by one positive case per stable column added — explicit cost.
- `docs/reference/red-schema.md` becomes a tracked surface — every PR touching `red.*` updates it.
- Removing a column is a multi-release process; engineers cannot land a rename in a single PR.
- Clients that consume `red.*` programmatically can rely on minor-version stability and one-major-release-of-warning before any breaking change.
- `_experimental_` prefix becomes load-bearing — engineers must consciously decide whether a new field is stable or experimental at the moment of introduction. Defaulting to experimental is encouraged for new metrics whose shape is still evolving.

## Cross-references

- ADR 0010 (wire adapters translate, never duplicate)
- CONTEXT.md (`red` schema, `SHOW COLLECTIONS`)
- `docs/reference/red-schema.md` (to be created in PRD #237 implementation)
- `tests/conformance/` (corpus seeded by PRD #227)
