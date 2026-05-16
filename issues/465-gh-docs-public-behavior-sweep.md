---
status: open
tag: AFK
gh: 465
---

# [AFK] gh-465: Make docs and examples match tested public behavior

GitHub: reddb-io/reddb#465

## What to build

Update README, query docs, driver READMEs, and examples so they only promise behavior covered by the contract matrix and conformance tests. Unsupported or future behavior should be removed, clearly marked, or linked to the right issue.

## Acceptance criteria

- [ ] README examples only use tested supported behavior.
- [ ] docs/query/*.md matches implemented command behavior and errors.
- [ ] Driver READMEs match helper conformance results.
- [ ] Document, KV, probabilistic, graph, transport, and ASK docs reflect the final contracts.
- [ ] Helpful error messages point users to correct APIs where they use unsupported forms.
- [ ] Contract matrix links each public promise to a test, issue, or explicit unsupported note.

## Notes

- Use `CARGO_TARGET_DIR=.target-gh465` for isolated builds.
- Commit with `Closes #465` in message.
- Audit README.md and docs/ for examples; check against existing e2e tests (`tests/`). Flag/fix unsupported forms; don't introduce new test surface.

## Iteration 1 (2026-05-16)

Fixed: README + docs/ examples that promised behavior the parser does not accept today.

Files touched:
- `README.md`
  - Document INSERT: JSON literal must be a quoted string (`'{"level":"info"}'`), not bare `{...}`. Verified against `tests/e2e_rid_row_envelope.rs:155`, `tests/e2e_documents_first_class_crud.rs:108`.
  - KV PUT: bare `PUT key = val` does not parse — supported forms are `KV PUT 'key' = 'val'` and `PUT CONFIG ns key = val`. Verified against `tests/e2e_kv_namespaced_keys.rs:67`, `tests/e2e_config_crud.rs:43`.
  - HYPERTABLE: column-list + `WITH (ttl=...)` form is **not** parsed. Tested form is `CREATE HYPERTABLE name TIME_COLUMN ts CHUNK_INTERVAL '1d' [TTL '7d']`. Verified against `tests/e2e_create_hypertable.rs:16,33`.
- `docs/data-models/partition-ttl.md`, `docs/data-models/timeseries.md` — same hypertable correction.
- `docs/guides/logs-quickstart.md`, `docs/guides/using-reddb-for-logs.md` — flagged column-list/`WITH (ttl=...)` as planned, added the shipped form alongside.
- `docs/reference/sql-1-0-x.md` — example column updated to underscored keywords + quoted intervals.
- `docs/reference/limitations.md` — Hypertables and Partition TTL rows updated from "DDL pending" to "shipped (minimal DDL)" with pointer to planned column-list form.

Not done this iteration (next pass):
- Driver READMEs (crates/reddb-client/README.md, crates/reddb-client-connector/README.md): not audited yet against helper conformance.
- `docs/query/*.md` cross-check vs parser: still pending. Recent commit 2a30a5bf already swept docs/data-models/* for ADR 0019 vocab; query/ surface not part of that sweep.
- Graph commands (`GRAPH HITS`, `TOPOLOGICAL_SORT`, `PROPERTIES`, `PATH`) flagged by audit as documented-but-untested — needs separate test-coverage audit, not a docs-only fix.
- "Contract matrix links each public promise to a test" acceptance item: no contract matrix doc exists yet; that is a larger structural deliverable.

Blocker: Bash `git` commands require approval in current harness, so this iteration's edits are uncommitted. Human or a subsequent run with looser permissions needs to stage + commit:
  `git add README.md docs/ issues/465-gh-docs-public-behavior-sweep.md`
  Commit message suggestion: `docs: align hypertable + KV + document INSERT examples with parser (refs #465)`
Issue stays **open** because acceptance criteria (driver READMEs, query/*.md sweep, contract matrix) are only partially addressed.
