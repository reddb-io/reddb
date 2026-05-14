# ADR: parameter contract, wire Value enum, error taxonomy [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/377

Labels: hitl, release-blocker

GitHub issue number: #377

## Parent

#351

## What to build

Decision record that locks the cross-layer contracts before parameterized query
work is considered complete. Defines:

- the parameter `Value` enum used on the wire and in the binder
- the canonical placeholder syntax and how drivers surface it
- the error taxonomy for bind failures
- which transports carry typed values vs JSON-encoded values

## Current implementation state

ADR 0015 exists at `docs/adr/0015-parameterized-query-contract.md` and records
the parameter value taxonomy, placeholder syntax, transport encoding, driver
surfaces, compatibility policy, and error taxonomy.

The remaining gate is human review. The ADR currently says:

```text
Status: Draft (open for human review)
```

## Acceptance criteria

- [x] ADR document exists describing the wire `Value` enum variants.
- [x] ADR records the chosen placeholder syntax and how each driver surfaces it.
- [x] ADR records the error taxonomy with code/message shape.
- [x] ADR records which transports carry typed values vs JSON-encoded values.
- [ ] Team has reviewed and accepted the ADR (HITL gate).

## Blocked by

Human acceptance of ADR 0015.
