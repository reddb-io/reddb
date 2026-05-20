# Public Surface Contract Matrix

> **Machine-readable source of truth:**
> [`public-surface-contract-matrix.json`](./public-surface-contract-matrix.json)
> (schema: [`public-surface-contract-matrix.schema.json`](./public-surface-contract-matrix.schema.json)).
> Every promise is a row; every public surface (`sql`, `http`, `redwire`,
> `grpc`, `driver_helpers`) is a column. A cell marked `supported` or `partial`
> **must** name at least one automated test that exists on disk. The release
> gate [`scripts/verify-contract-matrix.mjs`](../../scripts/verify-contract-matrix.mjs)
> enforces this and **blocks the release** otherwise (wired into both
> `.github/workflows/ci.yml` and the `plan` job of `.github/workflows/release.yml`).
> Run it locally with `node scripts/verify-contract-matrix.mjs`.
>
> The prose tables below are the human-facing ledger and feedback-coverage map;
> when they disagree with the JSON, **the JSON wins** because it is the gated
> artifact.

This matrix is the release-quality ledger for public RedDB promises that were
visible in README/docs/drivers/examples or proven by the Grimms feedback files.
It is intentionally conservative: a row is only `passing` when there is named
automated coverage or a known passing feedback probe. Otherwise it is either
`missing test coverage`, `failing`, or `intentionally unsupported`.

Status vocabulary: `passing`, `failing`, `missing test coverage`,
`intentionally unsupported`.

Conformance layers: `runtime/parser`, `HTTP`, `persistence`, `transport smoke`,
`SDK`.

## Public Promise Matrix

