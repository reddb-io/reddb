# RedDB conformance fixtures

This directory is the shared fixture root for contracts consumed across
multiple packages and language drivers.

Fixtures here are not owned by an individual driver. A contract crate owns the
encoding or format, and every adapter consumes the same fixture file.

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
