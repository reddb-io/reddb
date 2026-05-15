# @reddb-io/sdk

Official RedDB SDK for JavaScript and TypeScript. Speaks JSON-RPC 2.0 over
stdio to a local `red` binary, which is downloaded automatically on install.
Works in **Node 18+**, **Bun** and **Deno** (via `npm:` specifier) — same
package, no per-runtime fork.

Use this package when your application should run an embedded local RedDB
engine. For remote HTTP, gRPC, or RedWire connections, install
`@reddb-io/client` instead. If you just want to launch the CLI from npm, use:

```bash
npx @reddb-io/cli@latest version
npx @reddb-io/cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```

## Install

```bash
pnpm add @reddb-io/sdk
# or
npm install @reddb-io/sdk
# or
bun add @reddb-io/sdk
```

In Deno:

```ts
import { connect } from 'npm:@reddb-io/sdk'
```

The `postinstall` script downloads the matching `red` binary from GitHub
Releases into `node_modules/@reddb-io/sdk/bin/`. If your environment blocks
postinstall scripts or has no network, set `REDDB_BINARY_PATH=/path/to/red`
and the driver will use that instead.

## Quickstart

```js
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')        // ephemeral
// or:  await connect('file:///var/lib/reddb/data.rdb')   // persisted

await db.insert('users', { name: 'Alice', age: 30 })
await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])

const result = await db.query('SELECT * FROM users')
console.log(result.rows)

await db.close()
```

TypeScript is the same API:

```ts
import { connect, RedDBError } from '@reddb-io/sdk'

const db = await connect('file:///var/lib/reddb/data.rdb')

try {
  const result = await db.query('SELECT * FROM users LIMIT 10')
  console.log(result.rows)
} catch (err) {
  if (err instanceof RedDBError) {
    console.error(err.code, err.message)
  }
}

await db.close()
```

For the SQL/RQL grammar that `db.query()` accepts, see
[`docs/reference/sql-1-0-x.md`](../../docs/reference/sql-1-0-x.md).

## Transactions

Use `db.transaction()` when a group of writes must commit or roll back together.
The callback receives a transaction handle with the same `query`, `insert`, and
`bulkInsert` methods as `db`.

```js
const userId = await db.transaction(async (tx) => {
  const inserted = await tx.insert('users', { name: 'Ada' })
  await tx.query('INSERT INTO audit (action) VALUES ($1)', 'created user')
  return inserted.id
})
```

The wrapper sends `BEGIN`, commits when the callback resolves, and rolls back
when the callback or a `tx.query()` / `tx.insert()` call throws. Nested
transactions on the same connection are rejected with `NESTED_TX_NOT_SUPPORTED`;
open another `connect()` handle for independent concurrent transactions.

## Connection URIs

| URI                        | Mode                                 |
|----------------------------|--------------------------------------|
| `memory://`                | Ephemeral, in-memory database        |
| `file:///absolute/path`    | Embedded engine, persisted to disk   |

Remote URIs such as `http://...`, `red://...`, and `grpc://...` are rejected
with `EMBEDDED_ONLY`. Use `@reddb-io/client` for those transports.

## API

### `connect(uri, options?) → Promise<RedDB>`

Spawns `red rpc --stdio` with arguments derived from the URI, attaches a
JSON-RPC client to its stdin/stdout, then returns a connection handle.

Options:
- `binary` — override the `red` binary path. Defaults to `bin/red` next to the
  package, or `REDDB_BINARY_PATH` env var if set.

Examples:

```js
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')
const persisted = await connect('file:///tmp/app.rdb')
const custom = await connect('memory://', { binary: '/usr/local/bin/red' })
```

### `db.query(sql, ...params) → Promise<{ statement, affected, columns, rows }>`

Bind user values with `$1`, `$2`, ... placeholders. The variadic form is the
preferred API; the older `db.query(sql, paramsArray)` form remains supported.

```js
const result = await db.query(
  'SELECT * FROM users WHERE id = $1 AND name = $2',
  42,
  'Alice',
)
```

`db.execute(sql, ...params)` is an alias for statements where the affected row
count is the primary result.

`ASK '...'` returns the ASK envelope directly:

```js
const answer = await db.query("ASK 'why did deploy fail?'")
answer.answer
answer.citations
answer.sources_flat
answer.validation
answer.cache_hit
answer.cost_usd
```

`ASK '...' STREAM` notifications are not wired over the JS stdio JSON-RPC
client yet. Use the HTTP streaming API for incremental ASK frames; stdio
currently supports materialised cursor batching through `query.open` /
`query.next`, which is separate from ASK token streaming.

### `db.insert(collection, payload) → Promise<{ affected, rid? }>`

### `db.bulkInsert(collection, payloads) → Promise<{ affected }>`

### `db.get(collection, rid) → Promise<{ entity }>`

### `db.delete(collection, rid) → Promise<{ affected }>`

### `db.health() → Promise<{ ok, version }>`

### `db.version() → Promise<{ version, protocol }>`

### `db.close() → Promise<void>`

Sends `close` to the binary, waits for it to exit. Calls after `close()` reject
with `RedDBError('CLIENT_CLOSED', ...)`.

## Errors

All RPC failures throw `RedDBError`:

```js
import { RedDBError } from '@reddb-io/sdk'

try {
  await db.query('NOT VALID SQL')
} catch (err) {
  if (err instanceof RedDBError) {
    console.error(err.code)    // 'QUERY_ERROR'
    console.error(err.message) // server-provided detail
    console.error(err.data)    // optional structured data
  }
}
```

Stable error codes:

| code              | when                                                       |
|-------------------|------------------------------------------------------------|
| `PARSE_ERROR`     | Server got malformed JSON (driver bug, please report)      |
| `INVALID_REQUEST` | Missing field or unknown method                            |
| `INVALID_PARAMS`  | params didn't match the method schema                      |
| `QUERY_ERROR`     | SQL parse, type or constraint error                        |
| `NOT_FOUND`       | Entity / collection does not exist                         |
| `INTERNAL_ERROR`  | Server caught a panic                                      |
| `CLIENT_CLOSED`   | Driver-side: call after `close()` or unexpected EOF        |
| `UNSUPPORTED_SCHEME` | URI scheme not yet supported by the driver              |

## Limits

- **stdio IPC overhead.** Each call is a JSON serialize, write, parse, read
  round-trip. For most apps (web servers, scripts, ETL) this is invisible.
  For very high-throughput single-process ingestion, use the embedded Rust
  crate `reddb` directly.
- **No edge runtimes.** Cloudflare Workers, Vercel Edge and the browser have
  no subprocess support. If you need RedDB there, wait for the planned HTTP
  transport (see `PLAN_DRIVERS.md`).
- **One process per connection.** No connection pooling yet. If you need
  concurrent independent transactions, open multiple `connect()` handles.

## Testing locally

```bash
cargo build --bin red       # at repo root
cd drivers/js
node test/smoke.test.mjs
```

Same test runs in Bun and Deno:

```bash
bun test/smoke.test.mjs
deno run -A test/smoke.test.mjs
```

## Remote Deploy

When you're ready to point JavaScript code at a production RedDB cluster, use
`@reddb-io/client`. The SDK package is embedded-only and intentionally rejects
remote URIs.

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use TLS** for any traffic crossing the network.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.