| ID | Source | Public promise | Feedback coverage | Status | Minimum conformance layer | Evidence or next action |
|---|---|---|---|---|---|---|
| PSC-001 | README.md | RedDB is a single multi-model database with tables, graph, KV, timeseries, probabilistic, vector, queue, and document surfaces. | FB-OLD-40, FB-OLD-41, FB-OLD-42, FB-NEW-33 | missing test coverage | persistence | Add one cross-model reopen smoke that proves every promised model can be created, written, read, and reopened from one file. |
| PSC-002 | docs/query/graph-commands.md | MATCH supports node, edge, label, property, and LIMIT projections. | FB-OLD-01, FB-OLD-02, FB-OLD-03, FB-NEW-04, FB-NEW-05, FB-NEW-07 | missing test coverage | runtime/parser | Existing regressions cover some MATCH behavior; expand to edge-property projection, case rules, and HTTP parity. |
| PSC-003 | docs/query/graph-commands.md | GRAPH algorithms accept semantic identifiers, limits, ordering, and return stable rich rows. | FB-OLD-04, FB-OLD-05, FB-OLD-06, FB-OLD-07, FB-NEW-01, FB-NEW-02, FB-NEW-03, FB-NEW-08 | missing test coverage | HTTP | Add embedded plus HTTP conformance for CENTRALITY, PROPERTIES, SHORTEST_PATH, LIMIT, and label lookup. |
| PSC-004 | docs/query/insert.md | INSERT can create rows, nodes, edges, vectors, documents, and native timeseries points. | FB-OLD-18, FB-OLD-19, FB-OLD-28, FB-NEW-10, FB-NEW-21, FB-NEW-23 | failing | runtime/parser | Documents and vectors are publicly promised but not yet first-class enough for the feedback scenarios. |
| PSC-005 | docs/query/probabilistic-commands.md | HLL, SKETCH, and FILTER have write and read commands for cardinality, frequency, and membership. | FB-OLD-24, FB-OLD-25, FB-OLD-26, FB-NEW-20, FB-NEW-21, FB-NEW-22, FB-NEW-32 | failing | runtime/parser | Next slice must reproduce SQL-read forms and command forms for probabilistic collections. |
| PSC-006 | docs/query/insert.md | Timeseries stores timestamped metrics with tags and supports useful query/readback. | FB-OLD-31, FB-OLD-32, FB-NEW-15, FB-NEW-16 | missing test coverage | HTTP | Add HTTP or SDK smoke that writes tags and asserts tags decode as usable JSON, not placeholder text. |
| PSC-007 | docs/reference/metrics.md | Operational metrics and health/readiness surfaces are usable by clients. | FB-NEW-24, FB-NEW-30 | missing test coverage | transport smoke | Add readiness and bind-collision tests that prove one failed transport does not hide healthy query paths. |
| PSC-008 | drivers/ | Official drivers expose usable query, parameter, transport, error, and model helpers. | FB-OLD-14, FB-OLD-35, FB-OLD-36, FB-OLD-37, FB-OLD-38, FB-NEW-24, FB-NEW-25, FB-NEW-26 | missing test coverage | SDK | Define one shared helper spec, then implement driver conformance across JS, Python, Rust, Go, Java, .NET, Dart, and PHP. |
| PSC-009 | crates/reddb-client/README.md | Rust client supports one connection-string API across embedded, gRPC, HTTP, and RedWire with parameters. | FB-OLD-14, FB-NEW-24, FB-NEW-25, FB-NEW-26, FB-NEW-27 | missing test coverage | SDK | Add local binary contract tests for parameterized query and transport response envelopes. |
| PSC-010 | examples/ | Example environments demonstrate tested behavior and do not require undocumented workarounds. | FB-OLD-43, FB-OLD-44, FB-NEW-31, FB-NEW-33 | missing test coverage | transport smoke | Update examples only after implementation slices remove or document each workaround. |
| PSC-011 | ../feedbacks.md | SQL aggregate, projection, expression, and mutation behavior should match ordinary SQL expectations where advertised. | FB-OLD-08, FB-OLD-09, FB-OLD-10, FB-OLD-11, FB-OLD-12, FB-OLD-13, FB-OLD-15, FB-OLD-16, FB-OLD-17 | missing test coverage | runtime/parser | Add focused regressions for COUNT alias, count column collision, CONCAT, CURRENT_TIMESTAMP, affected counts, joins, and subqueries. |
| PSC-012 | ../feedbacks.md | Collection DDL should either work as advertised or fail with clear unsupported messages. | FB-OLD-18, FB-OLD-19, FB-OLD-20, FB-OLD-21, FB-OLD-22, FB-OLD-23 | failing | runtime/parser | CREATE DOCUMENT and CREATE VECTOR need implementation or public docs correction before release. |
| PSC-013 | ../feedbacks.md | KV should support natural namespaced keys or reject them clearly, and expose first-class get/list helpers. | FB-OLD-27, FB-OLD-28, FB-OLD-29, FB-OLD-30, FB-NEW-13, FB-NEW-14 | missing test coverage | SDK | Decide support versus rejection for colon keys; add SQL, HTTP, and SDK tests. |
| PSC-014 | ../feedbacks.md | Queue and cache public APIs should be usable where exposed. | FB-OLD-29, FB-OLD-30 | failing | SDK | Either implement queue push/pop and embedded cache or mark them transport-limited in docs/types. |
| PSC-015 | ../feedbacks.md | Introspection should expose indexes, schema, kinds, and user/internal columns clearly. | FB-OLD-33, FB-OLD-34, FB-OLD-39, FB-NEW-09, FB-NEW-22 | missing test coverage | runtime/parser | Add SHOW INDEXES, DESCRIBE, SHOW CREATE, graph node/edge view, and probabilistic kind regressions. |
| PSC-016 | ../feedbacks-new.md | File-backed multi-model rebuilds must be order-independent and durable. | FB-NEW-10, FB-NEW-31 | missing test coverage | persistence | Add persistent Grimms-scale ordering regression: graph first then table, table first then graph, reopen both. |
| PSC-017 | ../feedbacks-new.md | Server transports must expose the same query contract as embedded. | FB-NEW-03, FB-NEW-24, FB-NEW-25, FB-NEW-26, FB-NEW-27, FB-NEW-28, FB-NEW-29, FB-NEW-30 | missing test coverage | transport smoke | Promote Grimms mini-smoke across HTTP, RedWire, and gRPC with identical result-shape assertions. |
| PSC-018 | ../feedbacks-new.md | Native statistics, vector, ASK, and SEARCH surfaces should either work or be documented as limited. | FB-NEW-12, FB-NEW-17, FB-NEW-19, FB-NEW-23, FB-NEW-33 | failing | runtime/parser | Close statistics/vector/search issues or remove public promises from docs/examples until supported. |

