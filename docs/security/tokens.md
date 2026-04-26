# Auth Methods, Tokens & Keys

RedDB supports six authentication paths, each suited to a different
deployment shape:

| Method | Best fit | Wire transports | RTT cost |
|--------|----------|-----------------|----------|
| API key (`rdb_k_*`) | service accounts, CI | HTTP, gRPC, RedWire v2 | 0 (header) |
| Session token (`rdb_s_*`) | interactive logins | HTTP, gRPC, RedWire v2 | 1 RTT to mint |
| SCRAM-SHA-256 | drivers wanting RFC 5802 challenge/response without TLS | RedWire v2, PG wire | 3 RTTs |
| OAuth / OIDC JWT | external IdP federation | HTTP, gRPC, RedWire v2 | 0 (header) |
| HMAC-signed request | tamper-evident replay-protected calls | HTTP, gRPC, RedWire v2 | 0 (header) |
| Client certificate (mTLS) | zero-trust mesh | HTTPS, RedWire v2 + TLS | 0 (TLS handshake) |

The RedWire v2 handshake (`Hello` → `HelloAck`) advertises the
methods the server has enabled, so drivers pick the strongest one
without an extra probe round-trip.

## API Keys

API keys are persistent credentials tied to a user and role. They don't expire until revoked.

### Create an API Key

```bash
# Via CLI
red auth create-api-key alice --name "ci-token" --role write

# Via HTTP
curl -X POST http://127.0.0.1:8080/auth/api-keys \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"username": "alice", "name": "ci-token", "role": "write"}'
```

Response:

```json
{
  "ok": true,
  "key": "rdb_k_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "name": "ci-token",
  "role": "write",
  "username": "alice"
}
```

> [!WARNING]
> The full API key is only shown once. Store it securely.

### Using an API Key

```bash
curl http://127.0.0.1:8080/collections/users/scan \
  -H 'Authorization: Bearer rdb_k_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
```

### Revoking an API Key

```bash
grpcurl -plaintext \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"payloadJson": "{\"key\":\"rdb_k_xxxx\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/AuthRevokeApiKey
```

## Session Tokens

Session tokens are obtained by logging in and expire after the session ends.

### Login

```bash
# Via CLI
red auth login alice --password secret

# Via HTTP
curl -X POST http://127.0.0.1:8080/auth/login \
  -H 'content-type: application/json' \
  -d '{"username": "alice", "password": "secret"}'
```

Response:

```json
{
  "ok": true,
  "token": "rdb_s_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "username": "alice",
  "role": "admin"
}
```

### Using a Session Token

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'Authorization: Bearer rdb_s_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users"}'
```

## Token Prefixes

| Prefix | Type |
|:-------|:-----|
| `rdb_k_` | API Key |
| `rdb_s_` | Session Token |

## SCRAM-SHA-256

RFC 5802 SCRAM is wired both at the PG-wire listener and inside the
RedWire v2 handshake. Useful when a driver wants a challenge/response
flow without forcing TLS — the password never crosses the wire and a
replay can't steal future sessions.

Server-side primitives: `src/auth/scram.rs`. Client-side primitives:
`drivers/rust/src/redwire/scram.rs`. The `ScramServer` / `ScramClient`
state machines pair with the user-vault entry stored as
`SCRAM-SHA-256$<iterations>:<salt-b64>:<stored-key-b64>:<server-key-b64>`.

```bash
# Provision a SCRAM credential for an existing user
red auth set-scram alice --password 'changeme' --iterations 4096
```

```rust
// Driver-side, abstracted by `connect()`
use reddb::redwire::scram::{ScramClient, ScramConfig};
let mut client = ScramClient::new("alice", "changeme")?;
let client_first = client.client_first_message();
// ... server-first → client-final → server-final exchange happens
//     transparently inside the v2 Hello/AuthStart/AuthChallenge frames.
```

## OAuth / OIDC JWT

Bearer tokens minted by an external identity provider (Auth0,
Keycloak, Cognito, Azure AD, Google Identity, …). The server mounts
a pluggable `JwtVerifier` so the operator picks the signing
algorithm and key source (JWKS URL, shared secret, KMS).

Server config (`AuthConfig.oauth`):

```toml
[auth.oauth]
issuer  = "https://auth.example.com/"
audience = "reddb"
username_claim = "preferred_username"      # default; falls back to `sub`
role_claim = "https://reddb/role"           # optional, otherwise lookup user
jwks_url = "https://auth.example.com/.well-known/jwks.json"
```

The validator enforces `iss`, `aud`, `exp`, and `nbf`. Every
transport that already understands `Authorization: Bearer …`
(HTTP, gRPC, RedWire v2) accepts JWTs without further config.

```bash
curl http://reddb:8080/collections/users/scan \
  -H "Authorization: Bearer ${OIDC_JWT}"
