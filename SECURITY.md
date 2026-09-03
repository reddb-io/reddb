# Security policy

## Reporting a vulnerability

Email **security@reddb.io** privately. Do not file a public GitHub issue for security-impacting bugs.

You can also use [GitHub Security Advisories](https://github.com/reddb-io/reddb/security/advisories) for private coordinated disclosure.

We acknowledge reports within **48 business hours**, confirm or reject within 5 business days, and target a fix or mitigation within **90 days** for non-critical issues. Critical issues (data loss, auth bypass, RCE) are prioritized and patched as fast as we can ship and verify a fix.

## Scope

In scope:

- The RedDB engine (`/src`, `/wal`, `/drivers/*`).
- Official Docker images under `ghcr.io/reddb-io/reddb`.
- Helm chart under `/charts/reddb`.
- The managed Cloud at `*.reddb.io`.

Out of scope:

- Third-party AI provider APIs (report to the provider).
- User misconfiguration (e.g. exposing port 5000 to the public internet without auth).
- Social engineering, physical attacks, denial-of-service against managed Cloud.

## Coordinated disclosure

We follow coordinated disclosure: we ask reporters to keep findings private until a fix is available or 90 days elapse, whichever comes first. We will publicly credit reporters in the CHANGELOG and on https://reddb.io/security unless they prefer to stay anonymous.

## Hardening notes for self-hosters

- Run RedDB behind a reverse proxy with TLS terminated. The bundled HTTP server supports TLS but most operators prefer to terminate at the proxy.
- Set `REDDB_AUTH=true` and `REDDB_REQUIRE_AUTH=true` in production (the official images set both). Anonymous access is convenient for local development only; `REDDB_NO_AUTH=true` opts out explicitly.
- Set `RED_ADMIN_TOKEN` for the operator surface (`/admin/*`, `/metrics`). Without it those routes require an admin-role bearer from the user auth store.
- Use the `vault` module for AI provider keys instead of plaintext config.
- Enable WAL fsync (default). Only disable on disposable nodes where replay-from-source is acceptable.
- Restrict the admin API (`/v1/admin`) to a private network or behind SSO.
- Set `REDDB_BOOTSTRAP_TOKEN` (or `REDDB_BOOTSTRAP_TOKEN_FILE`) on any store whose bootstrap endpoint is reachable over the network. Bootstrap is first-caller-wins; with the token set, `POST /auth/bootstrap` and the gRPC `AuthBootstrap` call must present it in `x-reddb-bootstrap-token`.
- Terminate TLS on every listener that is not bound to loopback. The server warns at boot for each plaintext listener reachable beyond the host.

## Telemetry and outbound connections

RedDB has no telemetry, usage reporting or update check: the server never contacts a RedDB-operated endpoint. The only outbound connections it opens are the ones an operator configures — AI providers (`red.config.ai.*`), the audit-log and operator-event webhook sinks, replication peers, remote backup backends, and the `red ui` bundle download — plus image references that a vision-enrichment policy reads from row data, which are subject to the same egress rules as AI providers (`https` only off loopback, no private or link-local addresses unless `REDDB_AI_ALLOW_PRIVATE_PROVIDERS=1`; local files only with `REDDB_AI_VISION_ALLOW_LOCAL_FILES=1`).

## Security audit

The engine is source-available under the Business Source License 1.1. To audit:

```sh
git clone https://github.com/reddb-io/reddb.git
cd reddb
cargo audit              # dependency CVEs
cargo test --workspace   # baseline correctness
```

Pointers for a focused review:

- Storage and durability: `crates/reddb-file`, `crates/reddb-server/src/storage`.
- Auth and tokens: `crates/reddb-server/src/auth` (vault in `auth/vault.rs`).
- Network surface: `crates/reddb-server/src/server`, `crates/reddb-server/src/grpc`, `crates/reddb-server/src/wire`.
- Dependency policy: `Cargo.lock` is committed; `cargo audit` runs in CI.

For a longer write-up — encryption, backups, isolation, compliance roadmap — see https://reddb.io/security.
