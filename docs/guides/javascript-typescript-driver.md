# JavaScript and TypeScript Driver

Use the `@reddb-io/sdk` package for application code in Node, Bun, and Deno.

Use `@reddb-io/cli` only when you want to launch the real `red` binary from npm:

```bash
npx @reddb-io/cli@latest version
npx @reddb-io/cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```

> **Performance check before you commit.** RedDB's measured wins (and
> the gaps where it still loses) are catalogued in
> [`docs/perf/wins.md`](../perf/wins.md) and
> [`docs/perf/when-not-reddb.md`](../perf/when-not-reddb.md). If your
> hot path is bulk typed ingest or compact-write throughput, you are
> on the right track. If it is highly concurrent OLTP, large `UPDATE`s,
> `GROUP BY` aggregates, or filtered `SELECT`s on secondary indices,
> read `when-not-reddb.md` first — those are in-flight.

## 1. Install the driver

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
environment blocks install scripts, set `REDDB_BINARY_PATH=/path/to/red`.

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

const result = await db.query('SELECT * FROM users')
console.log(result.rows)

await db.close()
```

Available methods:

- `db.query(sql)`
- `db.insert(collection, payload)`
- `db.bulkInsert(collection, payloads)`
- `db.get(collection, id)`
- `db.delete(collection, id)`
- `db.health()`
- `db.version()`
- `db.close()`

## 4. Error handling

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

## 5. Override the binary path

If you already manage the `red` binary yourself:

```ts
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://', {
  binary: '/usr/local/bin/red',
})
```

## 6. CLI vs driver

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
