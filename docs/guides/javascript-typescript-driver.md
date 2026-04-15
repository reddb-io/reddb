# JavaScript and TypeScript Driver

Use the `reddb` package for application code in Node, Bun, and Deno.

Use `reddb-cli` only when you want to launch the real `red` binary from npm:

```bash
npx reddb-cli@latest version
npx reddb-cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```

## 1. Install the driver

```bash
pnpm add reddb
```

Or:

```bash
npm install reddb
bun add reddb
```

In Deno:

```ts
import { connect } from 'npm:reddb'
```

The package downloads the matching `red` binary during `postinstall`. If your
environment blocks install scripts, set `REDDB_BINARY_PATH=/path/to/red`.

## 2. Connect

```ts
import { connect } from 'reddb'

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
import { connect } from 'reddb'

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
import { connect, RedDBError } from 'reddb'

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
import { connect } from 'reddb'

const db = await connect('memory://', {
  binary: '/usr/local/bin/red',
})
```

## 6. CLI vs driver

Use this rule:

- application code: `reddb`
- npm-launched CLI: `reddb-cli`

Examples:

```bash
pnpm add reddb
```

```ts
import { connect } from 'reddb'
```

```bash
npx reddb-cli@latest server --http-bind 127.0.0.1:8080 --path ./data.rdb
```
