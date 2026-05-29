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

## Streaming

`db.query()` stays a one-shot Promise — ideal for small reads. For large
result sets or continuous ingest, use the explicit streaming surface so you
never accidentally buffer a huge result or OOM. The surface is identical
whether the connection is RedWire (`red://`) or HTTP (`http://`): RedWire is
used when available, HTTP NDJSON otherwise.

```js
// Read: a Node Readable that is also an AsyncIterable<Row>.
const users = db.collection('users')
for await (const row of users.stream('SELECT id, name FROM users')) {
  console.log(row.id, row.name)
}

// Write: a Node Writable; the server's terminal envelope resolves completion().
import { splitNdjson } from '@reddb-io/client'
import { createReadStream } from 'node:fs'
import { pipeline } from 'node:stream/promises'

const sink = users.inputStream() // columns inferred from the first row's keys
await pipeline(createReadStream('rows.ndjson'), splitNdjson(), sink)
const { row_count } = await sink.completion()
```

Backpressure flows naturally: the Readable via `read()` / `pause()` /
`resume()`, the Writable via `write()`'s return value and the `'drain'` event.
Both expose `.cancel(reason?)` — a `StreamCancel` over RedWire, an
`AbortController.abort()` over HTTP — which terminates the underlying transport
stream and rejects any pending iteration (or `.completion()`) with a
`STREAM_CANCELLED` error. A mid-stream server error surfaces as an `'error'`
event and as a rejected iteration. `db.stream(sql)` / `db.inputStream(target)`
are also available directly on the connection without a `collection()` handle.

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

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> Driver-helper (SDK Helper Spec v1.0) support for every public promise. A helper not marked supported here is not promised by this driver.

| Promise | driver_helpers |
| --- | --- |
| **PSC-001** — RedDB is one multi-model database (tables, graph, KV, timeseries, probabilistic, vector, queue, documents) backed by a single file. | ✅ supported |
| **PSC-002** — MATCH supports node, edge, label, property, and LIMIT projections. | ✅ supported |
| **PSC-003** — GRAPH algorithms accept semantic identifiers, limits, ordering, and return stable rich rows. | ❌ unsupported |
| **PSC-004** — INSERT creates rows, documents, and native timeseries points. | ✅ supported |
| **PSC-005** — HLL/SKETCH/FILTER expose write and read commands for cardinality, frequency, and membership. | ⚠️ partial |
| **PSC-006** — Timeseries stores timestamped metrics with tags and supports query/readback. | ⚠️ partial |
| **PSC-007** — Documents are first-class: create, read, update, delete, and SQL analytics over JSON. | ✅ supported |
| **PSC-008** — KV helpers expose get/put/delete; get of a missing key returns null, delete reports affected. | ✅ supported |
| **PSC-009** — Queue helpers expose create/push/peek/pop/len/purge with FIFO semantics; empty pop is not an error. | ✅ supported |
| **PSC-010** — Transactions are imperative (begin/commit/rollback) plus a run(callback) form; empty SQL rejects with INVALID_ARGUMENT. | ✅ supported |
| **PSC-011** — SQL aggregate, projection, expression, and mutation behaviour matches ordinary SQL expectations where advertised. | ✅ supported |
| **PSC-012** — Server transports expose the same query contract as embedded (HTTP, RedWire, gRPC parity). | ✅ supported |
| **PSC-013** — Official drivers implement the SDK Helper Spec v1.0 conformance suite (all 22 §12 case IDs). | ✅ supported |
| **PSC-014** — ASK / SEARCH semantic surfaces return ranked results with stable shape. | ⚠️ partial |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
