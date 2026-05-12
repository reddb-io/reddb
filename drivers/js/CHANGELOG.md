# @reddb-io/sdk

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
