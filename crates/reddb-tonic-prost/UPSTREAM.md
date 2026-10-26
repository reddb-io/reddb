# Tonic compatibility distribution

Upstream package: `tonic-prost 0.14.6`.
Source: https://static.crates.io/crates/tonic-prost/tonic-prost-0.14.6.crate
Upstream crate archive SHA-256: `50849f68853be452acf590cde0b146665b8d507b3b8af17261df47e02c209ea0`.
Upstream Git revision is retained in `.cargo_vcs_info.json`.

This package is published as `reddb-io-tonic-prost` under the RedDB release version.
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

The Prost archive omits a standalone license file; LICENSE is the same upstream
project MIT license carried by the verified tonic 0.14.6 archive.

The original upstream manifest is retained as UPSTREAM.Cargo.toml. Cargo reserves
Cargo.toml.orig for its own publication-time normalized manifest metadata.

The manifest permits the cosmetic manual_is_multiple_of Clippy lint in the
upstream tests, retaining their original Rust sources unchanged.
