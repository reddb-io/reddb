# ADR 0009 — Performance gate scope: universal 20% vs scenario-specific [HITL]

GitHub: reddb-io/reddb#153
Parent: #152

Author `docs/adr/0009-performance-gate-scope.md` recording the strategic gate decision: universal "≥20% faster than PG/Mongo/Neo4j on every mapped scenario" vs scenario-specific "RedDB wins where the unified engine matters; paridade or close-gap elsewhere".

Recommendation in PRD #152: scenario-specific. Universal-20% requires architecture work (sharded log structure, columnar push-down, MVCC redesign) and would invalidate every other slice in #152.

## Acceptance Criteria

- [ ] `docs/adr/0009-performance-gate-scope.md` exists, follows 0006/0007 format.
- [ ] Both options documented with concrete cost.
- [ ] Decision recorded with rationale citing BASELINE.md numbers.
- [ ] Cross-linked from PRD #152 and `docs/perf/roadmap.md`.
- [ ] Status: Accepted.
