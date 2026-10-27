# RedDB Browser — persistence and query engine

The `@reddb-io/browser` package runs the canonical Rust `.rdb` artifact publication/recovery code, RQL parser and shared
query evaluator in a dedicated WebAssembly worker, persisting through a real
OPFS sync access handle.
Artifact storage requires a secure context, module workers, OPFS and Web Locks.
The storage-independent runtime only needs module workers and WebAssembly.

The target is a fully offline local database with a JavaScript/TypeScript API.
The Rust/WASM engine ships with the package; local operation must not require a
RedDB server, account, CDN or external API. Applications bundle the JS, worker
and WASM assets and make them available locally or through their own offline
application cache. Sync is optional and must never gate local reads or writes.

Version `0.6.0` adds local database sessions for Dart/Flutter Web and retains reusable Worker sessions. Native `.rdb` backup export and restore remain available. It exposes local table SELECT, CREATE TABLE, INSERT, UPDATE, DELETE
and explicit BEGIN/COMMIT/ROLLBACK transactions through `openDatabase`.
Other models, synchronization, conflict
resolution and forwarding between tabs remain pending. The browser package
does not replace the remote `@reddb-io/client`.

## Use from JS/TS

See the [application guide](https://github.com/reddb-io/reddb/blob/main/docs/clients/browser.md) for installation,
Vite/React/Vue/Svelte, plain JavaScript, Worker ownership and offline assets.

```sh
npm add @reddb-io/browser
```

```ts
import { openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const database = await openDatabase({ name: 'my-app', create: true, worker: new RedDBWorker() })
try {
  await database.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
  console.log(await database.query('SELECT id, body FROM notes'))
} finally {
  await database.close()
}
```

Consumers receive built JS/Worker/WASM assets and TypeScript declarations; they
need no Rust toolchain. The commands below are for engine/package contributors.

## Reuse the Worker between file owners

Use a session when an application opens and closes a database frequently. The
session retains the initialized Worker and WASM; each database handle still
owns an exclusive file lock until it closes. Only one database can be open in
one session at a time.

```ts
import { createDatabaseSession } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const session = await createDatabaseSession({ worker: new RedDBWorker() })
try {
  // Repeat this block for later operations using the same session.
  await navigator.locks.request('my-app-operation', async () => {
    const db = await session.openDatabase({ name: 'my-app', create: true })
    try {
      await db.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
      console.log(await db.query('SELECT id, body FROM notes'))
    } finally { await db.close() }
  })
} finally { await session.close() }
```

Use the same outer application lock in every tab to queue operations. The SDK's
file lock still returns `FILE_BUSY` to simultaneous owners; sessions do not add
cross-tab forwarding or shared transactions. Protect stale application snapshots
with the application's existing writer/slot locks as well.

`db.close()` releases the file and discards an uncommitted transaction. Later
opens recover the current committed file, including writes by other owners.
An old handle remains closed even after the session opens another database.
A failed release fences the session and terminates its Worker. Close/discard a
session after a Worker failure and create a new one for retry.

`session.close()` closes its current file and terminates the Worker. It is
idempotent; callers must dispose sessions when finished. Keeping WASM initialized
retains its memory between operations. This amortizes startup, not native file
recovery or SQL execution. Standalone `openDatabase()` retains its existing
behavior: its handle closes both file and Worker.

## Local table SQL from JS/TS

Use `create: true` to create a missing local database. Existing native `.rdb`
artifacts recover their persisted collection contracts, snapshot and committed WAL.

```ts
import { openDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const database = await openDatabase({
  name: 'my-app', create: true, worker: new RedDBWorker(),
})
try {
  await database.query('CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
  await database.query('BEGIN')
  await database.query("INSERT INTO notes VALUES (1, 'Offline'), (2, 'Temporary')")
  await database.query("UPDATE notes SET body = 'Saved offline' WHERE id = 1")
  await database.query('DELETE FROM notes WHERE id = 2')
  await database.query('COMMIT')
  console.log(await database.query('SELECT id, body FROM notes ORDER BY id'))
} finally { await database.close() }
```

The example inserts fixed primary keys; subsequent runs should supply new IDs
or update the existing records. The handle uses the same exclusive Web Lock/OPFS
ownership as `openArtifact`; a second owner receives `FILE_BUSY`. Without
`create: true`, missing files return `DATABASE_NOT_FOUND`. Corrupt existing files
and missing/corrupt catalogs are errors, never recreated or inferred from rows.

Mutations return `{ columns: ['affectedRows'], rows: [[count]] }`. A statement
publishes only after all rows pass the shared schema, defaults, generated-field,
CHECK and uniqueness rules. Inside BEGIN, reads see the handle's staged writes;
COMMIT publishes them together. ROLLBACK, close or worker termination discard
uncommitted changes. Statement errors abort an explicit transaction until
ROLLBACK. Ambiguous storage failures fence the handle until reopen. Savepoints
and explicit isolation options are not exposed by this single-owner API.

Supported: one named table, aliases, `*`, literals, columns and JSON paths,
arithmetic/casts/CASE, nullable predicates, source-expression ORDER BY with native
NULL ordering, and literal LIMIT/OFFSET. Admitted scalar calls are UPPER, LOWER,
LENGTH/CHAR_LENGTH/CHARACTER_LENGTH/OCTET_LENGTH, TRIM/LTRIM/RTRIM, CONCAT,
COALESCE, ABS, JSON_PARSE, JSON_EXTRACT/JSON_EXTRACT_TEXT and CONTAINS. Unknown
fields/functions are rejected even on empty tables. Functions and operators use
the canonical Rust evaluators; exact values retain `reddb-json-v1` tags.

DISTINCT, grouping/aggregates, windows, joins/subqueries,
universal/system sources, other collection models, TTL and retention are rejected
explicitly. Materialization is capped at 100,000 visible source rows, independently
of LIMIT. Superseded and moderated rows use the shared committed-scan visibility;
secret values in `SELECT *` keep the canonical redacted JSON representation.
Writes currently require ordinary table contracts without AI/event policies,
context indexes or encrypted columns. RETURNING, ON CONFLICT, UPDATE LIMIT and
compound assignment syntax are rejected. Transactions admit at most 100,000
pending row actions. DML commits use the native WAL batch; catalog changes use
one complete native snapshot publication. BEGIN copies the committed resident
image, so transaction startup costs memory proportional to the local database.

### Positional parameters

Use `query(sql, params)` to bind values separately from the SQL text:

```ts
const body = "O'Reilly's offline note"
await database.query('INSERT INTO notes (id, body) VALUES ($1, $2)', [1, body])
const found = await database.query('SELECT id, body FROM notes WHERE id = $1', [1])
```

The canonical native Rust binder replaces AST parameter nodes with typed literals.
Values never become SQL syntax. `$1`, `$2`, … and numbered `?1`, `?2`, … use
one-based, contiguous indexes; repeated indexes reuse the same value. Missing,
extra or skipped positions return `INVALID_PARAMS` and abort an active SQL
transaction until ROLLBACK. Invalid JavaScript argument types are rejected before
statement execution.

Parameters accept `null`, boolean, string, finite fractional numbers, safe integer
numbers and `bigint` in the signed i64 or unsigned u64 range. Unsafe integer
numbers must use `bigint`; exact results retain the existing `$int`/`$uint` tags.
Objects, arrays, dates and binary values are not parameter values in this release.
At most 65,536 values are admitted. Bind parameters in supported SELECT/INSERT/
UPDATE/DELETE expressions, including LIMIT (> 0) and OFFSET (>= 0); table/column
names, operators and DDL are not parameterized.

The embedding application owns this local file and its origin. This API does not
establish a server user/tenant session or replay server RLS policies: export only
the authorized subset into an application-owned file. Import a native `.rdb` into
`navigator.storage.getDirectory()/reddb-browser/<name>.rdb` under the same
`reddb-browser:<name>` Web Lock before opening it. Keep native binary bytes intact;
no format translation is needed. Newly created browser databases use the same format.

## Shared runtime and language interop

`createRuntime({ worker? })` opens no file and requires neither OPFS nor Web
Locks. It exposes `info`, `validateQuery`, `executeScalar` and `close`:

```ts
import { createRuntime } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'
const runtime = await createRuntime({ worker: new RedDBWorker() })
try {
  console.log(await runtime.info())
  console.log(await runtime.executeScalar('SELECT 6 * 7 AS answer'))
} finally { await runtime.close() }
```

The versioned Worker/WASM contract reports `abiVersion: 1`,
`valueEncoding: 'reddb-json-v1'` and capabilities. Currently `queryValidation`
and `scalarQueries` are true. Version `0.4.0` also reports `storedQueries: true`
with `storedQueryMode: 'read-write-tables'`, `sqlWrites: true` and
`transactions: true`, `queryParameters: true` and `databaseBackups: true`; `sync` remains false.
Capabilities describe the engine, not the operations exposed by a particular handle. Mismatched SDK/Worker/WASM versions fail with `VERSION_MISMATCH` before
opening storage. Cache the JS, Worker and WASM as one versioned unit.

`createInteropRuntime({ worker? })` returns a synchronous bridge object with
`invoke(method, params?)` and `close()`. `invoke` accepts `info`, `validateQuery`
and `executeScalar` (queries take `{ sql }`) and resolves JSON-safe envelopes:
`{ ok: true, result }` or `{ ok: false, error: { code, message } }`.
A void result is `null`; exact integer/decimal tags are unchanged. `close()` is
idempotent and cancels pending initialization/operations. Calls after close
return `CLOSED`. Each bridge owns a dedicated Worker.

Source adapters use this same bridge for
[Dart/Flutter Web](https://github.com/reddb-io/reddb/blob/main/drivers/dart/BROWSER.md) and
[C#/Blazor](https://github.com/reddb-io/reddb/blob/main/drivers/dotnet/src/Reddb.Browser/README.md). They are not yet
published on pub.dev/NuGet. Their real compiled browser probes exercise scalar
execution, exact values, errors and disposal online and offline. Full offline
browser restarts are verified on Chromium/Firefox; Playwright WPE persistent
profiles lack working CacheStorage, so Safari/iOS qualification remains pending. Run `pnpm --filter @reddb-io/browser build:interop` followed by
`pnpm --filter @reddb-io/browser test:interop` with Dart 3.13.7 and .NET 8.0.426.
These adapters do not expose artifact storage or qualify complete offline apps.

## Build and verify

Install the repository-pinned Rust toolchain, its `wasm32-unknown-unknown` target,
and `wasm-bindgen-cli` **0.2.120**. Install workspace dependencies with pnpm.

```sh
pnpm --filter @reddb-io/browser build
pnpm --filter @reddb-io/browser exec playwright install --with-deps chromium firefox webkit
pnpm --filter @reddb-io/browser test
cargo test --locked -p reddb-io-file
cargo test --locked -p reddb-io-engine
```

The `WASM_BINDGEN` environment variable can point at the pinned CLI executable.
`REDDB_BROWSER_BROWSERS=chromium` restricts the local browser test matrix; CI runs all
three engines. Generated JS/WASM bindings are ignored and rebuilt from Rust.

The tests probe the browser's actual native APIs. If a port lacks synchronous
OPFS (currently Playwright's WPE WebKit runner), its persistence tests explicitly
report skips; the unsupported-browser error path still runs. I/O or format
failures never cause a skip. Chromium and Firefox exercise persistence and
cross-tab ownership. WebKit persistence and real Safari/iOS qualification remain
pending; a green CI run does not imply that coverage.

The conformance tests compile a separate test-only WASM artifact and execute the same
assertions as the native engine tests: scalar goldens/errors, compiled row
filtering/sorting/projection, joins, aggregation, windows, set operations and
cancellation. These do not require OPFS, so they run even on browser ports
without synchronous storage. The same worker also checks the shared 16 KiB page
layout, checksum corruption detection, freelist trunk recovery, pinned-page
eviction and dirty writeback. Native unit tests for these primitives run from
`reddb-engine`; native module paths retain the same types, with pager restart
coverage. The native pager now shares byte I/O, sealed superblock writes and
in-file double-write staging/recovery over `VfsFile` with WASM. Shared fixtures
interrupt each write/flush in page publication, root publication and recovery,
and check short reads, checksums and bounded frames. A test-only OPFS worker
recovers a torn page after a full browser restart with networking disabled
and the HTTP host disconnected.
Those fixtures are excluded from the production SDK. Pager orchestration now
also lives in `reddb-engine`: cached I/O, deferred reclamation, freelist and
allocation fences, physical-tail reservation and checkpoint markers. The native
adapter supplies file locks, encryption and its WAL writer through a synchronous
barrier; portable pagers without a WAL reject nonzero page LSNs. Shared native/WASM
fixtures cover failed allocation fences and WAL barriers. The OPFS restart fixture
uses this complete pager and verifies checkpoint completion through another fresh
pager. Those page-level fixtures are distinct from the public local SQL API
described above; encryption and concurrent multi-connection qualification remain pending.
Native differential tests additionally compare
the scalar corpus to actual `RedDBRuntime` query results.

The shared core also owns the native B-tree/cursor, compressed value layout and
overflow chains. Native APIs alias these implementations through a page-access
contract; the browser executes them in test-only WASM fixtures. Conformance covers
splits, scans, seeks, updates/deletes, large values and reclamation. An additional
OPFS test mutates a native-created tree, restarts the browser with the HTTP host
stopped, and reopens the exported file natively. The fixture includes native
encoded entities as well as opaque B-tree values. These lower-level records do
not supply runtime collection schemas or atomic database transactions. No raw
tree API is added to the production SDK.

## Current query contract

```js
const artifact = await openArtifact({ name: 'example' })
await artifact.executeScalar('SELECT 2 + 3 * 4 AS answer')
// { columns: ['answer'], rows: [[14]] }
await artifact.close()
```

`executeScalar` uses the shared Rust evaluator, built-in type/operator/function
catalogs and output labels. It accepts storage-free scalar SELECT expressions
and literal LIMIT/OFFSET; unsupported clauses and stored-data queries fail
explicitly. It never reads or changes the artifact. Integer values outside JS's
safe range retain canonical `$int`/`$uint` tags instead of rounding. This small
entrypoint is an execution milestone, not the full RQL/storage runtime.

## Current storage contract

`openArtifact({ name })` opens or creates `reddb-browser/<name>.rdb` inside the
origin's private filesystem. It exposes `append`, `readWal`, `checkpoint`,
`readSnapshot`, `generation`, `validateQuery` and `close`. These are artifact
primitives: WAL payloads are native binary records, and checkpoint images are
complete native store snapshots. Passing arbitrary SQL as a payload does not
execute it. The caller must incorporate committed WAL into a checkpoint image.

The browser and native file authority share the same format codecs and write
ordering. Acknowledgement follows both the WAL flush and root publication flush.
After a storage operation fails, that Rust handle rejects further I/O until it
is reopened and recovered. Existing corrupt files are preserved and reported;
they are never silently replaced. A second open returns `FILE_BUSY`; closing or
terminating the owning worker releases the lock. Invalid input and parse errors
do not invalidate the storage handle.

The host must provide exclusive ownership when using Rust's `EmbeddedRdbFile`
directly. The browser wrapper enforces this with Web Locks and OPFS's default
exclusive access handle. The prototype does not use relaxed/read-write-unsafe
access, emulate file locks, or require `SharedArrayBuffer` or isolation headers.

OPFS is subject to origin quota, storage eviction and explicit site-data clearing.
Applications can request persistent storage through `navigator.storage.persist()`;
these primitives do not promise that a browser will grant it or sync after closing.

## Database backups

`database.exportBackup()` returns a `Uint8Array` containing the complete native
`.rdb` file, including its committed catalog, snapshot and WAL. The Worker keeps
exclusive ownership while reading the file and transfers the resulting buffer
to the caller. No SQL is generated and no storage format is reimplemented in JS.
An active transaction stays active; its uncommitted changes are excluded.
The Rust publication fence is checked before copying. A backup read/flush failure
closes the owner; reopen and recover before issuing more operations. A size-limit
rejection leaves the handle usable.

```ts
import { restoreDatabase } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'

const backup = await database.exportBackup()
// Save these bytes outside this origin, for example as a downloaded .rdb file.
const restored = await restoreDatabase({
  name: 'my-app-restored', backup, worker: new RedDBWorker(),
})
try { console.log(await restored.query('SELECT * FROM notes')) }
finally { await restored.close() }
```

Restore requires a new name. An existing file returns `DATABASE_EXISTS`; an
actively owned destination returns `FILE_BUSY`. It writes the new file, flushes
it and opens it through the canonical Rust engine before returning a handle.
A rejected backup removes only its newly created destination before reporting
the failure. Worker/browser termination during restore can leave an incomplete
new destination; it is never silently overwritten on retry.

Backups are bounded at 256 MiB of physical file bytes. The embedding application
owns file selection, download and retention. These are complete physical copies,
which can contain older/deleted bytes, not compacted or redacted exports. They
do not provide additional encryption, synchronization or tenant filtering.
Treat backups as sensitive and export only an authorized database. File codecs,
format versions and native publication/recovery remain unchanged. Export takes
O(file size) reads and one full JS buffer; restore also holds the input and the
recovered resident database in memory. No performance improvement is claimed.

## Offline asset loading

The SDK does not register a service worker or control application updates. A
PWA should include `index.js`, `protocol.js`, `worker.js`, `wasm/reddb_wasm.js` and
`wasm/reddb_wasm_bg.wasm` from `src/` in its own versioned application cache,
along with its application shell. Those assets must remain available together
after installation. A locally packaged host can serve the same assets locally.
HTTPS or a trusted localhost context is required for OPFS and Web Locks.

The browser suite includes a test host's service worker and restarts the entire
browser with networking disabled. It reloads the JS/worker/WASM assets, recovers
OPFS data, writes again and executes a scalar query without reaching the HTTP
host. Installed TS/Vite consumers additionally create tables and run CRUD,
commit and rollback across two offline browser restarts with the HTTP listener stopped.

## Shared transaction state

The canonical native MVCC snapshot manager, visibility predicate, serializable
read/write conflict tracking and per-connection transaction state now live in
`reddb-engine::transaction`. The server uses the same types and algorithms.
Native/WASM fixtures cover snapshot versus read-committed reads, aborted writers,
savepoint sub-XIDs, failed commit preparation, publication-guard lifetime,
retained vacuum horizons, checkpoint exclusions, unique-key reservations and
serializable row/vault predicates with tenant separation. On Chromium/Firefox,
the test host also caches this separate conformance artifact and reruns the suite
after a full offline browser restart with the HTTP listener stopped.

These are volatile engine assertions in a test-only WASM artifact. The public
SDK uses exclusive single-owner transactions as described above; it does not
expose this full native concurrent-MVCC/savepoint surface.

## Shared entities and read visibility

The native `UnifiedEntity` model (rows/documents, graph, vectors, time series,
queues and auxiliary references/embeddings) now lives in
`reddb-engine::storage::entity`. Native paths re-export those same types.
Entity timestamps retain Unix **seconds**; WASM obtains the host wall clock from
`Date.now`, while query timing continues to use the monotonic performance clock.
The browser test worker independently checks entity/embedding/reference timestamps
against JS time, including after the offline restart.

`SnapshotContext`, own/aborted subtransaction visibility, logical-ID SSI read
tracking and table-row version selection are also shared. The native store
supplies candidates through `TableRowVersionStore`; its existing dispatch,
auth/tenant/moderation gates and statement-frame lifetime remain in the host.
Native persisted-row tests cover both dump and paged reopen through the shared
entity/visibility selector. Six new native/WASM cases exercise the representation
and read rules, repeated offline on Chromium/Firefox. They use in-memory fixtures
in WASM, not browser-persisted collections.

Native value tags, checked decoding and Zstd compression now have one owner in
`reddb-file::native_value`; the logical entity adapter lives in
`reddb-engine::storage::entity_codec`. Native dump, paged-record and WAL paths
use that same adapter. A frozen corpus produced by the original native writer
checks all store versions V1–V12 and all value variants, including exact integers,
compressed text/blob values, document bodies, MVCC fields and auxiliary data.
Four native/WASM conformance cases compare the original bytes and reject
truncated values, invalid lengths/tags, inconsistent compressed lengths and
excessive array nesting. Arrays use a checked cursor and an explicit decoding
stack bounded by the existing JSON nesting limit. This is not a general
browser memory-budget qualification.

The OPFS tree fixture now imports eight encoded native entities, decodes and
updates their sequence/timestamps in WASM, restarts the browser offline with the
HTTP host stopped, and exports for native verification. It retains opaque
metadata bytes without interpreting native metadata policy. A separate native
dump test checks the complete entity corpus and actual native metadata. These
remain test-only records without application collection schemas or transactional
publication. The public table SQL API uses these shared codecs with the durable
query/transaction paths described above. Other collection models remain pending;
the canonical file format is unchanged.

## Shared query records and materialization

`reddb-engine::query::unified` now owns the native `UnifiedRecord`/`UnifiedResult`
representation, interned system keys, matched graph records/paths and query
statistics. Native module paths re-export the exact same types. Native stored
graph nodes retain their conversion through `From<&StoredNode>`; their storage
and graph executor remain native.

`reddb-engine::query::records` owns the existing full/lean/projected/universal
entity materializers, by value and by reference, including catalog-schema
fallback, binary-document field promotion, managed timestamps, logical `rid`,
KV/document capabilities, graph endpoints, typed time-series tags and identity
hints. The native runtime calls these same functions. Representation does not
supply authorization, tenancy, visibility, secret handling, catalog discovery or
transaction publication; those policies remain with the caller/runtime.

A frozen pre-extraction native oracle checks 112 results (eight paths over
14 entity shapes), preserving existing differences between materialization
paths. Six relocated native record-layout property tests and four shared
native/WASM cases cover schema overwrite/overflow, projections, document bodies,
typed tags, graph representation and identities. The offline OPFS fixture now
materializes full/projected records from its persisted entities after restart
and verifies their fields before native export. Native integration compares
shared records with actual stored-data query results after dump and paged reopen.

These are internal/test-only extraction fixtures. Public local table SQL now
uses the shared materialization, query and publication paths described above.
Other collection models remain pending in the public SDK.

## Shared resident collection storage

The growing/sealed segment, typed metadata, key/unique-key and physical graph
candidate indexes, and resident payload accounting now live in
`reddb-engine::storage`. Native paths re-export the same implementations. The
native segment manager retains lifecycle, consolidation, explicit snapshot and
moderation gates. Storage clocks use Unix seconds on both native and WASM hosts.
Four shared conformance cases exercise all original record-oracle entity shapes,
key-version lookup and raw mutation, bounded/cancelled scans, metadata and sealing;
the browser worker repeats them after the existing offline restart.

These fixtures exercise canonical resident storage independently of the public
table SQL and durable transaction API described above. Other collection models
remain pending; the artifact/scalar API and all file formats remain compatible.

## Shared snapshot recovery

Native and WASM now use the same typed metadata/entity-record codecs and streaming
V1–V12 dump reader. Recovery preserves empty collection registration, record
metadata, cross-references and the optional V10+ catalog blob. The shared file
layer verifies the checksum before any restore callback and lends the payload
without copying the entire snapshot. Hosts must restore into private state and
publish it only after successful recovery.

Frozen pre-extraction native metadata bytes and entity fixtures check compatible
encoding and decoding. Shared native/WASM cases also cover truncated metadata,
invalid counts/tags/UTF-8, nesting at the RQL depth limit, checksum failures,
legacy optional tails and host callback errors. Metadata nesting above the existing
RQL JSON limit is now rejected; previously the native metadata reader was unbounded.
The native host retains collection/index restoration, ID allocation and WAL policy.
The public local table SQL API now builds on these recovery primitives; the
internal fixtures remain separate from application entrypoints.

## Remaining work toward the approved Browser design

1. Extract the remaining storage-coupled executor/runtime into the shared portable engine and
   preserve native query/transaction semantics for tables, documents, KV, graph
   and vectors. Port clocks, crypto randomness and task scheduling.
2. Add a transactional outbox, change observation, and one-owner request forwarding
   between tabs to the application-facing SDK.
3. Implement versioned, policy-filtered snapshot and resumable change protocols
   in `reddb-wire` and the server. Revalidate normal authorization/RLS on upload;
   preserve transaction grouping, idempotency, stable graph references and deletes.
4. Add explicit conflict resolution, expired-cursor rebootstrap without losing
   pending intent, identity separation, offline assets and the storage-status UI.
5. Validate two-device sync and engine conformance, Safari/iOS on real devices,
   quota/failure cases, bundle size, startup, memory and query performance.
