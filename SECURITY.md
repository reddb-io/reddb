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
- User misconfiguration (e.g. exposing port 8080 to the public internet without auth).
- Social engineering, physical attacks, denial-of-service against managed Cloud.

## Coordinated disclosure

We follow coordinated disclosure: we ask reporters to keep findings private until a fix is available or 90 days elapse, whichever comes first. We will publicly credit reporters in the CHANGELOG and on https://reddb.io/security unless they prefer to stay anonymous.

## Hardening notes for self-hosters

- Run RedDB behind a reverse proxy with TLS terminated. The bundled HTTP server supports TLS but most operators prefer to terminate at the proxy.
- Set `RED_AUTH_REQUIRED=true` in production. Anonymous access is convenient for local development only.
- Use the `vault` module for AI provider keys instead of plaintext config.
- Enable WAL fsync (default). Only disable on disposable nodes where replay-from-source is acceptable.
- Restrict the admin API (`/v1/admin`) to a private network or behind SSO.

## Security audit

The engine is AGPL-3.0. To audit:

```sh
git clone https://github.com/reddb-io/reddb.git
cd reddb
cargo audit              # dependency CVEs
cargo test --workspace   # baseline correctness
```

Pointers for a focused review:

- Storage and durability: `/wal`, `/src/storage`.
- Auth and tokens: `/src/auth`, `/src/vault`.
- Network surface: `/src/server/http`, `/src/server/grpc`.
- Dependency policy: `Cargo.lock` is committed; `cargo audit` runs in CI.

For a longer write-up — encryption, backups, isolation, compliance roadmap — see https://reddb.io/security.
