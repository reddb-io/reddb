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
| `red://host:8443?proto=https` / `https://host:8443`                     | HTTPS              | fetch() → REST + TLS        | bearer / basic / login   |
| `red://host:5050` / `red://host:5050` / `grpc://host:5050`          | RedWire v2 plain   | TCP framed binary           | bearer / anonymous       |
| `reds://host:5050` / `red://host:5050?proto=redwires`                | RedWire v2 + TLS   | TLS-wrapped framed binary   | bearer / mTLS            |
| `red://host:5050?tls=true&cert=/c.pem&key=/k.pem&ca=/ca.pem`             | RedWire v2 + mTLS  | TLS + client cert           | mTLS (CN→user) + bearer  |
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

### RedWire v2 (binary TCP)

```js
const db = await connect('red://reddb.example.com:5050', {
  auth: { token: 'sk-abc' },                            // bearer
})
// Or anonymous (server has auth.enabled=false):
const db = await connect('red://reddb.example.com:5050')
```

### RedWire v2 + mTLS (production)

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

### gRPC (legacy bridge — explicit opt-out)

The `grpc://` scheme defaults to RedWire v2 because it shares port
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
| RedWire v2 plain | shares the v1 wire listener via 0xFE dispatch | yes (`spawn_wire_listeners`) |
| RedWire v2 + TLS | `start_wire_tls_listener` + dispatch | yes when TLS configured |
| PG wire   | `start_pg_wire_listener`   | yes                   |

## Auth methods supported

| Method         | HTTP | gRPC (Bearer) | RedWire | PG wire |
|----------------|------|---------------|---------|---------|
| Username + password (login → token) | ✅ | ✅ via `/auth/login` then bearer | ✅ same path | ❌ |
| Bearer token / API key             | ✅ | ✅            | ✅      | ❌ |
| mTLS client cert                   | ✅ via TLS | n/a    | ✅      | ✅ |
| OAuth / OIDC JWT                   | ✅ | ✅            | planned | ❌ |
| SCRAM-SHA-256                      | ❌ | ❌            | planned | ✅ |
| SQL-cleartext                      | ❌ | ❌            | ❌      | ✅ |
| Anonymous (auth disabled)          | ✅ | ✅            | ✅      | ✅ |

## See also

- `docs/adr/0001-redwire-tcp-protocol.md` — wire protocol spec
- `docs/clients/wire-protocol-comparison.md` — vs Postgres / Mongo
- `docs/security/overview.md` — server-side auth config