## Feedback Scenario Coverage

| ID | Source | Scenario | Contract row | Current disposition |
|---|---|---|---|---|
| FB-OLD-01 | ../feedbacks.md | MATCH node projection returned empty objects. | PSC-002 | Needs regression for materialized node projection. |
| FB-OLD-02 | ../feedbacks.md | MATCH edge-label filter was ignored. | PSC-002 | Needs label-filter regression and case-rule definition. |
| FB-OLD-03 | ../feedbacks.md | MATCH LIMIT was rejected. | PSC-002 | Needs parser/runtime LIMIT coverage. |
| FB-OLD-04 | ../feedbacks.md | Native GRAPH algorithms worked and should stay rich. | PSC-003 | Preserve with algorithm smoke coverage. |
| FB-OLD-05 | ../feedbacks.md | GRAPH NEIGHBORHOOD required numeric internal IDs. | PSC-003 | Needs label lookup or documented ID-return path. |
| FB-OLD-06 | ../feedbacks.md | GRAPH NEIGHBORHOOD lacked labelled-edge filtering. | PSC-003 | Needs feature decision and tests. |
| FB-OLD-07 | ../feedbacks.md | SHORTEST_PATH/TRAVERSE edge-label filters were missing. | PSC-003 | Needs feature decision and tests. |
| FB-OLD-08 | ../feedbacks.md | Plain graph row projection returned empty objects. | PSC-011 | Needs projection regression. |
| FB-OLD-09 | ../feedbacks.md | Basic aggregates and grouping worked. | PSC-011 | Preserve in regression pack. |
| FB-OLD-10 | ../feedbacks.md | SUM(count) returned null because column name collided with aggregate name. | PSC-011 | Needs exact regression. |
| FB-OLD-11 | ../feedbacks.md | Subqueries were rejected. | PSC-011 | Decide unsupported docs or implement. |
| FB-OLD-12 | ../feedbacks.md | JOINs were rejected. | PSC-011 | Decide unsupported docs or implement. |
| FB-OLD-13 | ../feedbacks.md | Prepared `?` placeholders routed into SPARQL errors. | PSC-008 | Needs SDK and parser routing regression. |
| FB-OLD-14 | ../feedbacks.md | SQL expression/function default names and values were surprising. | PSC-011 | Needs expression alias/value regression. |
| FB-OLD-15 | ../feedbacks.md | CONCAT and `||` produced quoted broken values. | PSC-011 | Needs runtime expression fix. |
| FB-OLD-16 | ../feedbacks.md | CURRENT_TIMESTAMP behaved like a column/table projection. | PSC-011 | Needs scalar function or clear rejection. |
| FB-OLD-17 | ../feedbacks.md | UPDATE, DELETE, and INSERT returned affected zero after mutation. | PSC-011 | Needs affected-count regression. |
| FB-OLD-18 | ../feedbacks.md | CREATE VECTOR was rejected while advertised by parser/docs. | PSC-004 | Needs vector slice or docs correction. |
| FB-OLD-19 | ../feedbacks.md | CREATE DOCUMENT was rejected. | PSC-004 | Needs first-class document CRUD. |
| FB-OLD-20 | ../feedbacks.md | HLL/SKETCH/FILTER/QUEUE/KV/TIMESERIES DDL worked. | PSC-012 | Preserve positive DDL coverage. |
| FB-OLD-21 | ../feedbacks.md | HLL/SKETCH/FILTER parameters were not accepted. | PSC-012 | Needs options decision. |
| FB-OLD-22 | ../feedbacks.md | Probabilistic collections had no useful query-time read API. | PSC-005 | Needs failing test and implementation. |
| FB-OLD-23 | ../feedbacks.md | Probabilistic kinds were reported as table. | PSC-015 | Needs introspection regression. |
| FB-OLD-24 | ../feedbacks.md | Embedded KV put accepted multi-type values. | PSC-013 | Preserve SDK smoke. |
| FB-OLD-25 | ../feedbacks.md | Colon KV keys were silently normalized to underscore. | PSC-013 | Needs support or clear rejection. |
| FB-OLD-26 | ../feedbacks.md | KV GET SQL syntax failed. | PSC-013 | Needs command/parser or docs correction. |
| FB-OLD-27 | ../feedbacks.md | SDK lacked db.kv.get. | PSC-013 | Needs helper spec and implementation. |
| FB-OLD-28 | ../feedbacks.md | KV watch APIs existed but were not exercised. | PSC-013 | Needs automated coverage. |
| FB-OLD-29 | ../feedbacks.md | Queue could be created but had no push/pop API. | PSC-014 | Needs queue usable slice. |
| FB-OLD-30 | ../feedbacks.md | Cache APIs existed in SDK but failed in embedded. | PSC-014 | Needs docs/type gating or embedded implementation. |
| FB-OLD-31 | ../feedbacks.md | Timeseries insert and basic aggregates worked. | PSC-006 | Preserve smoke. |
| FB-OLD-32 | ../feedbacks.md | Timeseries tags came back as placeholder text. | PSC-006 | Needs tags round-trip test. |
| FB-OLD-33 | ../feedbacks.md | Index creation worked and should stay visible. | PSC-015 | Preserve create/explain coverage. |
| FB-OLD-34 | ../feedbacks.md | SHOW INDEXES returned zero rows after index creation. | PSC-015 | Needs introspection fix. |
| FB-OLD-35 | ../feedbacks.md | Entity insert results did not return IDs. | PSC-008 | Needs SDK/runtime returning-id design. |
| FB-OLD-36 | ../feedbacks.md | Internal red_* columns leaked into SELECT star. | PSC-015 | Needs public/internal projection rule. |
| FB-OLD-37 | ../feedbacks.md | DESCRIBE was unsupported. | PSC-015 | Needs schema introspection slice. |
| FB-OLD-38 | ../feedbacks.md | SHOW CREATE TABLE returned no rows. | PSC-015 | Needs schema introspection slice. |
| FB-OLD-39 | ../feedbacks.md | Error messages sometimes listed rejected token as expected. | PSC-012 | Needs error-message regression. |
| FB-OLD-40 | ../feedbacks.md | bulkInsert was effectively single-row over stdio. | PSC-008 | Needs SDK performance/contract test. |
| FB-OLD-41 | ../feedbacks.md | Typed rows and exists/list helpers were missing. | PSC-008 | Needs helper spec. |
| FB-OLD-42 | ../feedbacks.md | Multi-model in one file was the killer working story. | PSC-001 | Preserve as capstone conformance. |
| FB-OLD-43 | ../feedbacks.md | Embedded snapshots were portable. | PSC-001 | Preserve reopen/snapshot smoke. |
| FB-OLD-44 | ../feedbacks.md | Docs needed a what-works page. | PSC-010 | Docs must track matrix statuses. |
| FB-NEW-01 | ../feedbacks-new.md | Node inserts did not return IDs for edge creation. | PSC-003 | Needs ID-return or label-edge insert. |
| FB-NEW-02 | ../feedbacks-new.md | GRAPH SHORTEST_PATH required internal IDs, not labels. | PSC-003 | Needs label shortest-path regression. |
| FB-NEW-03 | ../feedbacks-new.md | GRAPH CENTRALITY became an operational dependency and differed on HTTP. | PSC-017 | Needs transport parity test. |
| FB-NEW-04 | ../feedbacks-new.md | MATCH edge-label case normalization was unclear. | PSC-002 | Needs explicit case contract. |
| FB-NEW-05 | ../feedbacks-new.md | MATCH edge property projection was unreliable. | PSC-002 | Needs r.evidence regression. |
| FB-NEW-06 | ../feedbacks-new.md | GRAPH PROPERTIES returned confusing node_type values. | PSC-003 | Needs node_type regression. |
| FB-NEW-07 | ../feedbacks-new.md | Native MATCH still needed TS fallback for rich commands. | PSC-002 | Needs broader MATCH conformance. |
| FB-NEW-08 | ../feedbacks-new.md | Graph algorithms lacked ergonomics and consistent pagination. | PSC-003 | Needs algorithm limit/page contract. |
| FB-NEW-09 | ../feedbacks-new.md | Graph table view mixed nodes and edges confusingly. | PSC-015 | Needs nodes/edges view or docs rule. |
| FB-NEW-10 | ../feedbacks-new.md | File-backed rebuild was sensitive to graph/table write order. | PSC-016 | Needs persistence ordering regression. |
| FB-NEW-11 | ../feedbacks-new.md | COUNT(*) AS count parse was fragile. | PSC-011 | Needs exact regression. |
| FB-NEW-12 | ../feedbacks-new.md | Tables became fallback for native model gaps. | PSC-018 | Needs feature-specific closure. |
| FB-NEW-13 | ../feedbacks-new.md | KV keys rejected or normalized colon names. | PSC-013 | Needs decision and tests. |
| FB-NEW-14 | ../feedbacks-new.md | KV surfaced as kv_default instead of clear KV UX. | PSC-013 | Needs helper/docs improvement. |
| FB-NEW-15 | ../feedbacks-new.md | Timeseries tags were not usable ergonomically. | PSC-006 | Needs tags smoke. |
| FB-NEW-16 | ../feedbacks-new.md | Timeseries lacked bucket/window/downsample/range/tag UX. | PSC-006 | Needs scope decision. |
| FB-NEW-17 | ../feedbacks-new.md | Rich statistics remained in TypeScript. | PSC-018 | Needs stats feature decision. |
| FB-NEW-18 | ../feedbacks-new.md | Aggregate syntax was sensitive despite basics working. | PSC-011 | Needs aggregate grammar regression. |
| FB-NEW-19 | ../feedbacks-new.md | Graph statistics did not replace exploratory statistics. | PSC-018 | Needs stats docs or implementation. |
| FB-NEW-20 | ../feedbacks-new.md | Probabilistic structures lacked useful SDK/query interrogation. | PSC-005 | Needs command and SQL forms. |
| FB-NEW-21 | ../feedbacks-new.md | HLL insert format failed against declared HLL collection. | PSC-005 | Needs add/write contract. |
| FB-NEW-22 | ../feedbacks-new.md | SHOW COLLECTIONS historically reported probabilistic as table. | PSC-015 | Needs kind introspection regression. |
| FB-NEW-23 | ../feedbacks-new.md | VECTOR keyword existed but CREATE VECTOR was rejected. | PSC-004 | Needs vector support or docs removal. |
| FB-NEW-24 | ../feedbacks-new.md | Official HTTP client rejected readiness while query worked. | PSC-017 | Needs readiness/client smoke. |
| FB-NEW-25 | ../feedbacks-new.md | RedWire returned incomplete response envelopes. | PSC-017 | Needs RedWire rows/columns smoke. |
| FB-NEW-26 | ../feedbacks-new.md | gRPC was parsed as RedWire frames. | PSC-017 | Needs gRPC transport smoke. |
| FB-NEW-27 | ../feedbacks-new.md | HTTP parser rejected queries accepted by embedded. | PSC-017 | Needs query parity smoke. |
| FB-NEW-28 | ../feedbacks-new.md | GRAPH CENTRALITY remote default returned only 100 rows. | PSC-017 | Needs limit/default contract. |
| FB-NEW-29 | ../feedbacks-new.md | SQL over HTTP did not see graph nodes inserted via graph syntax. | PSC-017 | Needs projection parity smoke. |
| FB-NEW-30 | ../feedbacks-new.md | Wire port collision caused full server startup failure. | PSC-007 | Needs bind/readiness behavior fix. |
| FB-NEW-31 | ../feedbacks-new.md | Embedded rebuild order should not matter. | PSC-016 | Needs persistence regression. |
| FB-NEW-32 | ../feedbacks-new.md | Errors did not point to the correct API for HLL writes. | PSC-005 | Needs actionable error message. |
| FB-NEW-33 | ../feedbacks-new.md | Showcase still required broad TS/raw-fetch workarounds. | PSC-010 | Needs capstone showcase smoke after slices land. |

