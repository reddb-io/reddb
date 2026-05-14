# PRD: Finalize partial and still-open issue evidence gaps

Labels: prd

GitHub: https://github.com/reddb-io/reddb/issues/333

GitHub issue number: #333

## Problem Statement

RedDB now has an evidence report for all 311 GitHub issues, but the report still separates the work into three materially different classes:

- 272 issues have strong current code or test evidence.
- 39 issues have only partial current evidence, which means the current codebase shows some implementation signal but not enough proof to say every acceptance criterion is complete.
- 3 GitHub issues are still open even though the current code evidence is strong: #238, #252, and #282.

From the maintainer perspective, this creates release risk. A closed issue with partial evidence may hide an incomplete behavior, a test gap, a stale acceptance criterion, or a feature that was later superseded by another issue. An open issue with strong evidence creates workflow noise and makes it unclear whether the remaining work is implementation, review, or just GitHub reconciliation.

This is especially risky in RedDB because many of the partial issues touch correctness-sensitive surfaces: migrations, statement execution, collection contracts, DML target scans, parser hardening, blob cache persistence, queue semantics, event subscriptions, DDL auth, Config/Vault, and transport/driver contracts.

## Solution

Create a focused completion project that turns every partial or still-open issue into one of three final states:

1. Verified complete with direct evidence and enough tests.
2. Superseded by a later issue with explicit cross-reference and evidence.
3. Reopened or split into follow-up issues with a concrete missing acceptance criterion.

The project should consume the generated issue evidence report as its starting manifest. For each partial issue, a maintainer or agent should inspect the current implementation, map the original acceptance criteria to observable behavior, and either strengthen the evidence or create a narrow implementation ticket. For each still-open issue with strong code evidence, the project should reconcile GitHub state by closing the issue or documenting the remaining human decision.

The final deliverable is a clean release-readiness ledger where no issue is merely "probably done". Each issue must have code evidence, test evidence, supersession evidence, or a new follow-up issue.

## User Stories

1. As a RedDB maintainer, I want every partial issue to have a final disposition, so that the issue tracker reflects the actual state of the engine.
2. As a RedDB maintainer, I want open issues with strong code evidence to be reconciled, so that the backlog does not contain stale work.
3. As a release owner, I want the partial issue list to be reduced to zero, so that release readiness does not rely on guesswork.
4. As a storage engineer, I want migration issues to be checked against runtime behavior, so that schema evolution remains safe across WAL, VCS, and tenant boundaries.
5. As a storage engineer, I want `APPLY MIGRATION *` behavior to be verified end to end, so that bulk migration apply order is not only parser-supported.
6. As a storage engineer, I want branch-scoped migration conflict behavior verified, so that VCS merge semantics cannot silently corrupt schema state.
7. As a runtime engineer, I want Statement Execution Frame follow-ups to be proven by behavior, so that auth, tenant, config, lock, and policy context are consistently derived.
8. As a runtime engineer, I want CollectionContract enforcement to be tested across insert and mutation paths, so that collection model invariants are not bypassed.
9. As a runtime engineer, I want DML Target Scan behavior proven for DELETE and UPDATE, so that multi-model mutation semantics stay consistent.
10. As a security engineer, I want parser hardening and secret-redaction issues to include test evidence, so that parser snapshots and generated corpora cannot leak secrets.
11. As a security engineer, I want KV policy action coverage reconciled for #252, so that the action vocabulary is clearly final or clearly still under human review.
12. As a security engineer, I want DDL drop/truncate authorization evidence, so that destructive DDL cannot bypass the policy resolver.
13. As an operator, I want the nightly DR drill failure #116 verified from workflow and script behavior, so that backup/restore confidence is based on the fixed runner path.
14. As an operator, I want red_client binary-size and container-image issues fully evidenced, so that release artifacts stay small and predictable.
15. As an SDK user, I want Python cache methods to be verified through public behavior, so that SDK cache APIs do not drift from server semantics.
16. As an SDK user, I want Redis migration tooling status to be explicit, so that users are not promised a dual-write CLI flow that is only partially present.
17. As a query engineer, I want parser conformance corpus coverage to be tied to real corpus files and CI jobs, so that docs and landing examples remain parseable.
18. As a query engineer, I want parser coverage uplift claims to be tied to measurable coverage, so that coverage targets are not closed administratively.
19. As a performance engineer, I want Blob Cache tracer, TTL, admission, invalidation, L2, synopsis, and warm restart issues to have runtime and persistence evidence, so that cache correctness is not inferred from module existence.
20. As a performance engineer, I want Blob Cache benchmark and public API review status to be explicit, so that product claims are backed by repeatable measurements.
21. As a docs maintainer, I want Red Schema reference status to be verified, so that `red.*` introspection docs remain canonical.
22. As a queue user, I want FANOUT runtime semantics verified through consumer-visible behavior, so that broadcast delivery is reliable.
23. As a queue user, I want ALTER QUEUE mode transition behavior verified, so that active consumers do not observe ambiguous semantics.
24. As an events user, I want multi-subscription behavior verified through producer and queue output, so that event fanout and redaction remain predictable.
25. As a Config user, I want Config WATCH, LIST, and TAGS status verified, so that operational configuration is observable and discoverable.
26. As a Vault user, I want sealed storage, unseal, redaction, audit, and policy evidence, so that secrets remain protected across reads, writes, and transports.
27. As an API user, I want domain-separated KV, Config, and Vault transports to supersede the older umbrella issue explicitly, so that users understand the current contract.
28. As an agent working the backlog, I want a manifest of partial issues grouped by domain, so that independent slices can be picked up safely.
29. As an agent working the backlog, I want each gap to produce either evidence or a follow-up issue, so that no investigation disappears.
30. As a reviewer, I want each follow-up to include acceptance criteria and verification commands, so that completion can be checked without reading the entire history.
31. As a maintainer, I want superseded issues to reference the newer issue that won, so that later implementation can intentionally overwrite earlier design.
32. As a maintainer, I want the evidence report regenerated after fixes, so that the ledger reflects the current workspace rather than stale closure state.
33. As a maintainer, I want the GitHub state for #238, #252, and #282 reconciled, so that open issues represent real remaining work.
34. As a maintainer, I want partial evidence to be separated from strong evidence, so that release risk is visible.
35. As a maintainer, I want this cleanup to be incremental, so that high-risk domains can be completed before lower-risk docs or tooling gaps.
36. As a product owner, I want a clear final summary of what was completed, superseded, reopened, and split, so that the roadmap can be trusted.

