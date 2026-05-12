---
"@reddb-io/cli": patch
"@reddb-io/sdk": patch
"@reddb-io/client": patch
"@reddb-io/client-bun": patch
---

Skips the stalled `v1.0.6` release. This bundles everything that landed since `v1.0.5` plus the infrastructure that makes future releases atomic.

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