## Non-Public Inputs

ADRs, internal planning notes, and issue files are useful design inputs, but
they are not public product promises by themselves. This matrix treats only
README.md, docs/query/, docs/reference/, drivers/, crates/reddb-client/README.md,
examples/, ../feedbacks.md, and ../feedbacks-new.md as public or feedback-derived
sources for conformance. Internal ADRs can justify an implementation choice, but
they cannot downgrade a public promise without a docs/examples update.

## Minimum Conformance Rule

For every public promise that remains in docs, examples, or driver READMEs, the
minimum acceptable proof is:

- `runtime/parser`: focused parser/runtime regression for the exact syntax and
  result shape.
- `HTTP`: the same behavior through the public HTTP query surface.
- `persistence`: reopen or rebuild test proving data and metadata survive.
- `transport smoke`: parity check for the advertised transport or server mode.
- `SDK`: driver-level test against local RedDB behavior, not only mocked client
  serialization.

## Focused Regression Packs

- `tests/e2e_feedback_regression_pack.rs` covers the #451 feedback regressions
  for PSC-003, PSC-005, PSC-006, PSC-011, and PSC-013: probabilistic SQL-read
  forms, `COUNT(*) AS count`, quoted KV colon keys, timeseries JSON tags, and
  `GRAPH PROPERTIES` `node_type` preservation.
