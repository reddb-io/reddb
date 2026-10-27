RedDB Browser 0.3.0 adds positional SQL parameters to the local JavaScript/TypeScript database. It uses the same AST binder as the native RedDB runtime: values bind as typed literals and never become SQL syntax. The Rust/WASM Worker and OPFS database continue to run locally without a RedDB server, account, API key or network service.

```sh
npm add @reddb-io/browser@0.3.0
```

Vite applications import `openDatabase` and supply the bundled Worker:

```ts
import { openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const db = await openDatabase({
  name: 'my-app', create: true, worker: new RedDBWorker(),
})
try {
  await db.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
  await db.query('BEGIN')
  const existing = await db.query('SELECT id FROM notes WHERE id = $1', [1])
  if (!existing.rows.length) await db.query('INSERT INTO notes VALUES ($1, $2)', [1, "O'Reilly's offline note"])
  await db.query('COMMIT')
  console.log(await db.query('SELECT id, body FROM notes'))
} finally { await db.close() }
```

Included in this release:

- `query(sql, params)` binds `$1`, `$2`, … and numbered `?1`, `?2`, … through the shared native Rust binder. Repeated positions reuse values; missing, extra or skipped positions return `INVALID_PARAMS`.
- Parameters accept null, boolean, string, finite fractional numbers, safe integer numbers and bigint within i64/u64 bounds. Unsafe integer numbers, out-of-range bigints and unsupported parameter types are rejected. Exact results retain `$int`/`$uint` envelopes.
- CREATE TABLE, INSERT, UPDATE, DELETE and SELECT remain available. Parameters apply to supported SELECT/INSERT/UPDATE/DELETE expressions and pagination, with a 65,536-value cap. SQL identifiers and DDL are not parameterized.
- BEGIN/COMMIT/ROLLBACK with own-write visibility and durable committed recovery. Close or Worker termination discards uncommitted changes; statement errors abort an explicit transaction until ROLLBACK.
- Shared native schema, defaults, generated columns, CHECK and uniqueness enforcement. Row-only commits use the canonical WAL batch; catalog changes publish a complete canonical snapshot.
- Existing artifact, scalar-runtime and JSON-safe language interop APIs remain available. The JS, TypeScript declarations, Worker and WASM ship together; consumers need no Rust toolchain.

One handle exclusively owns its file. Tables and pending transaction actions are each bounded at 100,000 rows/actions. BEGIN copies the committed resident image. Ambiguous storage errors fence the handle until reopen.

This version supports ordinary tables. Objects, arrays, binary values and dates are not JavaScript parameter values. Other collection models, advanced SELECT clauses, encrypted writes, AI/events, observation and synchronization remain unsupported. The local file-owner API does not establish a server identity or enforce server tenant/RLS policies; export only an authorized subset.

The embedding application must bundle or cache its HTML, JS, Worker and WASM together for offline startup. Installed TypeScript/Vite consumers verify CRUD, commit/rollback and two complete offline browser restarts in Chromium and Firefox, including recovery of writes made offline. Real Safari/iOS persistence remains unqualified.

Usage guide: https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md
API details: https://github.com/reddb-io/reddb/tree/main/packages/browser

The `.tgz` release asset is directly installable with npm/pnpm; `SHA256SUMS` records its checksum. The Browser version/tag is independent of native RedDB releases and publishes to npm's `latest` channel.
