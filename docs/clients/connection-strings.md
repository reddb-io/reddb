# RedDB Connection Strings

Single URL covers every transport. The driver parses, picks the
right adapter, and falls back when needed. mTLS is wired through
the URL or via the `options.tls` object.

## Transports — quick matrix

| URL                                                                     | Transport          | Wire shape                  | Auth path                |
|-------------------------------------------------------------------------|--------------------|-----------------------------|--------------------------|
| `red://`                                                                | embedded           | spawn → stdio JSON-RPC      | n/a (process privileges) |
| `red://:memory` / `red://:memory:`                                      | embedded           | spawn → stdio JSON-RPC      | n/a                      |
| `red:///path/to/data.rdb`                                               | embedded persistent| spawn → stdio JSON-RPC      | n/a                      |
| `red://host:8080?proto=http` / `http://host:8080`                       | HTTP/1.1           | fetch() → REST              | bearer / basic / login   |
| `red://host:8443?proto=https` / `https://host:8443`                     | HTTPS              | fetch() → REST + TLS        | bearer / basic / login / OAuth-JWT |
| `red://host:50051?proto=grpc` / `grpc://host:50051`                     | gRPC plain         | HTTP/2 framed protobuf      | bearer / OAuth-JWT       |
| `red://host:50052?proto=grpcs` / `grpcs://host:50052`                   | gRPC + TLS         | HTTP/2 + TLS                | bearer / mTLS / OAuth-JWT |
| `red://host:5050` / `red://host:5050` / `grpc://host:5050`          | RedWire plain      | TCP framed binary           | bearer / anonymous       |
| `reds://host:5443` / `red://host:5443?proto=redwires`                | RedWire + TLS      | TLS-wrapped framed binary   | bearer / mTLS            |
| `red://host:5443?tls=true&cert=/c.pem&key=/k.pem&ca=/ca.pem`             | RedWire + mTLS     | TLS + client cert           | mTLS (CN→user) + bearer  |
| `red://host:5432?proto=pg`                                              | PostgreSQL wire    | PG F+B v3 (via psql / node-pg) | SCRAM/MD5/cleartext   |

## Examples

### Embedded (in-memory or file)

```js
import { connect } from 'reddb'
const a = await connect('red://')                      // memory, ephemeral
const b = await connect('red://:memory:')              // SQLite-style alias
const c = await connect('red:///var/lib/db.rdb')       // persistent
```

### HTTP / HTTPS

```js
const db = await connect('https://reddb.example.com:8443', {
  auth: { username: 'admin', password: 'secret' },     // → /auth/login
})
// Or bearer:
const db = await connect('https://reddb.example.com:8443', {
  auth: { token: 'sk-abc' },
})
```

### RedWire (binary TCP)

```js
const db = await connect('red://reddb.example.com:5050', {
  auth: { token: 'sk-abc' },                            // bearer
})
// Or anonymous (server has auth.enabled=false):
const db = await connect('red://reddb.example.com:5050')
```

### RedWire + mTLS (production)

URL form (paths read from disk):
```js
const db = await connect(
  'reds://reddb.example.com:5050'
  + '?cert=/etc/reddb/client.pem'
  + '&key=/etc/reddb/client.key'
  + '&ca=/etc/reddb/ca.pem',
)
```

Programmatic form (PEM strings or file paths):
```js
const db = await connect('reds://reddb.example.com:5050', {
  tls: {
    ca: pemCaBuffer,
    cert: pemClientCert,
    key: pemClientKey,
    servername: 'reddb.example.com',
    rejectUnauthorized: true,                          // default
  },
})
```

### gRPC + TLS (`grpcs://`)

```js
const db = await connect('grpcs://reddb.example.com:50052', {
  auth: { token: 'sk-abc' },
  tls: {
    ca: fs.readFileSync('/etc/reddb/ca.pem'),       // pinned internal CA
    servername: 'reddb.example.com',
    rejectUnauthorized: true,
  },
})
```

Add `cert` / `key` for mTLS:

```js
const db = await connect('grpcs://reddb.example.com:50052', {
  tls: {
    ca:   fs.readFileSync('/etc/reddb/ca.pem'),
    cert: fs.readFileSync('/etc/reddb/client.pem'),
    key:  fs.readFileSync('/etc/reddb/client.key'),
  },
})
```

Server side (Agent B in this round):

```bash
red server \
  --grpc-tls-bind      0.0.0.0:50052 \
  --grpc-tls-cert      /run/secrets/grpc.crt \
  --grpc-tls-key       /run/secrets/grpc.key \
  --grpc-tls-client-ca /run/secrets/clients-ca.pem    # optional, enables mTLS
```