- `tests/feedback_regression.rs` is the #549 feedback-scenario regression
  bundle: one named test per `FB-OLD-NN` and `FB-NEW-NN` row in the
  Feedback Scenario Coverage table above. Each test header documents the
  source feedback file and the PSC contract row it maps to. Runtime-
  reachable scenarios assert engine behavior directly; transport, SDK,
  and persistence scenarios pin the matrix row so any silent change to
  the disposition breaks the suite.

## Adding a promised feature

When you ship (or document) a new public capability, add it to the matrix in
the same change so the release gate keeps the promise honest:

1. **Add a row** to `public-surface-contract-matrix.json`. Pick the next free
   `PSC-NNN` id, set `source` (the README/doc/driver file that makes the
   promise), and write the `promise` text. Add a `cells` entry for **every**
   surface listed in `surfaces` — the verifier rejects a row that skips one.
2. **Set each cell's status honestly:**
   - `supported` — offered on this surface and backed by automated tests.
   - `partial` — offered with known gaps; the listed tests pin current
     behaviour.
   - `unsupported` — not offered on this surface (no test required).
3. **Name the backing test(s).** For `supported`/`partial`, `tests` must list
   at least one path that exists in the repo (Rust integration test, driver
   conformance test, wire fixture, etc.). The path is relative to the repo
   root. If you don't have a test yet, write one first — that is the point of
   the gate; you cannot mark a cell supported on a promise alone.
