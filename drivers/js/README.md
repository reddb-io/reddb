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
npx @reddb-io/cli@latest server --http-bind 127.0.0.1:5000 --path ./data.rdb
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
Releases into `node_modules/@reddb-io/sdk/bin/`. If the download fails (no
network, 404, unsupported platform) the install now **fails loud** with an
actionable multi-line message — it no longer silently ships an empty `bin/`
that explodes the first time you call `connect()`. See the next section
for the supported offline paths.

## Offline / restricted-network installs

If your CI or workstation has no network during `npm install`, or your
environment blocks postinstall scripts entirely, opt out explicitly and
point the driver at a binary you provide yourself:

```bash
# 1. Install the SDK without trying to download the engine.
REDDB_SKIP_POSTINSTALL=1 npm install @reddb-io/sdk

# 2. At runtime, tell connect() where the red binary lives.
export REDDB_BIN=/path/to/red          # canonical, per ADR 0006
# (REDDB_BINARY_PATH is a deprecated alias kept for the rollout window.)
```

Three ways to get a `red` binary:

- Install the latest stable release with the official installer and use
  `REDDB_BIN="$(command -v red)"`:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash
  ```
- Build from a workspace checkout of `reddb-io/reddb`:
  ```bash
  cargo build --release --bin red
  export REDDB_BIN="$PWD/target/release/red"
  ```
- Download a prebuilt asset from the releases page and drop it at
  `<package>/bin/red[.exe]`:
  <https://github.com/reddb-io/reddb/releases>

When `REDDB_SKIP_POSTINSTALL=1` is set the postinstall script prints a one-line
notice and exits 0; without it, any download failure exits non-zero so the
install surfaces the problem immediately. If you forget to provide the
binary, `connect()` raises a clear `binary "red" not found` error that names
`REDDB_BIN` as the override.

## Quickstart

```js
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')        // ephemeral
// or:  await connect('file:///var/lib/reddb/data.rdb')   // persisted

await db.insert('users', { name: 'Alice', age: 30 })
await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])

const result = await db.query('SELECT * FROM users')
console.log(result.rows)

const doc = await db.documents.insert('events', {
  event_type: 'login',
  attempts: 1,
})
await db.documents.patch('events', doc.rid, { reviewed: true })

await db.query('CREATE KV settings')
const kv = db.kv('settings')
await kv.put('characters:hansel', 'crumbs')
console.log(await kv.get('characters:hansel'))

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

This driver supports **both** transaction forms from the SDK Helper Spec
(§7): the imperative `begin` / `commit` / `rollback` trio and the callback
form.

**Callback form** — `db.transaction(callback)` (or `db.tx().run(callback)`):

```js
const userId = await db.transaction(async (tx) => {
  const inserted = await tx.insert('users', { name: 'Ada' })
  await tx.query('INSERT INTO audit (action) VALUES ($1)', 'created user')
  return inserted.rid
})
```

The wrapper sends `BEGIN`, commits when the callback resolves, and rolls back
when the callback or a `tx.query()` / `tx.insert()` call throws.

**Imperative form** — `db.tx()` returns a transaction handle:

```js
const tx = db.tx()
await tx.begin()
try {
  await db.query("INSERT INTO audit (action) VALUES ('created user')")
  await tx.commit()
} catch (err) {
  await tx.rollback()
  throw err
}
```

`begin` / `commit` / `rollback` each resolve to a `QueryResult`. A nested
`tx.run()` (or `db.transaction()`) on the same connection is rejected with
`INVALID_ARGUMENT` (`NESTED_TX_NOT_SUPPORTED` for the legacy
`db.transaction()` shortcut) — callers wanting savepoints issue them
directly via `tx.query()`. Open another `connect()` handle for independent
concurrent transactions.

### Isolation levels

The default isolation is **snapshot isolation** (each transaction reads a
consistent MVCC snapshot). To request a stronger level, issue the isolation
clause with the opening statement via `db.query()`:

```js
await db.query('BEGIN ISOLATION LEVEL SERIALIZABLE')
await db.query("INSERT INTO audit (action) VALUES ('serializable write')")
await db.query('COMMIT')
```

RedDB accepts the PG-compatible spellings `READ UNCOMMITTED`,
`READ COMMITTED`, `REPEATABLE READ` (= `SNAPSHOT`), and `SERIALIZABLE`.
`SERIALIZABLE` engages the serializable-snapshot-isolation (SSI) path, which
can abort a transaction at commit with a serialization conflict (see below).

### Serialization conflicts & retries

Under snapshot / serializable isolation, two transactions that write the same
row on overlapping snapshots resolve **first-committer-wins**: the later
transaction is aborted with a **retryable serialization conflict**. It
surfaces as a `RedDBError` with code `QUERY_ERROR` whose message begins
`serialization conflict`. This is the one error class you should catch and
**retry** rather than surface to the user:

```js
import { connect, RedDBError } from '@reddb-io/sdk'

const db = await connect('file:///var/lib/reddb/data.rdb')

/** True only for the retryable first-committer-wins conflict. */
function isSerializationConflict(err) {
  return (
    err instanceof RedDBError &&
    err.code === 'QUERY_ERROR' &&
    /serialization conflict/i.test(err.message)
  )
}

/** Run a transaction, retrying with backoff on serialization conflicts. */
async function withRetry(fn, { maxRetries = 5 } = {}) {
  for (let attempt = 0; ; attempt++) {
    try {
      return await db.transaction(fn)
    } catch (err) {
      if (isSerializationConflict(err) && attempt < maxRetries) {
        await new Promise((r) => setTimeout(r, 2 ** attempt * 5)) // backoff
        continue
      }
      throw err // not retryable, or out of attempts
    }
  }
}

// Concurrent debits stay consistent — losers retry against the fresh snapshot.
await withRetry(async (tx) => {
  const { rows } = await tx.query('SELECT balance FROM accounts WHERE id = $1', 1)
  await tx.query('UPDATE accounts SET balance = $1 WHERE id = $2', rows[0].balance - 10, 1)
})
```

Only conflicts are retried — a syntax error, a constraint violation, or any
other `QUERY_ERROR` propagates on the first attempt.

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

### `db.insert(collection, payload) → Promise<{ affected, rid, id }>`

`id` is a legacy alias for `rid`.

### `db.bulkInsert(collection, payloads) → Promise<{ affected, rids, ids }>`

`ids` is a legacy alias for `rids`.

### `db.get(collection, rid) → Promise<{ entity }>`

### `db.delete(collection, rid) → Promise<{ affected }>`

### `db.documents`

Document helpers follow the SDK Helper Spec:

```js
const inserted = await db.documents.insert('events', {
  event_type: 'login',
  details: { ip: '10.0.0.7' },
})

const event = await db.documents.get('events', inserted.rid)
const page = await db.documents.list('events', {
  filter: "event_type = 'login'",
  limit: 10,
})
const updated = await db.documents.patch('events', inserted.rid, {
  reviewed: true,
})
await db.documents.delete('events', inserted.rid)
```

`documents.insert()` creates the document collection when needed. Patch is a
top-level merge: unrelated fields survive. An empty patch raises
`INVALID_ARGUMENT`. `documents.delete` returns `{ affected, deleted }` and
NEVER raises on a missing rid (returns `{ affected: 0, deleted: false }`).

### `db.kv(collection?)`

KV helpers preserve exact keys, including namespaced keys with `:`.

```js
await db.query('CREATE KV settings')
const kv = db.kv('settings')

await kv.set('characters:hansel', 'crumbs')  // `put` is a back-compat alias
await kv.get('characters:hansel')        // 'crumbs'   (null when missing)
await kv.exists('characters:hansel')     // { exists: true }
await kv.list({ prefix: 'characters:' }) // { items: [{ key, value }] }
await kv.delete('characters:hansel')     // { affected, deleted }
```

A `kv.get` of a missing key returns `null` (never `NOT_FOUND`). `kv.delete`
returns the `{ affected, deleted }` envelope; deleting a missing key is not an
error and returns `{ affected: 0, deleted: false }`.

### `db.queues` (alias `db.queue`)

Queue helpers cover the embedded FIFO workflow. The spec-canonical namespace
is the plural `db.queues`; `db.queue` remains as an alias.

```js
await db.queues.create('jobs')           // CREATE QUEUE IF NOT EXISTS (idempotent)
await db.queues.push('jobs', { task: 'ship' })
await db.queues.push('jobs', { task: 'email', orderId: 42 }, { dedup: 'outbox:email:42' })
await db.queues.push(
  'jobs',
  { task: 'rebuild_account', accountId: 'acct_123' },
  { key: 'acct_123' },
)
await db.queues.peek('jobs')             // does NOT decrement length
await db.queues.pop('jobs')              // empty queue → [] (never NOT_FOUND)
await db.queues.len('jobs')
await db.queues.purge('jobs')
```

## Graph, vector and time-series

RedDB is one multi-model engine: graphs, vectors and time-series live in the
same database and are reached through `db.query()` (dedicated helpers arrive in
Helper Spec v1.1). Every snippet below is executed on each release by
[`test/readme-examples.test.mjs`](test/readme-examples.test.mjs).

**Graph** — nodes and edges in a `network` collection, then a shortest-path
traversal (the first user row gets `rid` 1024):