## Implementation Decisions

- Treat the generated evidence report as the source manifest for this PRD.
- Do not assume that a closed GitHub issue is complete. Completion requires current code evidence, current test evidence, or explicit supersession by a later issue.
- Do not assume that a later issue automatically supersedes an earlier one. Supersession must be recorded with the newer issue number and the behavior that replaced the older objective.
- Group work by domain to keep review scopes small:
  - Migrations and VCS schema conflict behavior.
  - Statement execution, policy context, collection contracts, and DML target scans.
  - Release tooling for red_client size and container distribution.
  - Parser hardening, parser coverage, conformance corpora, and secret redaction.
  - Blob Cache internals, persistence, invalidation, warm restart, and benchmarking.
  - SDK and migration tooling for cache and Redis compatibility.
  - Red Schema reference and introspection documentation.
  - Queue modes, queue transition behavior, and DLQ/queryable queue grammar.
  - Event subscriptions and event fanout behavior.
  - DDL authorization for destructive operations.
  - Config/Vault system collections, domain-separated APIs, policy, audit, and watch/list/tags behavior.
- Add or update deep modules only when they reduce cross-path duplication:
  - A migration verification harness for parser, registry, apply, rollback, VCS, tenant, and dependency behavior.
  - A statement context or execution frame module with a stable interface for auth, tenant, config, and lock intent derivation.
  - A collection contract gate shared by insert and mutation paths.
  - A DML target scan abstraction shared by UPDATE and DELETE.
  - A parser conformance registry that can connect docs examples, corpus cases, fuzz seeds, and coverage gates.
  - A Blob Cache verification harness that exercises L1/L2, synopsis, invalidation, TTL, restart, and benchmark entry points.
  - A keyed-domain API contract layer for KV, Config, and Vault that keeps HTTP, MCP, drivers, policy, and audit aligned.
- For #238, #252, and #282, decide whether each issue should be closed, moved to human review, or split into a new implementation issue. The current report says code evidence is strong, but GitHub remains open.
- For the 39 partial issues, each must end with one of these labels in the evidence report: confirmed, superseded, reopened, or split.
- The evidence report generator should remain reproducible and should not depend on transient local intent.
- Avoid broad refactors while closing evidence gaps. Each follow-up should be a small vertical slice with a concrete verification command.

## Testing Decisions

