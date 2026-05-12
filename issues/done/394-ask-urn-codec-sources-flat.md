# URN codec + flat sources_flat array with urn per source (UrnCodec) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/394

Labels: enhancement

GitHub issue number: #394

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Promote URN to a first-class field on every source entry, and introduce a flat `sources` array view alongside the legacy bucketed layout.

Adds the `UrnCodec` deep module — bidirectional codec for `reddb:<collection>/<id>` with model-specific suffixes (`#<score>` for vector hits, `#<edge_id>` for graph edges, `#<fragment>` for document chunks). Pure, round-trip testable.

Response gains a `sources_flat` array where each entry has `{kind, urn, content, score, ...kind_specific}`. Legacy bucketed `sources` remains for one release as a deprecation period. The `citations` array gains a `urn` field per citation, derived from `sources_flat[index].urn`.

## Acceptance criteria

- [ ] `UrnCodec` deep module: property-based round-trip tests for every `kind`; UTF-8 collection names; percent-encoding edges.
- [ ] `sources_flat` array present on every ASK response.
- [ ] Each citation in `citations` carries its `urn`.
- [ ] Legacy bucketed `sources` field still present; documented as deprecated.
- [ ] Integration test verifies URN navigability for every kind (table row, document, vector hit, graph node, graph edge, KV entry).

## Blocked by

- #393
