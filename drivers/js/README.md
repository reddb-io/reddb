# reddb

Official RedDB driver for JavaScript and TypeScript. Speaks JSON-RPC 2.0 over
stdio to a local `red` binary, which is downloaded automatically on install.
Works in **Node 18+**, **Bun** and **Deno** (via `npm:` specifier) — same
package, no per-runtime fork.

## Install

```bash
pnpm add reddb
# or
npm install reddb
# or
bun add reddb
```

In Deno:

```ts
import { connect } from 'npm:reddb'
```

The `postinstall` script downloads the matching `red` binary from GitHub
Releases into `node_modules/reddb/bin/`. If your environment blocks postinstall
scripts or has no network, set `REDDB_BINARY_PATH=/path/to/red` and the driver
will use that instead.

## Quickstart

```js
import { connect } from 'reddb'

const db = await connect('memory://')        // ephemeral
// or:  await connect('file:///var/lib/reddb/data.rdb')   // persisted

await db.insert('users', { name: 'Alice', age: 30 })
await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])

const result = await db.query('SELECT * FROM users')
console.log(result.rows)

await db.close()
```

## Connection URIs

| URI                        | Mode                                 |
|----------------------------|--------------------------------------|
| `memory://`                | Ephemeral, in-memory database        |
| `file:///absolute/path`    | Embedded engine, persisted to disk   |
| `grpc://host:port`         | Remote server (planned, not yet)     |

`grpc://` is not supported by the JS driver yet — the binary needs the
`--connect` flag wired up first. See `PLAN_DRIVERS.md` in the repo root.

## API

### `connect(uri, options?) → Promise<RedDB>`

Spawns `red rpc --stdio` with arguments derived from the URI, attaches a
JSON-RPC client to its stdin/stdout, then returns a connection handle.

Options:
- `binary` — override the `red` binary path. Defaults to `bin/red` next to the
  package, or `REDDB_BINARY_PATH` env var if set.

### `db.query(sql) → Promise<{ statement, affected, columns, rows }>`

### `db.insert(collection, payload) → Promise<{ affected, id? }>`

### `db.bulkInsert(collection, payloads) → Promise<{ affected }>`

### `db.get(collection, id) → Promise<{ entity }>`

### `db.delete(collection, id) → Promise<{ affected }>`

### `db.health() → Promise<{ ok, version }>`

### `db.version() → Promise<{ version, protocol }>`

### `db.close() → Promise<void>`

Sends `close` to the binary, waits for it to exit. Calls after `close()` reject
with `RedDBError('CLIENT_CLOSED', ...)`.

## Errors

All RPC failures throw `RedDBError`:

```js
import { RedDBError } from 'reddb'

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