Full flag / env-var reference:
[`docs/security/transport-tls.md`](../security/transport-tls.md#grpc-agent-b-this-round).

### gRPC (legacy bridge — explicit opt-out)

The `grpc://` scheme defaults to RedWire because it shares port
5050. To force the legacy stdio→gRPC bridge:

```js
const db = await connect('grpc://reddb.example.com:5051?proto=spawn-grpc')
```

## URL parameters reference

| Param                | Values                  | Notes |
|----------------------|-------------------------|-------|
| `proto`              | `red` (default), `redwires`, `grpc`, `grpcs`, `http`, `https`, `pg`, `spawn-grpc` | Picks transport |
| `tls`                | `true` / `1` / `false`  | Force-on TLS for `red://` |
| `cert`               | path or PEM             | Client certificate (mTLS) |
| `key`                | path or PEM             | Client private key (mTLS) |
| `ca`                 | path or PEM             | Trusted CA bundle |
| `servername`         | hostname                | SNI override |
| `rejectUnauthorized` | `true` / `false`        | Skip server cert verify (dev only) |
| `token`              | string                  | Bearer / API-key token |
| `apiKey`             | string                  | Alias for `token` |
| `loginUrl`           | absolute URL            | Override `/auth/login` for username/password flow |

## Server-side requirements

| Transport | Engine listener           | Wired in `service_cli` |
|-----------|----------------------------|------------------------|
| HTTP / HTTPS | `start_http_server`     | yes                   |
| gRPC      | `start_grpc_server`        | yes                   |
| RedWire plain    | `spawn_wire_listeners`              | yes |
| RedWire + TLS    | `start_wire_tls_listener`           | yes when TLS configured |
| PG wire   | `start_pg_wire_listener`   | yes                   |

## Auth methods supported

| Method         | HTTP | gRPC (Bearer) | RedWire    | PG wire |
|----------------|------|---------------|------------|---------|
| Username + password (login → token) | ✅ | ✅ via `/auth/login` then bearer | ✅ same path | ❌ |
| Bearer token / API key             | ✅ | ✅            | ✅         | ❌ |
| mTLS client cert                   | ✅ via TLS | n/a    | ✅         | ✅ |
| OAuth / OIDC JWT                   | ✅ | ✅            | ✅         | ❌ |
| SCRAM-SHA-256                      | ❌ | ❌            | ✅         | ✅ |
| HMAC-signed request                | ✅ | ✅            | ✅         | ❌ |
| SQL-cleartext                      | ❌ | ❌            | ❌         | ✅ |
| Anonymous (auth disabled)          | ✅ | ✅            | ✅         | ✅ |

The RedWire handshake advertises supported methods inline in the
server `Hello` frame, so the driver picks the strongest method
without an extra probe round-trip. SCRAM-SHA-256 follows RFC 5802
(client-first → server-first → client-final → server-final);
OAuth/JWT validates via the server's pluggable `JwtVerifier`.

## Reference drivers

| Driver | Transports landed | Auth methods |
|--------|--------------------|--------------|
| `reddb` (JS / TS) — `drivers/js` | embedded, HTTP, HTTPS, RedWire (TCP / TLS / mTLS), PG wire | bearer, login, mTLS, OAuth/JWT, SCRAM (via RedWire) |
| `reddb` (Rust) — `drivers/rust` | embedded, HTTP, HTTPS, RedWire (TCP / TLS / mTLS), PG wire | bearer, login, mTLS, OAuth/JWT, SCRAM (via RedWire) |
| `reddb` (Python) — `drivers/python` | embedded (PyO3), HTTP | bearer, login |

The JS and Rust drivers share the **6-transport matrix** (embedded,
HTTP, HTTPS, RedWire-TCP, RedWire-TLS, RedWire-mTLS) plus a PG-wire
fallback. The Python driver exposes the embedded engine and HTTP
adapter only — RedWire bindings live behind the `redwire` extra.

## See also

- `docs/adr/0001-redwire-tcp-protocol.md` — wire protocol spec
- `docs/clients/wire-protocol-comparison.md` — vs Postgres / Mongo
- `docs/clients/sdk-compatibility.md` — driver feature matrix
- `docs/security/overview.md` — server-side auth config
- `docs/security/tokens.md` — bearer / SCRAM / OAuth / HMAC token reference
- `docs/security/transport-tls.md` — full TLS posture for `https://`, `grpcs://`, `reds://`: server flags, env vars, mTLS, OAuth/JWT, reverse-proxy patterns
