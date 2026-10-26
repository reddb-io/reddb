# Tonic compatibility distribution

Upstream package: `tonic 0.14.6`.
Source: https://static.crates.io/crates/tonic/tonic-0.14.6.crate
Upstream crate archive SHA-256: `ac2a5518c70fa84342385732db33fb3f44bc4cc748936eb5833d2df34d6445ef`.
Upstream Git revision is retained in `UPSTREAM.vcs.json`.

This package is published as `reddb-io-tonic` under the RedDB release version.
Its Rust library name is unchanged. The Rust sources and MIT license are copied
without changes. RedDB's BUSL license does not replace this package's MIT license.
Only package metadata, dependency manifests and repository-local formatting
configuration differ. The runtime requires zstd >=0.14.3, and the Prost codec
uses the same compatibility runtime. Build/code-generation crates remain upstream.

Consumers of RedDB's generated stubs can import the matching runtime through
`reddb_grpc_proto::tonic`. Separately depending on upstream `tonic` creates a
different Rust type identity; use the re-export or alias this package as `tonic`.
The gRPC protocol and compression encodings remain the upstream implementation.

For an upstream update, replace both compatibility packages together, retain the
licenses and archive provenance, update the manifests, and rerun audit, transport
compression, driver, package and release-contract checks. Remove the copies only
when a published upstream version supports the fixed zstd binding and all
consumers can migrate together.

The original upstream manifest is retained as UPSTREAM.Cargo.toml. Cargo reserves
Cargo.toml.orig for its own publication-time normalized manifest metadata.

The manifest permits the cosmetic collapsible_if Clippy lint introduced by the
repository toolchain, retaining the verified upstream Rust sources unchanged.

Cargo generates its own .cargo_vcs_info.json when publishing. UPSTREAM.vcs.json
keeps the original upstream commit visible in the published package as well.