```js
await db.query("INSERT INTO network NODE (label, node_type) VALUES ('gateway', 'Host')")
await db.query("INSERT INTO network NODE (label, node_type) VALUES ('app', 'Host')")
await db.query("INSERT INTO network NODE (label, node_type) VALUES ('db', 'Host')")
await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1025, 1.0)")
await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1025, 1026, 1.0)")
await db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1026, 5.0)")

const path = await db.query("GRAPH SHORTEST_PATH '1024' TO '1026' ALGORITHM dijkstra")
path.rows[0].total_weight   // 2 — two 1.0 hops beat the single 5.0 edge
```

**Vector** — store embeddings, then rank by similarity. A vector bind value is
a single `$N` param, so it goes in the params array as one element
(`[[1.0, 0.0]]`), not the variadic form:

```js
await db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway runbook')")
await db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual')")

const hits = await db.query('SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1', [[1.0, 0.0]])
hits.rows[0].score   // 1 — the identical vector scores exactly 1
```

**Time-series** — declare a series with retention + downsampling, ingest
points (timestamps are nanoseconds), then bucket them:

```js
await db.query('CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg')
await db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{\"host\":\"srv-a\"}', 0)")
await db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{\"host\":\"srv-a\"}', 60000000000)")

const rollup = await db.query(
  'SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples ' +
    "FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)",
)
```

## SDK Helper Spec conformance

This driver implements **SDK Helper Spec v1.0**
([`docs/spec/sdk-helpers.md`](../../docs/spec/sdk-helpers.md)). The version is
exposed for cross-driver CI dashboards:

```js
import { HELPER_SPEC_VERSION } from '@reddb-io/sdk'
HELPER_SPEC_VERSION   // '1.0'
db.helperSpecVersion  // '1.0'
```

**Return envelopes** (wire field names preserved when serialised to JSON):

| Envelope            | Fields                                            |
|---------------------|---------------------------------------------------|
| `QueryResult`       | `statement`, `affected`, `columns`, `rows`        |
| `InsertResult`      | `affected` (1), `rid` (`id` legacy alias)         |
| `BulkInsertResult`  | `affected`, `rids` in input order (`ids` alias)   |
| `DeleteResult`      | `affected`, `deleted` (= `affected > 0`)          |
| `ExistsResult`      | `exists`                                          |

**Transaction support:** imperative (`db.tx().begin/commit/rollback`) **and**
callback (`db.transaction(cb)` / `db.tx().run(cb)`). Nested callbacks reject
with `INVALID_ARGUMENT`.

**Case matrix** (spec §12 — ported verbatim in
`test/conformance.test.mjs`):

| Case ID                              | Status      |
|--------------------------------------|-------------|
| `meta.spec_version`                  | supported   |
| `generic.query.no_params`            | supported   |
| `generic.query_with.params`          | supported   |
| `generic.insert.rid`                 | supported   |
| `generic.bulk_insert.rids`           | supported   |
| `generic.delete`                     | supported   |
| `documents.crud_nested_patch`        | supported   |
| `documents.delete_missing_no_error`  | supported   |
| `documents.patch_empty_rejects`      | supported   |
| `kv.exact_key_round_trip`            | supported   |
| `kv.missing_get_returns_none`        | supported   |
| `kv.delete_returns_envelope`         | supported   |
| `queues.fifo_peek_pop_len`           | supported   |
| `queues.empty_pop_returns_empty`     | supported   |
| `queues.purge_resets_len`            | supported   |
| `tx.commit_persists`                 | supported   |
| `tx.rollback_discards`               | supported   |
| `errors.invalid_argument.empty_sql`  | supported   |
| `errors.not_found.document_get`      | supported   |
| `wire.probabilistic.hll_round_trip`  | provisional (SQL via `db.query`) |
| `wire.vectors.sql_round_trip`        | reachable via `db.query` (no v1.0 case) |
| `wire.graph.sql_round_trip`          | reachable via `db.query` (no v1.0 case) |
| `wire.timeseries.sql_round_trip`     | reachable via `db.query` (no v1.0 case) |

**Out-of-scope in v1.0 helpers** (reach via raw `db.query` until v1.1, per
spec): first-class `vectors.*`, `graph.*`, `timeseries.*`, and
`probabilistic.*` helpers (see [Graph, vector and
time-series](#graph-vector-and-time-series)); KV TTL (`kv.expire`) and gRPC
watch; priority queues, consumer groups, dead-letter routing; cross-shard
transactions; JSON Patch / nested / array-positional document patches
(top-level merge only). Transaction **isolation levels** and the retryable
serialization-conflict class are available today through raw `db.query` — see
[Isolation levels](#isolation-levels) and [Serialization conflicts &
retries](#serialization-conflicts--retries).

Run the conformance harness against a locally built binary:

```sh
cargo build                                   # produces target/debug/red
node drivers/js/test/conformance.test.mjs
# or: REDDB_BINARY_PATH=/path/to/red node drivers/js/test/conformance.test.mjs
```

The harness (and the README-examples test) self-skip with exit 0 when no
binary is present, so `pnpm test` stays green on machines without a build.

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
