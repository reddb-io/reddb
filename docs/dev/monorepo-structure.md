# Monorepo Structure

RedDB uses one repository for the Rust product, contract crates, language
drivers, package helpers, docs, examples, and conformance fixtures. The goal is
locality: changes to a contract should be made once, then verified through every
adapter that consumes it.

## Product

- Root `Cargo.toml` is the Cargo workspace and umbrella `reddb-io` package.
- `src/bin/red.rs` is the current product binary entrypoint.
- `crates/reddb-server` owns the server-side runtime, storage orchestration,
  transport listeners, replication runtime, and operational behavior.
- `crates/reddb-server/src/operational_bootstrap.rs` owns the boot contract
  that turns topology/node-role/config/storage env and CLI input into a runtime
  boot plan. `src/bin/red.rs`, Helm, and Compose should compile toward that
  contract instead of redeclaring precedence locally.

The root still hosts the `red` binary for compatibility. If the entrypoint grows
more bootstrap behavior, prefer moving that behavior into a deep module first;
move the binary into a dedicated `crates/reddb-cli` or `apps/red` package only
when the package split removes real complexity.

## Contract Modules

Contract modules are the authority for formats shared across runtimes or
languages:

- `crates/reddb-wire` owns communication contracts: RedWire frames, payloads,
  topology advertisements, connection strings, sanitizers, and replication wire
  messages.
- `crates/reddb-file` owns persistence contracts: file names, layouts,
  manifests, WAL envelopes, snapshots, checkpoints, and recovery metadata.
- `crates/reddb-rql` owns the RQL front-end and conformance corpus.
- `crates/reddb-types` owns neutral logical values and type vocabulary.
- `crates/reddb-grpc-proto` owns generated gRPC stubs while reusing canonical
  wire/topology types where applicable.

Runtime code may adapt these contracts, but should not redeclare protocol or
file shapes locally. See ADR 0046 for the wire/file authority rule.

## Adapters

Adapters sit at a seam and translate between a contract module and an external
runtime:

- `drivers/<language>` contains official language drivers.
- `drivers/js`, `drivers/js-client`, and `drivers/bun` are driver/package
  adapters with different distribution contracts.
- `packages/internal-*` contains private npm helper packages for release asset
  fetching, binary resolution, and version comparison.

Adapters should consume shared fixtures from `testdata/conformance` when the
contract is also consumed outside Rust. Avoid creating driver-local copies of
wire, file, RQL, or value fixtures.

Current JavaScript/npm layout:

| Path | Package | Role |
| --- | --- | --- |
| root `package.json` | `@reddb-io/cli` | npm launcher for the `red` binary |
| `drivers/js` | `@reddb-io/sdk` | embedded JS/TS SDK that can launch a local engine |
| `drivers/js-client` | `@reddb-io/client` | thin remote-only JS/TS driver |
| `drivers/bun` | `@reddb-io/client-bun` | Bun-native RedWire client |
| `packages/internal-*` | `@reddb-io/internal-*` | private release/install helpers |

Keep those paths stable until a release-window migration is planned. Future npm
normalization should prefer `packages/js/*` for npm packages while preserving
`drivers/<language>` for protocol-driver source and tests.

## Shared Fixtures

`testdata/conformance` is the stable fixture root for cross-language contract
tests. For example, `testdata/conformance/redwire/params/manifest.json` is read
by `reddb-wire`, the Rust client gRPC tests, and the official language drivers.

Use this rule of thumb:

- crate-private behavior: keep fixtures under the crate's own tests.
- cross-driver contract behavior: put fixtures under `testdata/conformance`.
- public support promises: describe them in `docs/conformance` and gate them
  with the contract matrix.

## Workspace Governance

The root manifest centralizes shared Cargo package metadata and internal path
dependencies through `[workspace.package]` and `[workspace.dependencies]`.
Individual crates keep their own `version`, `description`, `documentation`,
`keywords`, and `categories` because release cadence and audience differ between
product crates and contract crates.
