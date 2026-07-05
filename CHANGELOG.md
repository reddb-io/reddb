# Changelog

## 1.22.0

### Minor Changes

- Document DML overhaul — UPDATE surface + nested SET (ADR 0067, PRD [#1703](https://github.com/reddb-io/reddb/issues/1703), completing the clean break started in v1.21.0):

  - **UPDATE model markers removed.** `UPDATE <c> DOCUMENTS SET …`, `UPDATE <c> ROWS SET …`, and `UPDATE <c> KV SET …` are **rejected** with a didactic error — the catalog already knows the collection's model, so unmarked `UPDATE <c> SET …` infers it. `NODES`/`EDGES` markers stay (a graph holds both record kinds). Dotted assignment targets (`SET a.b.c = …`) now parse for all targets and are validated by the analyzer against the catalog model.
  - **Nested document SET** deep-merges dotted paths into the document body (intermediate objects created only where absent, siblings preserved), guarded against dotted paths through scalar/array intermediates and respecting the reserved-field rule.
  - **Docs re-canonicalized**: the Documents teaching surface uses the inline/unmarked forms throughout, with a new Identifier-vs-data case-rule section (identifiers fold case; JSON body keys are matched exactly).

  All published drivers (js, js-client, cpp, dart, dotnet, go, java, kotlin, php, python, python-asyncio, zig, client) emit the unmarked document UPDATE form in this release.

## 1.21.0

### Minor Changes

- Document DML overhaul (ADR 0066/0067, PRD [#1703](https://github.com/reddb-io/reddb/issues/1703)). The document write surface is redesigned around one canonical form:

  - **Inline strict-JSON literals are the only way to write JSON in RQL** — `INSERT INTO events DOCUMENT VALUES ({"level":"info"})`. The quoted-string coercion (`VALUES ('{…}')`) and the ceremonial `(body)` column list are rejected with didactic errors that name the sanctioned form; legacy `_ttl` metadata columns point at `WITH TTL`.
  - **`JSON_PARSE(<expr>)`** added as the explicit string→JSON conversion for runtime-string cases.
  - **Array literals parse losslessly** into a native array; vector-vs-JSON is resolved from the target's type at analysis/runtime instead of committing to an f32 vector at parse time (large integers destined for JSON no longer corrupt).
  - **Reserved envelope field names** (`rid`, `collection`, `kind`, `tenant`, `created_at`, `updated_at`) are enforced against user data at write time, with an error that names the field, the reserved set, and the rename recourse.
  - **The `DOCUMENT` marker is an optional idempotent model assertion**: unmarked `INSERT INTO c VALUES ({…})` infers the model from the catalog (existing document collection → document write; existing non-document → model error; unknown → didactic error), and `DOCUMENT` bootstraps-or-validates the collection's model.

  Published JS, js-client, and PHP/Python drivers flip to the inline form in this same release (no deprecation window — stated maintainer policy).

## 1.20.0

## 1.18.0

### Minor Changes

- minor release

## 1.17.0

### Minor Changes

- [#1517](https://github.com/reddb-io/reddb/pull/1517) [`f1f286d`](https://github.com/reddb-io/reddb/commit/f1f286ddfa034ad07826cd1bf75ab8941c21b2ac) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Harden release provenance with aggregate checksum manifests, GitHub Artifact
  Attestations, GHCR provenance/SBOM attestations, expanded release verification
  notes, and public support/release policy docs.

## 1.16.0

### Minor Changes

- testing maturity checkpoint

## 1.13.0

### Minor Changes

- [`99c9d9b`](https://github.com/reddb-io/reddb/commit/99c9d9bfa7ce40351d38d71cec8da752dd331745) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Centralize operational topology, monorepo contract authorities, client routing membership, queue lifecycle delivery, and parameterized statement execution.

## 1.12.0

### Minor Changes

- [#1308](https://github.com/reddb-io/reddb/pull/1308) [`a474ddb`](https://github.com/reddb-io/reddb/commit/a474ddb12045258a755d0c485974300838e6347a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Cluster bootstrap & operational telemetry groundwork.

  - **Cluster bootstrap authority**: fail-closed seam for cluster-shaped auth bootstrap ([#1229](https://github.com/reddb-io/reddb/issues/1229)), real auth-store wiring for cluster vault first boot ([#1231](https://github.com/reddb-io/reddb/issues/1231)), write-if-absent initial config on fenced bootstrap manifest apply ([#1232](https://github.com/reddb-io/reddb/issues/1232)), bootstrap-completion marker observed at boot through the authority seam ([#1230](https://github.com/reddb-io/reddb/issues/1230)), and cloud policy-first bootstrap manifest protections ([#1233](https://github.com/reddb-io/reddb/issues/1233)).
  - **Helm/Compose**: cluster bootstrap contract documented and render-checked; cluster members carry no bootstrap credentials, gated cluster auth/vault path with fail-closed messaging ([#1234](https://github.com/reddb-io/reddb/issues/1234)), plus duplicate/concurrent bootstrap drills ([#1235](https://github.com/reddb-io/reddb/issues/1235)).
  - **Durability**: collision-proof WAL backup temp paths so segment digests stay honest ([#1294](https://github.com/reddb-io/reddb/issues/1294)).
  - **Operational telemetry**: Phase-0 substrate contract (ADR 0060) defining the store/read-model boundary, retention/cardinality budgets, and redaction rules ahead of the metric slices ([#1247](https://github.com/reddb-io/reddb/issues/1247)).
  - **Process**: binding merge gate + green ratchet on `main` (ADR 0059, [#975](https://github.com/reddb-io/reddb/issues/975)).

## 1.11.0

### Minor Changes

- [#1206](https://github.com/reddb-io/reddb/pull/1206) [`6f29076`](https://github.com/reddb-io/reddb/commit/6f29076fe701822d0f576fa9cf49ec5d67b8f94a) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Centralize operational topology, monorepo contract authorities, client routing membership, queue lifecycle delivery, and parameterized statement execution.

## 1.10.1

## 1.10.0

## Unreleased

### Added

- `QueueLifecycle` gains three new methods on the canonical surface:
  `pop`, `delete_with_state`, `move_between_queues`
  ([#539 follow-up](https://github.com/reddb-io/reddb/issues/539),
  [#716](https://github.com/reddb-io/reddb/issues/716)). Lifecycle
  wrappers delegate to new `QueueStore` trait methods
  (`pop_available`, `delete_with_state`, `move_to_queue`) implemented
  on all three stores — `InMemoryQueueStore` (semantic implementation
  with proper tombstone routing), `PrimaryQueueStore` (shims to the
  existing `queue_delivery::*` MVCC-aware paths), and
  `ReplicaQueueStore` (returns `ReplicaImmutable`).

- Every queue command handler in `runtime/impl_queue.rs` (Pop, Peek,
  Purge, Read, Claim, Ack, Nack, Move) now routes through
  `QueueLifecycle` instead of `queue_delivery::*` directly
  ([#716](https://github.com/reddb-io/reddb/issues/716)). The lifecycle
  is the single funnel for every mutation on a queue; `queue_delivery`
  shrinks to an implementation detail of the primary store and will be
  fully inlined in a later slice. Wire output is byte-identical
  (existing transport tests unchanged).

- Domain vocabulary for **Live queue wait**, **Ephemeral notification**,
  and **Durable stream** + **ADR 0028** establishing the boundary
  between the three primitives so the queue lifecycle does not absorb
  incompatible semantics. First-cut `QUEUE READ ... WAIT` semantics
  scoped (autocommit-only, explicit duration, server-side max cap,
  Prometheus telemetry).

### Notes

- The `## Unreleased` entries below carried the NEON + QUEUE ACK
  delivery_id work shipped in v1.6.0; they have been moved under that
  release heading where they belong.

## 1.6.0

### Added

- TurboQuant NEON scoring kernel on the canonical blocked-by-32 layout
  ([#692](https://github.com/reddb-io/reddb/issues/692),
  [PRD #688](https://github.com/reddb-io/reddb/issues/688),
  [ADR 0024](.red/adr/0024-turboquant-encoded-storage-layout.md)). aarch64
  hosts now get SIMD scoring on TurboQuant — `select_scorer()` picks
  `NeonScorer` (NEON is mandatory in the AArch64 base ISA) and falls back to
  scalar on every other arch. The kernel reads aligned 128-bit lanes
  straight from `BlockedCodeStorage::block_codes(...)` with no per-query
  gather, uses `vqtbl1q_u8` for nibble lookups, and finishes with
  `vfmaq_f32` so output is bit-identical to `ScalarScorer` (the contract
  the AVX2/AVX-512BW slices already enforce). Equivalence test gated
  `#[cfg(target_arch = "aarch64")]`; release matrix already builds
  aarch64-gnu natively on a Blacksmith ARM runner and aarch64-musl via
  `messense/rust-musl-cross` so no new build deps are introduced.

- `QUEUE ACK` / `QUEUE NACK` accept the server-issued opaque `delivery_id`
  alongside the legacy `(queue, group, message_id)` tuple
  ([#627](https://github.com/reddb-io/reddb/issues/627),
  [ADR 0026](.red/adr/0026-delivery-id-wire-shape.md)). New RQL syntax:
  `QUEUE ACK <queue> [GROUP <group> '<message_id>'] [WITH delivery_id = '<base32>']`.
  The same wire shape applies to all four transports (redwire, gRPC,
  Postgres-wire, HTTP). When both handles are supplied, `delivery_id` wins
  unconditionally — a stale `delivery_id` returns an error instead of
  falling back to the tuple.

### Deprecated

- ACK/NACK by the legacy `(queue, group, message_id)` tuple. The tuple path
  still works in this release but emits a rate-limited server-side
  deprecation log line (per connection + queue, one entry per minute). The
  tuple path will be removed one minor release after this one — drivers
  should switch to sending `delivery_id`, which is already returned by
  `QUEUE READ` / `QUEUE CLAIM`.

## 1.5.0

### Minor Changes

- Policy-first auth, governance guardrails, NFS/build infra hardening, dep maintenance

  - feat(auth): policy-first evaluator — admin is no longer a bypass ([#644](https://github.com/reddb-io/reddb/issues/644))
  - feat(auth): managed policy guardrail tracer + system-owned user integration ([#646](https://github.com/reddb-io/reddb/issues/646), [#647](https://github.com/reddb-io/reddb/issues/647))
  - feat(auth): red.registry tracer + managed config namespace enforcement ([#648](https://github.com/reddb-io/reddb/issues/648), [#649](https://github.com/reddb-io/reddb/issues/649))
  - feat(auth): policy context for system-owned/platform-scoped users ([#645](https://github.com/reddb-io/reddb/issues/645))
  - feat(server): explicit --no-auth / --dev flag for local no-password mode ([#663](https://github.com/reddb-io/reddb/issues/663))
  - fix(engine): rid/logical-identity envelope regression + KV()-in-WHERE ([#636](https://github.com/reddb-io/reddb/issues/636))
  - fix(engine): parser unbounded recursion / OOM on view body + WITHIN ([#635](https://github.com/reddb-io/reddb/issues/635))
  - fix(engine): WAL recovery commit-batch replay, dotted-tenant scoping, $secret/$config SQL, ASK envelope fixes ([#638](https://github.com/reddb-io/reddb/issues/638), [#639](https://github.com/reddb-io/reddb/issues/639), [#640](https://github.com/reddb-io/reddb/issues/640), [#641](https://github.com/reddb-io/reddb/issues/641))
  - fix(gate): pnpm build now actually builds (was 90-min workspace-test); package.json scripts straightened out
  - chore(deps): bump grpc 1.79→1.81, protobuf 1.36.10→1.36.11, toml 0.8→1.1, criterion 0.5→0.8, lz4_flex, busybox

## 1.4.0

### Minor Changes

- a60b666: 1.4.0 minor release. Raises the storage engine page size from 4KB to 16KB
  (matching InnoDB's default) for higher B-tree fanout, and grows the maximum
  inline attribute value from 1024 to 4096 bytes (now derived as `PAGE_SIZE / 4`).

  **Breaking on-disk format change:** databases written by 1.3.x and earlier use
  4KB pages and will not open under 1.4.0 — there is no in-place migration. Treat
  this as a fresh-format release.

## 1.3.1

### Patch Changes

- [`cfce794`](https://github.com/reddb-io/reddb/commit/cfce794979cbfa68d16815974c70007dfa4bad16) Thanks [@filipeforattini](https://github.com/filipeforattini)! - 1.3.1 patch release. Re-publishes the 1.3.x line across all registries (npm,
  crates.io, GHCR, GitHub Release) after the 1.3.0 npm publish was blocked by a CI
  token issue. No functional change since 1.3.0 — the parser fix ([#635](https://github.com/reddb-io/reddb/issues/635)) and the
  `GRAPH COMMUNITY ... RETURN ASSIGNMENTS` feature ([#660](https://github.com/reddb-io/reddb/issues/660)) shipped in 1.3.0 are
  included here as well.

## 1.3.0

### Minor Changes

- [`162ed9b`](https://github.com/reddb-io/reddb/commit/162ed9bed3b7161d3e6e8f31de509eb376e16cf9) Thanks [@filipeforattini](https://github.com/filipeforattini)! - **Feature: per-node community assignment from `GRAPH COMMUNITY` ([#660](https://github.com/reddb-io/reddb/issues/660)).**
  `GRAPH COMMUNITY ALGORITHM louvain RETURN ASSIGNMENTS` now emits one row per
  node `{node_id, community_id}` — the node→community map needed to colour or
  visualise nodes by community. Without the `RETURN ASSIGNMENTS` clause the
  historical per-community aggregate shape (`community_id`, `size`) is unchanged
  (backward compatible). `LIMIT` caps the per-node rows.

- [#631](https://github.com/reddb-io/reddb/pull/631) [`b189b7e`](https://github.com/reddb-io/reddb/commit/b189b7eab9ea23553548007ec5106292a834e01f) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Promote the work merged since v1.2.5 to a minor release (1.3.0): new Analytics primitives + window functions are feature-level, so this is a minor, not a patch.

  **HTTP transport hardening ([#569](https://github.com/reddb-io/reddb/issues/569)).** Bounded handler concurrency via a deep
  `HttpConnectionLimiter` (hard cap → `503 Service Unavailable` + `Retry-After`
  on saturation, rejection before parse/routing), an overall per-handler
  wall-clock timeout that reclaims the limiter slot on expiry, the same shared
  cap enforced on the TLS accept loop (HTTP + HTTPS draw one cap), and Prometheus
  telemetry (`http_active_handler_threads`, `http_handler_rejected_total`,
  `http_handler_duration_seconds`, `http_handler_cap`). New config: `--http-max-handlers`,
  `--http-handler-timeout-ms`, `--http-retry-after-secs`.

  **QueueLifecycle foundations ([#527](https://github.com/reddb-io/reddb/issues/527) prereqs).** `QueueTxn` now participates in the
  caller's transaction via the runtime MVCC path so lifecycle ack/purge/delete are
  rollback-safe; the primary `QueueStore` adapter reads the legacy `queue_pending`
  state (closing the parallel meta-row divergence); and lifecycle gains
  `group_read` + `claim` methods that preserve the legacy `consumer`/`delivery_count`
  result shape. (The atomic cutover and the `delivery_id` wire handle remain
  follow-ups — the queue ACK/NACK contract is unchanged in this release.)

  **Analytics-event primitives ([#575](https://github.com/reddb-io/reddb/issues/575))** and **SDK Helper Spec v1.0 + cross-driver
  conformance ([#449](https://github.com/reddb-io/reddb/issues/449))** as previously merged on main.

  **Fix: embedding `linked_row_id` via rid envelope.** The row rid envelope
  refactor moved a query row's logical identity to the canonical `rid` key;
  `CLUSTER`/embedding writeback still keyed off the legacy `red_entity_id`
  column and silently dropped `linked_row_id`. Row→vector linkage is restored.

  Also documents that table column names persist across a file-backed reopen and
  that aggregate result columns use a single canonical `FUNC(arg)` casing —
  both verified by regression guards; these were 1.1.x-era reports already
  resolved on the 1.2 line.

### Patch Changes

- [`162ed9b`](https://github.com/reddb-io/reddb/commit/162ed9bed3b7161d3e6e8f31de509eb376e16cf9) Thanks [@filipeforattini](https://github.com/filipeforattini)! - **Fix: parser stack overflow + view filter desync ([#635](https://github.com/reddb-io/reddb/issues/635)).** Parsing a recursive
  view body could overflow the stack (the `parse_sql_command` match frame was
  oversized — extracted the CREATE arm to shrink it). Separately, the view
  rewriter merged the inner query's `filter`, but the executor prefers `where_expr`
  and nulled `filter` when present, so the merged predicate was silently dropped
  (`view_chain_resolves_recursively`); the rewriter now keeps `where_expr` in sync
  with the merged filter. Re-enables the previously quarantined view/materialized-view
  parser binaries ([#593](https://github.com/reddb-io/reddb/issues/593)/[#594](https://github.com/reddb-io/reddb/issues/594)/[#595](https://github.com/reddb-io/reddb/issues/595)/[#596](https://github.com/reddb-io/reddb/issues/596)/views).

## Unreleased

### Documentation

- New canonical SDK Helper Spec at `docs/spec/sdk-helpers.md` (v1.0). Defines
  the helper names (snake_case dot-namespaced), input shapes, output
  envelopes, error taxonomy, and per-model conformance cases for documents,
  KV, queues, transactions, vectors, graph, time-series, and probabilistic
  surfaces. The previous v0.1 draft at `docs/clients/sdk-helper-spec.md`
  now points at the canonical spec. A reference conformance harness ships
  in `crates/reddb-client/tests/conformance.rs`; other-language drivers
  port the case IDs verbatim. Refs #546.

## 1.2.0 - 2026-05-15

### Fixed

- Server and client Docker images now use `gcr.io/distroless/cc-debian13`
  so the glibc 2.39 binaries produced by the Ubuntu 24.04 release builders
  run on the runtime base. Prior 1.1.x images failed at startup with
  `version 'GLIBC_2.39' not found`.

### Breaking Changes

- Public item identity now uses the canonical RedDB ID field `rid` across
  rows, documents, KV, graph nodes, graph edges, vectors, events, CDC,
  HTTP/gRPC/MCP, and SDK-visible query results. Older public aliases such as
  `_entity_id`, `red_entity_id`, and `entity_id` are removed from public
  envelopes; graph edge endpoints are `from_rid` and `to_rid`.
- The item envelope fields `rid`, `collection`, `kind`, `tenant`,
  `created_at`, and `updated_at` are reserved system fields. New table schemas
  and supported top-level document, KV, node, and edge payloads that define
  those names fail with a reserved-field error.
- Multi-model updates are now explicit through `ROWS`, `DOCUMENTS`, `KV`,
  `NODES`, and `EDGES` targets. Compound assignment, `RETURNING`, and ordered
  `ORDER BY ... LIMIT` batches are documented as the canonical update surface;
  failed multi-item updates abort atomically without partial writes.
- Unaliased expression projections now use the rendered source expression as the
  result column name instead of the internal operator/function tag. For example,
  `SELECT UPPER(name)` now returns `UPPER(name)` and `SELECT id * 2` now returns
  `id * 2`, while explicit `AS <alias>` names still take precedence.

## 1.1.0

### Minor Changes

- [`9bef862`](https://github.com/reddb-io/reddb/commit/9bef862babe56cfbc75850c94c2b8f863a6fd8de) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Prepare the 1.1 line with parameterized query coverage across engine, transports, and drivers; ASK citation, streaming, cache, failover, audit, and gRPC/MCP surfaces; MVCC transaction recovery improvements; graph/vector/probabilistic query fixes; SDK helper APIs; and release asset hardening.

## 1.0.8

### Patch Changes

- [`c92f9e7`](https://github.com/reddb-io/reddb/commit/c92f9e7d386b459107a60a0796a25fce2c23ffc5) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Bundle of work that landed since `v1.0.7`.

  **Query / engine**

  - `SELECT … LIMIT $N OFFSET $N` parameterized — completes the `$N` bind sweep (`closes [#361](https://github.com/reddb-io/reddb/issues/361)`).
  - ASK responses now include inline `[^N]` citation markers, parsed by a dedicated `CitationParser` (`closes [#393](https://github.com/reddb-io/reddb/issues/393)`).

  **Drivers**

  - **Go.** `c.Query(ctx, sql, params...)` accepts variadic `any` bind values, routed through the binary `QueryWithParams` frame when the server advertises `FEATURE_PARAMS`. Full Go → engine type mapping documented in the driver hub page (`closes [#363](https://github.com/reddb-io/reddb/issues/363)`).
  - **JS / TS.** RedWire `QueryWithParams` frame emit + `FEATURE_PARAMS` capability negotiation in the SDK and thin client.

  **Infrastructure**

  - `lru` bumped from `0.12` → `0.16`, picking up the upstream fix for the `IterMut` Stacked Borrows UB flagged by Dependabot (alerts [#19](https://github.com/reddb-io/reddb/issues/19), [#20](https://github.com/reddb-io/reddb/issues/20)).
  - CI workflows replace `arduino/setup-protoc@v3` (Node 20-deprecated) with a local `install-protoc` composite action that downloads protoc 28.3 directly. Eliminates the deprecation warning on every job.
  - Dropped the unsupported `save-always: true` input from `Swatinem/rust-cache@v2` (silently ignored but emitted a warning per build).

## 1.0.7

### Patch Changes

- [`da708d9`](https://github.com/reddb-io/reddb/commit/da708d979ed3a2289dd5da985db48879504d6b94) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Skips the stalled `v1.0.6` release. This bundles everything that landed since `v1.0.5` plus the infrastructure that makes future releases atomic.

  **Query / engine**

  - `$N` positional parameters across the query language (INSERT VALUES, SEARCH SIMILAR, SEARCH HYBRID, SEARCH TEXT, SEARCH MULTIMODAL, SEARCH SPATIAL, SEARCH INDEX — `LIMIT $N`, `MIN_SCORE $N`, `K $N`).
  - `?` positional placeholders in the parser, with mixing detection.
  - HTTP `/query` now accepts a `params` JSON array.

  **Distribution & docs**

  - Workspace crates published under the `reddb-io-*` namespace on crates.io (`reddb-io`, `reddb-io-client`, `reddb-io-server`, `reddb-io-wire`, `reddb-io-grpc-proto`, `reddb-io-client-connector`). Rust library import paths (`use reddb::…`, `use reddb_client::…`, etc.) are unchanged.
  - Per-language driver pages under `docs/clients/drivers/` (rust, python, python-asyncio, go, php, dart, cpp, zig, bun) plus a hub matrix at `docs/clients/drivers.md`.

  **Release pipeline**

  - macOS Intel binaries (`red-macos-x86_64`, `red_client-macos-x86_64`) are now produced by the release matrix.
  - Adopted [Changesets](https://github.com/changesets/changesets) for atomic version + release: the version bump and the GitHub Release tag are now produced by CI in a single step, eliminating the race that caused `@reddb-io/sdk`'s postinstall to 404 in the window between a local `pnpm version` and the release workflow finishing.
  - `postinstall` scripts now (a) print actionable recovery paths when an asset 404s and (b) skip cleanly when running from a workspace checkout (`pnpm install` in the monorepo no longer surfaces a download error).

All notable changes to this project are documented here. Versions follow
[SemVer](https://semver.org/) and the engine + locked-step npm/PyPI
packages share one number (see `scripts/check-versions.sh`).

## 1.0.0 — 2026-05-06

First stable release. Engine, drivers, and CLI move to the `@reddb-io/*`
npm org and lock at `1.0.0`.

### npm migration (#125)

- `@reddb-io/cli` — CLI launcher (was `reddb-cli`).
- `@reddb-io/sdk` — JS/TS driver (RedWire/gRPC/HTTP/embedded).
- `@reddb-io/client` — thin remote-only driver, downloads `red_client`
  binary post-install (#136, #137, #138).
- `scripts/sync-version.js` propagates engine bumps to all locked-step
  packages including `drivers/js-client/`.
- `scripts/check-versions.sh` enforces lock-step in CI.
- Legacy `reddb-cli` npm package deprecated post-publish via
  `scripts/deprecate-legacy-npm.sh` (operator-triggered, see
  `docs/release-runbook.md`).

### Engine

- UnifiedRecord storage layout (perf push closure).
- WAL lock-free append path.
- Pager striped lock to remove the global page-cache contention.
- BTree batch ingest path.
- AggregateQueryPlanner for push-down aggregates.
- IncrementalIndex maintenance.
- AI batching final cleanup (#281): synchronous `openai_embeddings`,
  `openai_prompt`, and `anthropic_prompt` Rust helpers are deprecated.
  Internal server paths now use `AiBatchClient` or the pooled
  `AiTransport` path. The mock-provider AUTO EMBED validation for 1000
  rows drops provider fan-out from 1000 requests to 1 request when the
  provider max batch is 2048, reducing the provider-wait component from
  about 100s to about 0.1s at 100ms provider latency.

### Topology (#164)

- Topology discovery PRD substantially shipped: advertiser, consumer, and
  router landed; E2E coverage in flight (#172).
- Strategic gate ADR 0009 — gate is scenario-specific, not global.

### Out of scope for this slice

- The unscoped `reddb` npm name is owned by an unrelated upstream package
  and is not deprecated.
- `drivers/python-asyncio` and `charts/reddb` follow independent version
  policies.