```

## HMAC-signed requests

When you want tamper-evidence and replay protection (e.g. cross-org
webhook traffic), the HMAC scheme signs the canonical request and
travels alongside the standard bearer header.

Headers expected on the request:

| Header | Description |
|--------|-------------|
| `X-RedDB-Key-Id` | API key ID (the bearer half) |
| `X-RedDB-Timestamp` | RFC 3339 timestamp; rejected outside ±5 min window |
| `X-RedDB-Nonce` | Single-use 128-bit random; replay cache keyed by `(key_id, nonce)` |
| `X-RedDB-Signature` | `base64(HMAC-SHA-256(secret, "{method}\n{path}\n{timestamp}\n{nonce}\n{sha256(body)}"))` |

The shared secret is the same value shown once at API-key creation
time (`rdb_k_…`). The server resolves it via the auth store, then
recomputes the signature against the canonical string. A mismatch is
a 401; a stale timestamp or replayed nonce is a 401 as well.

```bash
KEY_ID="rdb_k_xxxx"
SECRET="<from key creation>"
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
NONCE=$(openssl rand -hex 16)
BODY='{"query":"SELECT 1"}'
BODY_HASH=$(printf '%s' "$BODY" | openssl dgst -sha256 -binary | base64)
CANONICAL="POST\n/query\n$TS\n$NONCE\n$BODY_HASH"
SIG=$(printf "$CANONICAL" | openssl dgst -sha256 -hmac "$SECRET" -binary | base64)
curl -X POST https://reddb:8443/query \
  -H "X-RedDB-Key-Id: $KEY_ID" \
  -H "X-RedDB-Timestamp: $TS" \
  -H "X-RedDB-Nonce: $NONCE" \
  -H "X-RedDB-Signature: $SIG" \
  -H 'content-type: application/json' \
  -d "$BODY"
```

## Client certificate (mTLS)

Issue every workload a client cert from your private CA, configure
the server with that CA bundle, and the TLS handshake itself proves
identity. Two server-side identity-mapping modes:

- **`CommonName`** — the cert's CN is looked up against the user vault.
- **`SanRfc822Name`** — the cert's `subjectAltName rfc822Name` is the username.

Optional X.509 OID-to-role mapping lets the cert carry the RedDB
role inline, skipping the vault lookup. Configure via
`AuthConfig.cert`. Drivers connecting to RedWire v2 + TLS pass
`cert`, `key`, and `ca` either through the URL or via the
`tls: { ca, cert, key }` options object documented in
[`/clients/connection-strings`](../clients/connection-strings.md).

## Where each method is checked

| Surface | API key | Session | SCRAM | OAuth | HMAC | mTLS |
|---------|---------|---------|-------|-------|------|------|
| HTTP `/auth/*` | ✅ | mints | ❌ | ✅ | ✅ | via TLS |
| HTTP `/collections/*`, `/query` | ✅ | ✅ | ❌ | ✅ | ✅ | via TLS |
| HTTP `/admin/*` (audit logged) | ✅ | ✅ | ❌ | ✅ | ✅ | via TLS |
| gRPC | ✅ | ✅ | ❌ | ✅ | ✅ | n/a |
| RedWire v2 handshake | ✅ | mints | ✅ | ✅ | ✅ | ✅ |
| PG wire | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |

## See also

- [Auth & Security Overview](overview.md) — RBAC, vault, RLS
- [Connection Strings](/clients/connection-strings.md) — driver-side wiring
- [Wire Protocol Comparison](/clients/wire-protocol-comparison.md) — handshake diagrams
- ADR 0002 — phased rollout of SCRAM + OAuth in RedWire v2
