# JavaScript and TypeScript Driver

RedDB ships three npm packages under the `@reddb-io/` scope. Pick the one
that matches how your code talks to RedDB — they are not interchangeable
and each has a distinct binary-acquisition contract (see
[ADR 0007](../adr/0007-npm-package-matrix.md) for the rationale).

## Package matrix

### Decision tree

- **Using as a CLI / launching a local server from npm** →
  [`@reddb-io/cli`](#reddb-iocli). Operator and CI scenario; puts `red`
  on global `PATH`.
- **App code that needs the full SDK including embedded mode** →
  [`@reddb-io/sdk`](#reddb-iosdk). Embedded engine (`memory://`,
  `file:///...`), gRPC, and HTTP transports in one package.
- **Serverless / edge / CI / remote-only client** →
  [`@reddb-io/client`](#reddb-ioclient). Thin client that only speaks to
  a remote RedDB; embedded URIs are rejected.

### Comparison

| Package              | What it ships                                                          | Install size budget | Transports supported                          | Embedded mode | Env override        |
| -------------------- | ---------------------------------------------------------------------- | ------------------- | --------------------------------------------- | ------------- | ------------------- |
| `@reddb-io/cli`      | CLI launcher; downloads full `red` binary on postinstall to global `PATH` | ≤ 12 MB compressed  | n/a (CLI subcommands wrap the server binary)  | n/a           | `REDDB_BIN`         |
| `@reddb-io/sdk`      | Full JS SDK; downloads full `red` binary into `node_modules` on postinstall | ≤ 12 MB compressed  | embedded (stdio JSON-RPC), gRPC, HTTP         | yes           | `REDDB_BIN`         |
| `@reddb-io/client`   | Thin remote-only JS driver; downloads `red_client` thin binary on postinstall | ≤ 5 MB compressed   | gRPC, HTTP (remote endpoints only)            | no — rejects `memory://` and `file:///...` | `REDDB_CLIENT_BIN`  |

> **Performance check before you commit.** RedDB's measured wins (and
> the gaps where it still loses) are catalogued in
> [`docs/perf/wins.md`](../perf/wins.md) and
> [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md). If your
> hot path is bulk typed ingest or compact-write throughput, you are
> on the right track. If it is highly concurrent OLTP, large `UPDATE`s,
> `GROUP BY` aggregates, or filtered `SELECT`s on secondary indices,
> read `when-not-reddb.md` first — those are in-flight.

## 1. Install

### `@reddb-io/sdk`

The default for application code in Node, Bun, and Deno.

```bash
pnpm add @reddb-io/sdk
```

Or:

```bash
npm install @reddb-io/sdk
bun add @reddb-io/sdk
```

In Deno:

```ts
import { connect } from 'npm:@reddb-io/sdk'
```

The package downloads the matching `red` binary during `postinstall`. If your
environment blocks install scripts, set `REDDB_BIN=/path/to/red` (the legacy
name `REDDB_BINARY_PATH` is still honoured during the deprecation window).

### `@reddb-io/cli`

Launch the real `red` binary from npm without a separate install step:

```bash
npx @reddb-io/cli@latest version
npx @reddb-io/cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```

Or install globally so `reddb-cli` is on `PATH`:

```bash
pnpm add -g @reddb-io/cli
```

The CLI postinstall consults the `red` already on `PATH` and decides
between `install`, `upgrade`, and `skip`. Set `REDDB_SKIP_POSTINSTALL=1`
to short-circuit the network round-trip in CI cache-warming or air-gapped
installs.

### `@reddb-io/client`

Pick this when your runtime only talks to a *remote* RedDB and bundling
the full server binary is wasteful (Lambda layers, Cloudflare Workers,
edge containers, CI smoke tests):

```bash
pnpm add @reddb-io/client
```

Only `grpc://...` and `http://...` URIs are accepted. Embedded URIs
(`memory://`, `file:///...`) throw at `connect()` time — by design, the
thin `red_client` binary cannot host an engine. Override the binary
location with `REDDB_CLIENT_BIN=/path/to/red_client` when postinstall
is blocked.

## 2. Connect

```ts
import { connect } from '@reddb-io/sdk'

const memory = await connect('memory://')
const persisted = await connect('file:///var/lib/reddb/data.rdb')
```

Supported URI shapes today:

- `memory://`
- `file:///absolute/path`
- `grpc://host:port`

The driver forwards `grpc://...` to the binary via `red rpc --stdio --connect ...`.

## 3. Query and mutate data

```ts
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')

await db.insert('users', { name: 'Alice', age: 30 })
await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])

const result = await db.query(
  'SELECT * FROM users WHERE age >= $1',
  [30],
)
console.log(result.rows)

await db.close()
```

Available methods:

- `db.query(sql)`, `db.query(sql, params)` (see [Safe parameter binding](#4-safe-parameter-binding))
- `db.insert(collection, payload)`
- `db.bulkInsert(collection, payloads)`
- `db.get(collection, id)`
- `db.delete(collection, id)`
- `db.health()`
- `db.version()`
- `db.close()`

## 4. Safe parameter binding

`db.query` accepts positional `$N` bind values as a second argument. Use it
for any user-supplied value — string concatenation is a SQL-injection
footgun. The cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352):

```ts
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')

// Scalar params: int / text / null
const result = await db.query(
  'SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3',
  [42, 'acme', null],
)

// Vector param (HNSW / IVF similarity search)
const hits = await db.query(
  'SEARCH SIMILAR $1 IN embeddings K 5',
  [Float32Array.from([0.1, 0.2, 0.3])],
)
```

Native JS → engine type mapping (see `encodeValue` in
`drivers/js/src/redwire.js`):

| JS                                            | Engine             |
| --------------------------------------------- | ------------------ |
| `null` / `undefined`                          | Null               |
| `boolean`                                     | Bool               |
| `bigint`                                      | Int (i64)          |
| `number` (integer, safe range)                | Int (i64)          |
| `number` (otherwise)                          | Float (f64)        |
| `string`                                      | Text               |
| `Uint8Array` / `Buffer`                       | Bytes              |
| `Float32Array`, `Float64Array`, `number[]`    | Vector (f32)       |
| `{ $bytes: '<base64>' }`                      | Bytes (envelope)   |
| `{ $ts: <unix-seconds> }`                     | Timestamp          |
| `{ $uuid: '<hyphenated>' }`                   | Uuid               |
| plain object / array                          | Json (canonical)   |

RedWire routes through the binary `QueryWithParams` frame (`0x28`) when the
server advertises `FEATURE_PARAMS`; older servers raise `PARAMS_UNSUPPORTED`
instead of silently dropping the params. HTTP forwards a typed `params`
array — `Uint8Array` ships as `{"$bytes": "<base64>"}`, the timestamp /
UUID envelopes pass through unchanged. `db.query(sql)` with no params stays
byte-identical to the legacy path — old servers and the embedded fast path
don't see the new frame at all.

## 5. Grounding and citations

`ASK` rows use the same grounded envelope across embedded stdio, HTTP, gRPC,
MCP, and Postgres-wire. The citation contract is
[ADR 0013](../adr/0013-ask-grounding-citations.md), created from
[#392](https://github.com/reddb-io/reddb/issues/392), and is the user-visible
AI-native wedge tracked by [PRD #391](https://github.com/reddb-io/reddb/issues/391).

```ts
const ask = await db.query(
  "ASK $1 USING openai STRICT ON CACHE TTL '5m' LIMIT 5",
  'why did deploy fail?',
)

const row = ask.rows[0] ?? ask
console.log(row.answer)
console.log(row.sources_flat[0].urn)
console.log(row.citations[0].marker)
console.log(row.validation.ok)
```

Every factual claim in `answer` should carry a marker such as `[^1]`. That
marker maps to `sources_flat[0]`, whose `urn` is stable enough for UI
deep-links back to the row, document, vector, graph node, or KV entry. Strict
mode validates marker structure before returning; `STRICT OFF` keeps warnings
in `validation` without failing the call. `ASK ... STREAM` is exposed through
HTTP/SSE; JS stdio returns the non-streaming envelope.

## 6. Error handling

```ts
import { connect, RedDBError } from '@reddb-io/sdk'

const db = await connect('memory://')

try {
  await db.query('NOT VALID SQL')
} catch (err) {
  if (err instanceof RedDBError) {
    console.error(err.code)
    console.error(err.message)
  }
}

await db.close()
```

## 7. Override the binary path

If you already manage the `red` binary yourself:

```ts
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://', {
  binary: '/usr/local/bin/red',
})
```

## 8. CLI vs driver

Use this rule:

- application code: `@reddb-io/sdk`
- npm-launched CLI: `@reddb-io/cli`

Examples:

```bash
pnpm add @reddb-io/sdk
```

```ts
import { connect } from '@reddb-io/sdk'
```

```bash
npx @reddb-io/cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```