- Good tests must verify external behavior or durable contracts, not private implementation details.
- Migration tests should exercise SQL entry points and observable catalog/VCS/runtime state.
- Statement execution tests should verify authorization, tenant scoping, config resolution, lock intent, and mutation behavior from public SQL/API paths.
- Collection contract and DML target scan tests should cover insert, update, delete, invalid model operations, and multi-model behavior.
- Parser tests should reuse the existing parser hardening style: positive corpus, negative corpus, snapshots, property tests, and fuzz seeds where applicable.
- Secret-redaction tests should assert that generated snapshots and parser artifacts cannot emit known secret patterns.
- Blob Cache tests should cover TTL, admission, invalidation, L2 persistence, membership synopsis behavior, warm restart, and benchmark harness wiring.
- Queue tests should assert consumer-visible WORK/FANOUT behavior and mode transition behavior with active consumers.
- Event tests should assert multiple subscriptions, redaction, filters, and target queue output.
- DDL auth tests should assert denied destructive operations before execution and allowed operations for principals with the correct action.
- Config/Vault tests should cover system collection protection, sealed storage, redaction, unseal behavior, audit records, policy decisions, and domain-separated transport/driver behavior.
- SDK tests should verify public methods and wire-compatible envelopes rather than internal implementation details.
- CI/tooling tests should check artifact size, container publish configuration, and drill-nightly behavior through the public Makefile/workflow entry points.
- After every closure batch, regenerate the evidence report and compare the count of partial/open items.

## Out of Scope

- Rewriting the issue tracker workflow.
- Changing RedDB's public SQL grammar unless a partial issue proves the current grammar is incomplete.
- Large storage, WAL, or VCS refactors unrelated to the partial/open issue list.
- Adding new product features beyond what is required to finish, supersede, or split the existing partial/open issues.
- Treating documentation-only evidence as enough for runtime-sensitive behavior.
- Closing #238, #252, or #282 without either a maintainer decision or a follow-up issue for any remaining human review.

## Further Notes

Starting manifest:

- Evidence report: `reports/github_issues_objective_status.json`
- Reproducer: `scripts/issue_code_evidence_report.js`
- Summary at PRD creation time:
  - Total GitHub issues audited: 311
  - Strong current code/test evidence: 272
  - Partial current evidence: 39
  - Open GitHub issues with strong evidence: 3

Partial evidence issues to resolve:

- #16 migrations: APPLY MIGRATION * - bulk apply all pending in dependency order
- #21 migrations: branch-scoped migrations + VCS merge schema conflict detection
- #24 Clippy: drop redundant closures in impl_migrations.rs
- #36 AI providers: HuggingFace embeddings client + Anthropic embed fallback
- #46 Extract Statement Execution Frame for read statements
- #48 Move privilege and lock intent derivation into Statement Execution Frame
- #49 Centralize CollectionContract enforcement for INSERT paths
- #50 Centralize CollectionContract enforcement for mutation paths
- #51 Introduce DML Target Scan for DELETE
- #52 Reuse DML Target Scan for UPDATE
- #62 CI binary-size guard for red_client
- #68 Container image strategy for red_client
- #87 SQL query parser: hardening harness (property + fuzz + snapshot + limits)
- #97 Backfill: secret-redaction audit of existing parser snapshots
- #116 Nightly DR drill failed: 2026-05-06
- #140 Blob Cache: land internal in-memory tracer
- #141 Blob Cache: add rich TTL and admission policy
- #142 Blob Cache: add explicit invalidation primitives
- #143 Blob Cache: move SQL result cache onto L1 adapter
- #145 Blob Cache: add durable L2 backing
- #146 Blob Cache: add membership synopsis for fast L2 misses
- #147 Blob Cache: enable result-cache warm restart via L2
- #149 Blob Cache: benchmark impact against result cache and Redis
- #151 Blob Cache: review public API shape
- #163 Docs: productize RedDB perf wins
- #197 Python SDK: cache get/put/invalidate methods in drivers/python
- #199 CLI: red migrate-from-redis dual-write tool
- #231 Parser conformance: full positive corpus
- #233 Parser conformance: fuzz nightly schedule + corpus seeded
- #236 Parser coverage uplift C: lexer.rs + table.rs
- #263 Docs: docs/reference/red-schema.md canonical reference
- #287 Queue mode: FANOUT runtime semantics
- #289 Queue mode: ALTER QUEUE SET MODE + warning on active consumers
- #296 Events: multi-subscription per collection
- #309 DDL: auth enforcement for drop + truncate
- #317 Umbrella: Vault sealed storage
- #318 Umbrella: Vault unseal, redaction, audit, and policy
- #319 Umbrella: system collections red.config/red.vault
- #321 Umbrella: Config/Vault WATCH, LIST, and TAGS

Still-open issues to reconcile:

- #238 PRD: Redis-flavor KV DSL
- #252 KV policy actions + audit log
- #282 QUEUE MOVE + SELECT FROM QUEUE design review

This PRD should be triaged into small issues rather than implemented as one large patch.
