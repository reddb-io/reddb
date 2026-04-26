# Cargo Feature Matrix

RedDB keeps the default crate lean. Release CI compiles every supported combination below so operators do not discover feature drift in production.

| Feature set | CI command | Purpose |
|-------------|------------|---------|
| no default features | `cargo check --locked --no-default-features` | Baseline engine build. |
| `otel` | `cargo check --locked --no-default-features --features otel` | OpenTelemetry scaffolding. |
| `backend-s3` | `cargo check --locked --no-default-features --features backend-s3` | S3-compatible backend, including lease CAS via ETag. |
| `backend-turso` | `cargo check --locked --no-default-features --features backend-turso` | Turso/libSQL backend integration. |
| `backend-d1` | `cargo check --locked --no-default-features --features backend-d1` | Cloudflare D1 backend integration. |
| all features | `cargo check --locked --all-features` | Release guard against feature interaction breakage. |

Notes:

- There is no separate `crypto` feature in the current crate. Crypto primitives used by WAL hashing, HMAC, and page-encryption foundation compile in the baseline build.
- There is no separate `replication` feature in the current crate. Replication and serverless fencing are part of the baseline engine.
- S3 writer leases require `backend-s3`; HTTP writer leases require `RED_HTTP_CONDITIONAL_WRITES=true` and an HTTP service that supports ETag + `If-Match`.
