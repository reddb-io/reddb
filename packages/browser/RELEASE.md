RedDB Browser 0.5.0 adds `createDatabaseSession` for JavaScript/TypeScript applications that repeatedly open and close local databases. The session retains the initialized Rust/WASM Worker between operations while every database handle still releases its exclusive OPFS file ownership on close.

```sh
npm add @reddb-io/browser@0.5.0
```

```ts
import { createDatabaseSession } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const session = await createDatabaseSession({ worker: new RedDBWorker() })
try {
  const db = await session.openDatabase({ name: 'my-app', create: true })
  try {
    await db.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
    console.log(await db.query('SELECT id, body FROM notes'))
  } finally { await db.close() }
  // Later operations can open another handle in the same session.
} finally { await session.close() }
```

- One session has at most one open database, including during opening and release. Overlapping opens fail explicitly.
- Closing a database releases its file and discards an uncommitted transaction. Later opens recover current committed state, including changes by another tab or Worker.
- Old handles stay closed and cannot query or release a later owner's file.
- A failed release fences the session and terminates its Worker. Dispose failed sessions and create a new one before retrying after a Worker crash.
- `session.close()` is idempotent and closes the current file before terminating the Worker. Keeping the session alive retains initialized WASM memory; applications must dispose it when finished.
- Standalone `openDatabase`, artifact, scalar runtime, native backup and JSON-safe scalar interop APIs keep their existing behavior. No native file format, SQL semantics or Worker protocol changes are required.

Applications sharing a file across tabs must retain their writer/snapshot guards and queue open/operation/close with an application Web Lock. Sessions amortize Worker startup; file recovery and SQL costs remain. This release makes no universal latency, CPU or memory improvement claim. The optional browser benchmark compares equal 300 kB committed writes, alternating fresh Workers and a reused session with the same file history.

Local operation needs no RedDB server, account, API key or network service. Bundle/cache HTML, JS, Worker and WASM together for offline startup. Real Chromium and Firefox tests cover ownership handoff, peer commit visibility, rollback on close, stale-handle isolation, I/O failures and offline browser restarts. The packaged TS/Vite consumer also reopens sessions offline.

Table SQL/transactions and native `.rdb` backups remain supported with their existing limits. Other collection models, advanced SELECT clauses, AI/events, observation and synchronization remain unsupported. Real Safari/iOS persistence remains unqualified. See the full support contract and examples:

- https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md
- https://github.com/reddb-io/reddb/tree/main/packages/browser

The installable `.tgz` and `SHA256SUMS` accompany this release. Browser versioning is independent of native RedDB and publishes to npm's `latest` channel.