4. **Run the gate locally:** `node scripts/verify-contract-matrix.mjs` must
   exit 0, and `node --test scripts/contract_matrix_contract.test.mjs` must
   pass. Update the prose tables above if the new promise belongs in the
   human-facing ledger.

## Removing a promised feature

Removing or relaxing a promise is a release-policy decision (see below):

1. If the capability is being **dropped from a surface**, set that cell to
   `unsupported` and delete its `tests`. Also remove the promise from the
   README/doc/driver file named in `source` — the matrix must not promise what
   the docs no longer claim, and vice versa.
2. If the **entire promise is retired**, delete the row from the JSON and the
   corresponding prose rows, and remove the public claim from its `source`.
3. Re-run `node scripts/verify-contract-matrix.mjs`. Removing coverage never
   fails the gate, but leaving a `supported` cell with a now-deleted test
   will — so deletions and matrix edits land in one commit.

## Release-blocking policy and ownership (HITL)

The matrix is a **release-blocking** control. `scripts/verify-contract-matrix.mjs`
runs in the `contract-matrix` job of `ci.yml` and, critically, in the `plan`
job of `release.yml`; every `publish-*` job depends on `plan`, so a violation
stops GitHub, npm, crates.io, Docker, and PyPI publishing.

Because relaxing this gate (downgrading a cell, deleting the step, weakening
the rule) changes what "released" guarantees, those files are owned by the
release-policy owner in [`.github/CODEOWNERS`](../../.github/CODEOWNERS)
(`@filipeforattini`). Changes under `docs/conformance/` and to the verifier
require that owner's review — the human sign-off this policy depends on.
