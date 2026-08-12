# ADR 0075 — Separate vector-filter and entity metadata value vocabularies

Status: accepted
Date: 2026-08-12

## Context

RedDB currently uses the name `MetadataValue` for two different concepts:

- `reddb_types::vector_metadata::MetadataValue` is the scalar vocabulary of
  vector-search filters. Its five variants participate in equality and ordering
  comparisons and form part of the wire and driver contract.
- `reddb_server::storage::unified::metadata::MetadataValue` is the per-entity
  metadata storage model. Its twelve variants are used across the unified
  storage runtime and include values for which vector-filter ordering has no
  defined semantics.

Treating the types as competing declarations of one concept would either expand
the public filter contract speculatively or discard load-bearing storage values.
The devx query layer also exposes metadata terminology without making the role
of its vocabulary explicit. This ambiguity prevents the concept fence in #2126
from rejecting new declarations reliably.

ADRs 0046 and 0052 establish the authority direction: contract types live in
their authority or neutral keystone crate, while the server owns runtime models
and adapts them at the boundary.

## Decision

### The five-variant filter vocabulary remains the contract

`reddb_types::vector_metadata::MetadataValue` remains unchanged as the
canonical vector-filter vocabulary:

- `String(String)`
- `Integer(i64)`
- `Float(f64)`
- `Bool(bool)`
- `Null`

It continues to define wire, driver, and filter semantics. This decision does
not add variants or change an encoding.

### The twelve-variant storage vocabulary is an internal model

The server type is renamed to `EntityMetadataValue`. It remains the internal
per-entity metadata storage model; its additional variants are not retired.
The rename makes the distinct concept visible at imports and call sites.

### A total projection defines the vector-filter seam

One explicit, exhaustive projection from `EntityMetadataValue` to the contract
vocabulary lives at the vector-filter seam. Its result distinguishes an
included contract value from an excluded internal value. Every internal variant
has the following disposition:

| `EntityMetadataValue` variant | Vector-filter disposition |
| --- | --- |
| `String(value)` | Include as contract `String(value)` |
| `Int(value)` | Include as contract `Integer(value)` |
| `Float(value)` | Include as contract `Float(value)` |
| `Bool(value)` | Include as contract `Bool(value)` |
| `Null` | Include as contract `Null` |
| `Bytes(value)` | Exclude; byte sequences have no filter ordering contract |
| `Array(value)` | Exclude; collections have no filter ordering contract |
| `Object(value)` | Exclude; objects have no filter ordering contract |
| `Timestamp(value)` | Exclude; timestamp filter semantics are not part of the contract |
| `Geo { lat, lon }` | Exclude; geographic ordering has not been defined |
| `Reference(value)` | Exclude; entity-reference ordering has not been defined |
| `References(value)` | Exclude; reference-collection ordering has not been defined |

Exclusion is semantic, not a serialization fallback. None of the seven
non-scalar variants may be canonical-stringified for filtering. Making a
specific excluded variant filterable later requires a new decision supported by
defined comparison semantics and compatibility evidence.

### The devx query vocabulary is named by role

When the devx query-layer vocabulary is named during the execution sweep, it
receives the same concept separation. Entity metadata operations reuse
`EntityMetadataValue`; vector-filter operations reuse the keystone contract
type. The query layer does not own a third metadata-value vocabulary.

### Both names become fenceable concepts

Removing the `MetadataValue` name collision is a goal of this decision. Once
the rename and seam projection land, the concept fence in #2126 can reject new
declarations of either `MetadataValue` or `EntityMetadataValue` instead of
grandfathering an ambiguous duplicate.

## Considered options

- **Adopt all twelve variants into the keystone contract.** Rejected because it
  would expand the wire and driver surface with `Geo`, references, objects, and
  other values before filter comparison semantics exist.
- **Retire the seven additional variants.** Rejected because they are
  load-bearing parts of the unified storage runtime.
- **Canonical-stringify non-scalars at the seam.** Rejected because lexical
  ordering would invent permanent filter semantics unrelated to the underlying
  values.
- **Keep both types named `MetadataValue`.** Rejected because it hides the
  concept boundary and prevents a meaningful architectural fence.

## Consequences

- The public five-variant vector-filter contract and existing drivers remain
  wire-compatible.
- Unified storage retains all twelve entity metadata variants under a name that
  communicates their role.
- Callers crossing from entity metadata into vector filtering must handle
  exclusion explicitly.
- Tests for the seam must exercise all twelve projection arms, including each
  excluded variant.
- Follow-up issue #2249 performs the rename, projection, query-layer naming, and
  focused tests. This ADR slice does not perform those mechanical changes.

## Related

- Spec #2113 — authority-crate phase 2 and dead-code retirement
- Issue #2126 — concept-based authority fences
- Issue #2127 — decision slice
- Issue #2249 — execution slice
- ADR 0046 — wire and file crate authority boundary
- ADR 0052 — neutral keystone crate for the logical type system
