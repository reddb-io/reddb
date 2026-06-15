# ADR 0046 — Wire and file crate authority boundary

Status: accepted

RedDB has two correctness-critical serialization surfaces that must not be
redeclared opportunistically inside runtime code:

- communication contracts crossing a process/network boundary;
- file contracts crossing a persistence/recovery boundary.

## Decision

`reddb-wire` owns communication contracts. New frame layouts, frame codecs,
message kinds, payload envelopes, topology payloads, connection-string parsing,
sanitizers, routing hints, queue/stream envelopes, and replication wire messages
belong in `crates/reddb-wire`.

`reddb-file` owns file contracts. New path derivation, artifact names, sidecar
names, manifests, superblocks, WAL segment/envelope rules, checkpoint metadata,
basebackup/relay/timeline artifacts, repair markers, and recovery metadata
belong in `crates/reddb-file`.

`reddb-server` orchestrates runtime behavior. It may validate auth, execute SQL,
apply policy, own storage-engine semantics, and adapt internal types to the
contracts above. It must not introduce new persisted file formats or binary/wire
payload formats directly.

Cross-language fixtures for these contracts live under `testdata/conformance`.
The contract crate owns the encoding or format; Rust tests and official language
drivers consume the same fixture files instead of copying golden data into each
driver.

## Rules

- A new transport-visible payload or protocol discriminator starts in
  `reddb-wire`; server/client code imports it.
- A new disk-visible artifact starts in `reddb-file`; server code imports its
  path, name, encode/decode, checksum, and recovery metadata helpers.
- Runtime-only adapters can live in `reddb-server`, but only after the wire/file
  boundary has already parsed or produced the external contract.
- Compatibility shims are allowed only when they delegate to the canonical crate
  and do not carry a second frame, payload, path, manifest, or WAL format.
- A fixture that proves behavior across Rust and non-Rust adapters belongs under
  `testdata/conformance`, with the owning contract crate documented next to the
  fixture root.
- Official language drivers may translate contract values into local idioms, but
  must not fork protocol discriminators, binary layouts, file artifact names, or
  golden fixtures.

## Consequences

- `reddb-server` stays large but less ambiguous: it is the coordinator, not the
  authority for external contracts.
- Tests may grep for forbidden redeclarations in `reddb-server` and client
  adapters. These tests are architectural guardrails, not style preferences.
- Moving an existing contract into `reddb-wire` or `reddb-file` is behavior
  preserving unless the old local implementation was already divergent.
- Adding a cross-driver contract fixture should update the contract crate test
  first, then each adapter that claims support for that contract.

## Related

- ADR 0001 — RedWire TCP protocol
- ADR 0010 — Wire adapters translate, never duplicate
- ADR 0032 — WAL source of truth and term framing
- ADR 0042 — Operational manifest and DDL recovery
- ADR 0043 — Operational backup/restore boundary
