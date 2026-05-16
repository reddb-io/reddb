# @reddb-io/client

Thin **remote-only** RedDB driver for JavaScript and TypeScript. Speaks
RedWire (TCP + mTLS), gRPC, and HTTP straight to a remote RedDB
server. Ships the `red_client` thin binary for an ad-hoc REPL — about
10x smaller than `@reddb-io/sdk`.

This package is the remote counterpart to the embedded-only
`@reddb-io/sdk`.

> Embedded engines (`memory://`, `file:///path`) are intentionally
> rejected by this package. Use [`@reddb-io/sdk`](../js) instead if you
> need an in-process database.

## When to use this package

| You want…                                         | Install              |
| ------------------------------------------------- | -------------------- |
| Connect to a running RedDB server (most apps)     | `@reddb-io/client`   |
| Same, plus the ability to spin up a local engine  | `@reddb-io/sdk`      |
| The CLI launcher (`reddb-cli`)                    | `@reddb-io/cli`      |

All three packages stay version-locked.

## Install

```bash
pnpm add @reddb-io/client
# or
npm install @reddb-io/client
```

The `postinstall` script downloads the matching `red_client` binary
from GitHub Releases into `node_modules/@reddb-io/client/bin/`. If your
environment blocks postinstall scripts or has no network, set
`REDDB_CLIENT_BIN=/path/to/red_client` to point at a copy you've placed
manually. The driver itself does **not** need the binary for `connect()`
— it speaks the wire protocols directly from JS.

## Quickstart

```js
import { connect } from '@reddb-io/client'

const db = await connect('red://reddb.example.com:5050', {
  auth: { token: process.env.REDDB_TOKEN },
})

await db.insert('users', { name: 'Alice' })
const result = await db.query('SELECT * FROM users WHERE name = $1', 'Alice')
console.log(result.rows)

const doc = await db.documents.insert('events', { event_type: 'login' })
await db.documents.patch('events', doc.rid, { reviewed: true })

await db.query('CREATE KV settings')
const kv = db.kv('settings')
await kv.put('characters:hansel', 'crumbs')
console.log(await kv.get('characters:hansel'))

await db.close()
```

Use `db.query(sql, ...params)` or `db.execute(sql, ...params)` for
parameterized statements. The compatibility form `db.query(sql, paramsArray)`
is still accepted.

For `http://` and `https://` connections, `connect()` verifies readiness with
a lightweight `SELECT 1` round-trip. `/health` states such as `degraded` are
transient during boot and are not fatal as long as queries succeed.

## Transactions

Use `db.transaction()` when a group of writes must commit or roll back together.
The callback receives a transaction handle with the same `query`, `insert`, and
`bulkInsert` methods as `db`.

```js
const userId = await db.transaction(async (tx) => {
  const inserted = await tx.insert('users', { name: 'Ada' })
  await tx.query('INSERT INTO audit (action) VALUES ($1)', 'created user')
  return inserted.rid
})
```

The wrapper sends `BEGIN`, commits when the callback resolves, and rolls back
when the callback or a `tx.query()` / `tx.insert()` call throws. Nested
transactions on the same connection are rejected with `NESTED_TX_NOT_SUPPORTED`;
open another `connect()` handle for independent concurrent transactions.

## Rich Helpers

The client follows the SDK Helper Spec for the shared JS/TS surface:

- `db.insert(collection, payload)` returns `{ affected, rid, id }`; `id` is a
  legacy alias for `rid`.
- `db.bulkInsert(collection, payloads)` returns `{ affected, rids, ids }`; `ids`
  is a legacy alias for `rids`.
- `db.documents.insert/get/list/patch/delete` covers document CRUD. Insert
  creates the document collection when needed; patch currently accepts top-level
  fields.
- `db.kv(collection?)` preserves exact keys, including namespaced keys such as
  `characters:hansel`, and exposes `put/get/exists/delete/list`.
- `db.queue` exposes `push/pop/peek/len/purge`.

## Accepted URI schemes

| Scheme         | Transport                     | Default port |
| -------------- | ----------------------------- | ------------ |
| `red://`       | RedWire (TCP)                 | 5050         |
| `reds://`      | RedWire over TLS              | 5050         |
| `grpc://`      | gRPC                          | 5055         |
| `grpcs://`     | gRPC over TLS                 | 5056         |
| `http://`      | HTTP JSON                     | 8080         |
| `https://`     | HTTPS JSON                    | 8443         |

## Rejected URI schemes

`memory://`, `memory:`, `file:///abs/path`, `red://`, `red:///path`,
`red://:memory`, `red://:memory:` — all throw `EmbeddedNotSupported`
with the same wording as the underlying `red_client` binary:

> embedded schemes (memory:// / file://) are not supported. Use the
> full `red` binary for in-memory or file-backed engines.

## Authentication

```js
// Bearer / API key
await connect('red://host:5050', { auth: { token: 'sk-abc' } })

// or via the URI:
await connect('red://host:5050?token=sk-abc')

// Username + password (driver calls /auth/login first):
await connect('red://user:pass@host:5050')
```

## Environment overrides

| Variable                     | Effect                                                |
| ---------------------------- | ----------------------------------------------------- |
| `REDDB_CLIENT_BIN`           | Override path to `red_client` for spawn-style helpers |
| `REDDB_SKIP_POSTINSTALL=1`   | Don't download the binary on install                  |
| `REDDB_POSTINSTALL_VERSION`  | Pull a specific release tag                           |
| `REDDB_POSTINSTALL_REPO`     | Pull from a fork (default `reddb-io/reddb`)           |

## License

MIT.
