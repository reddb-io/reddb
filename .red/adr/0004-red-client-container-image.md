# ADR 0004 — Ship `red_client` as a separate container image

**Status:** Accepted
**Date:** 2026-05-05
**Supersedes:** —
**Superseded by:** —
**Related issues:** [#68](https://github.com/reddb-io/reddb/issues/68)

## Context

RedDB's main container image (`ghcr.io/reddb-io/reddb`) ships the full
`red` binary: server, embedded engine, gRPC + HTTP + RedWire endpoints,
plus a Debian-slim runtime layer with `tini`, `curl` for healthchecks,
and a non-root `reddb` user. The resulting image is around 50 MB.

The `red_client` thin client (introduced for CI runners, sidecars, and
ops tooling that connect to a remote RedDB) is a single ~4.5 MB
stripped binary. Bundling it into the main image would mean operators
who only need the client always pull the full server runtime — `tini`,
`curl`, the engine binary, the HEALTHCHECK, the data volumes — none of
which the client uses.

## Decision

Ship `red_client` as a **separate** container image:
`ghcr.io/reddb-io/reddb-client:<version>` (and `:latest` on stable
releases), built from a sibling `Dockerfile.client` at the repo root.

- Builder stage matches the main `Dockerfile` (Rust 1.91 slim-bookworm
  + `protobuf-compiler`) for tooling consistency.
- Runtime stage is `gcr.io/distroless/static-debian12:nonroot`, which
  provides `/etc/ssl/certs/ca-certificates.crt` (so HTTPS transport
  works) and a non-root user, but no shell, package manager, or other
  runtime weight.
- Entrypoint is `/red_client`. No CMD — operators always pass a URI
  and a query.

Operators choose:

- **Thin client image** when only `red_client` is needed (CI jobs,
  Kubernetes sidecars, ops scripts). Target size: < 10 MB.
- **Full server image** (`ghcr.io/reddb-io/reddb`) when running the
  engine.

## Alternatives considered

- **Bundle `red_client` inside the main image.** Rejected: defeats the
  thin-client goal. A 50 MB image to run a 4.5 MB binary is a 10×
  overhead, and it forces the engine's healthcheck, ports, volumes,
  and user model on workloads that don't need any of them.
- **Single image with both binaries on a distroless base.** Rejected:
  the engine's runtime needs (`tini`, `curl` for healthcheck, secret
  shim entrypoint, data volume ownership) don't fit cleanly in
  `distroless/static`, and pushing the engine onto distroless is a
  larger change than this ADR scopes.

## Consequences

- Two images to publish per release; release.yml gains a
  `publish-client-image` job parallel to `publish-docker`.
- Operators referring to the client by image name need the new
  `-client` suffix; documented in README and release notes.
- The client image inherits whatever Rust toolchain pin the main
  Dockerfile uses, so the two stay in lockstep.
