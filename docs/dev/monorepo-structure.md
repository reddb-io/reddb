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

## Published Package Names

Public package names carry the `reddb-io` identity across registries:

- The Rust umbrella crate is published as `reddb-io`.
- Every other published Rust workspace crate must use the `reddb-io-*`
  crates.io prefix, for example `reddb-io-wire`, `reddb-io-file`,
  `reddb-io-rql`, `reddb-io-types`, and `reddb-io-server`.
- npm packages publish under `@reddb-io/*`, including `@reddb-io/sdk`,
  `@reddb-io/client`, `@reddb-io/cli`, and `@reddb-io/internal-*`.

Rust import names stay idiomatic and use underscores because hyphens are not
valid Rust identifiers: `reddb-io-wire` imports as `reddb_wire`,
`reddb-io-file` as `reddb_file`, and so on.

Do not introduce public Rust packages named `reddb-*` or short package names
such as `red-rql`; they break the registry naming pattern and make the crate
graph harder to scan.

## Authority Crates

Authority crates are the single home for formats, vocabularies, and contracts
shared across runtimes or languages. Each owns a correctness-critical surface so
that surface is declared once, never redeclared opportunistically inside
`reddb-server`. They form an acyclic layer *below* the server: every authority
crate may depend on the keystone, but nothing depends back on `reddb-server`.

- `crates/reddb-types` (`reddb-io-types`) is the **neutral keystone** at the
  bottom of the graph. It owns the logical type vocabulary — `Value`,
  `DataType`, `SqlTypeName`, `TypeModifier`, `TypeCategory`, `ValueError`, `Row`
  — plus the `value_codec` serialization those types delegate to. It depends on
  no other workspace crate; every other authority crate depends on it. See
  ADR 0052.
- `crates/reddb-rql` (`reddb-io-rql`) owns the **RQL language front-end** and the
  **conformance corpus**: lexer, parser, AST, the mode translators (SQL,
  Gremlin, Cypher, SPARQL, Path, Natural, vector extensions), analyzer,
  expression typing, and the storage-agnostic optimizer — text in, typed logical
  plan out. The physical executors stay in `reddb-server`; this crate does not
  execute queries. Its sqllogictest-format conformance suite (truth from the
  public SQLite corpus plus RedDB-authored goldens) is the correctness
  specification CI runs against the server engine. Depends only on
  `reddb-io-types`. See ADR 0053.
- `crates/reddb-crypto` (`reddb-io-crypto`) owns the **per-page
  encryption-at-rest envelope**: the canonical `encrypt_page` / `decrypt_page`
  byte-format (AES-256-GCM), the fixed `params` (key/nonce/tag sizes, overhead,
  AEAD name), and key parsing (`parse_key`, hex or base64). The page-0
  "is-this-database-encrypted" header (`RDBE` marker, salt, key-check) stays in
  `reddb-file`. See ADR 0054.
- `crates/reddb-wire` (`reddb-io-wire`) owns **communication contracts**: RedWire
  frames, payloads, topology advertisements, connection strings, sanitizers, and
  replication wire messages.
- `crates/reddb-file` (`reddb-io-file`) owns **persistence contracts**: file
  names, layouts, manifests, WAL envelopes, snapshots, checkpoints, the page-0
  paged-encryption header, and recovery metadata.
- `crates/reddb-grpc-proto` (`reddb-io-grpc-proto`) owns the **generated gRPC
  stubs** (tonic server + client), reusing canonical wire/topology types where
  applicable.

Runtime code may adapt these contracts, but should not redeclare protocol, file,
type, query, or crypto shapes locally. `reddb-server` keeps thin delegating
facades over each authority crate (for example `crypto::page_encryption`, which
re-exports the envelope and adds the server-only `key_from_env`); per ADR 0046 a
facade carries no second format. See ADR 0046 for the umbrella authority rule and
ADRs 0052–0054 for the type, query, and crypto crates respectively.

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
