# Changelog

## Unreleased

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
