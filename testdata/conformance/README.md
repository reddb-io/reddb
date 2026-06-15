# RedDB conformance fixtures

This directory is the shared fixture root for contracts consumed across
multiple packages and language drivers.

Fixtures here are not owned by an individual driver. A contract crate owns the
encoding or format, and every adapter consumes the same fixture file.

The machine-readable authority index is
[`contract-authorities.json`](contract-authorities.json). New files under this
directory must be listed there so release tooling knows which contract owns
them.

Current shared fixtures:

- `redwire/params/manifest.json` — RedWire query-parameter value and query
  encoding fixtures. `reddb-wire` validates the canonical RedWire bytes,
  `reddb-grpc-proto` consumers validate the gRPC bytes, and language drivers
  validate their adapters against the same manifest.

Rules:

- Put cross-driver fixtures here, not under `drivers/*`.
- Put protocol fixtures here, not under a crate's private `tests/fixtures`
  directory, when non-Rust adapters also consume them.
- Keep fixture paths stable; CI driver conformance jobs read these files
  directly.

Current authorities:

| Contract | Authority |
| --- | --- |
| Public surface support matrix | `docs/conformance/public-surface-contract-matrix.json` |
| SDK Helper Spec | `docs/spec/sdk-helpers.md` |
| SDK helper reference harness | `crates/reddb-client/tests/conformance.rs` |
| Standard RQL corpus | `crates/reddb-rql/tests/corpus` |
| RedDB-only RQL corpus | `crates/reddb-rql/tests/reddb_corpus` |
| RQL result rendering | `crates/reddb-rql/src/conformance.rs` |
| RedWire param/value fixtures | `testdata/conformance/redwire/params/manifest.json` |
| RedWire protocol fence | `crates/reddb-wire/tests/protocol_authority.rs` |
| File layout fence | `crates/reddb-file/tests/layout_authority.rs` |
| File layout fence modules | `crates/reddb-file/tests/layout_authority` |
