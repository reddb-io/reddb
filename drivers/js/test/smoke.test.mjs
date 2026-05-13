/**
 * Smoke test for the JS driver against a locally built `red` binary.
 *
 * Run: node test/smoke.test.mjs
 *
 * Picks the binary up from REDDB_BINARY_PATH or ../../target/debug/red
 * (relative to drivers/js), so this works in CI and on dev machines
 * after `cargo build`.
 */

import { existsSync } from 'node:fs'
import { createServer } from 'node:http'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

import { connect, RedDBError, uriToArgs } from '../src/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const DEFAULT_BINARY = resolve(HERE, '..', '..', '..', 'target', 'debug', 'red')

const BINARY = process.env.REDDB_BINARY_PATH || DEFAULT_BINARY

if (!existsSync(BINARY)) {
  console.error(`SKIP: binary not found at ${BINARY}`)
  console.error('Run "cargo build" first or set REDDB_BINARY_PATH.')
  process.exit(0)
}

let passed = 0
let failed = 0

async function test(name, fn) {
  try {
    await fn()
    console.log(`  ok  ${name}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
    failed++
  }
}

function assert(cond, msg) {
  if (!cond) throw new Error(`assertion failed: ${msg}`)
}

function assertEqual(actual, expected, msg) {
  if (actual !== expected) {
    throw new Error(`${msg}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}

console.log(`reddb JS driver smoke test (binary: ${BINARY})`)
console.log(`runtime: ${detectRuntime()}`)

// -----------------------------------------------------------------------
// uriToArgs unit tests (don't need the binary at all)
// -----------------------------------------------------------------------

await test('uriToArgs memory://', async () => {
  const args = uriToArgs('memory://')
  assert(JSON.stringify(args) === JSON.stringify(['rpc', '--stdio']), `got ${args}`)
})

await test('uriToArgs file:///abs', async () => {
  const args = uriToArgs('file:///tmp/foo.rdb')
  assert(
    JSON.stringify(args) === JSON.stringify(['rpc', '--stdio', '--path', '/tmp/foo.rdb']),
    `got ${args}`,
  )
})

await test('uriToArgs grpc:// rejects with EMBEDDED_ONLY', async () => {
  try {
    uriToArgs('grpc://localhost:50051')
    throw new Error('expected throw')
  } catch (err) {
    assert(err instanceof RedDBError, 'expected RedDBError')
    assertEqual(err.code, 'EMBEDDED_ONLY', 'code')
  }
})

await test('uriToArgs unknown scheme throws', async () => {
  try {
    uriToArgs('mongodb://localhost')
    throw new Error('expected throw')
  } catch (err) {
    assert(err instanceof RedDBError, 'expected RedDBError')
  }
})

// -----------------------------------------------------------------------
// Real binary tests
// -----------------------------------------------------------------------

await test('connect memory:// then version()', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const v = await db.version()
  assert(typeof v.version === 'string', 'version string')
  assertEqual(v.protocol, '1.0', 'protocol version')
  await db.close()
})

await test('health() returns ok=true', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const h = await db.health()
  assertEqual(h.ok, true, 'ok')
  await db.close()
})

await test('insert + query round trip', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const ins1 = await db.insert('users', { name: 'Alice', age: 30 })
  assertEqual(ins1.affected, 1, 'inserted 1')
  assert(ins1.id !== undefined && ins1.id !== null, 'insert id is present')
  const ins2 = await db.insert('users', { name: 'Bob', age: 25 })
  assertEqual(ins2.affected, 1, 'inserted 1')
  assert(ins2.id !== undefined && ins2.id !== null, 'second insert id is present')
  const result = await db.query('SELECT * FROM users')
  assert(Array.isArray(result.rows), 'rows is array')
  assert(result.rows.length === 2, `expected 2 rows, got ${result.rows.length}`)
  const names = result.rows.map((r) => r.name).sort()
  assert(JSON.stringify(names) === JSON.stringify(['Alice', 'Bob']), `names: ${names}`)
  await db.close()
})

await test('bulkInsert affects N rows', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const r = await db.bulkInsert('items', [{ name: 'a' }, { name: 'b' }, { name: 'c' }])
  assertEqual(r.affected, 3, 'bulk affected')
  assertEqual(r.ids.length, 3, 'bulk ids count')
  const q = await db.query('SELECT * FROM items')
  assertEqual(q.rows.length, 3, 'rows count')
  await db.close()
})

await test('queue client round trips push pop peek len purge over stdio', async () => {
  const db = await connect('memory://', { binary: BINARY })
  assert(typeof db.queue.push === 'function', 'queue.push exists')
  assert(typeof db.queue.pop === 'function', 'queue.pop exists')
  assert(typeof db.queue.peek === 'function', 'queue.peek exists')
  assert(typeof db.queue.len === 'function', 'queue.len exists')
  assert(typeof db.queue.purge === 'function', 'queue.purge exists')

  await db.query('CREATE QUEUE jobs')
  await db.queue.push('jobs', 'alpha')
  await db.queue.push('jobs', 42)
  await db.queue.push('jobs', { task: 'ship', retries: 2 })

  assertEqual(await db.queue.len('jobs'), 3, 'queue length after pushes')
  const peeked = await db.queue.peek('jobs', 3)
  assertEqual(peeked[0], 'alpha', 'peek string payload')
  assertEqual(peeked[1], 42, 'peek number payload')
  assertEqual(peeked[2].task, 'ship', 'peek JSON task')
  assertEqual(peeked[2].retries, 2, 'peek JSON retries')

  assertEqual((await db.queue.pop('jobs'))[0], 'alpha', 'default pop count is one')
  assertEqual((await db.queue.pop('jobs', 0)).length, 0, 'pop zero returns empty array')
  const remaining = await db.queue.pop('jobs', 2)
  assertEqual(remaining.length, 2, 'pop count returns remaining values')

  await db.queue.push('jobs', 'purge-me')
  await db.queue.purge('jobs')
  assertEqual(await db.queue.len('jobs'), 0, 'purge clears queue')

  await db.query('CREATE QUEUE urgent PRIORITY')
  await db.queue.push('urgent', { task: 'deploy' }, { priority: 10 })
  assertEqual((await db.queue.peek('urgent'))[0].task, 'deploy', 'priority push payload')

  await db.close()
})

await test('db helpers exist list and from round trip over stdio', async () => {
  const db = await connect('memory://', { binary: BINARY })
  await db.query('CREATE TABLE users (id INTEGER, name TEXT)')
  await db.insert('users', { id: 1, name: 'Alice' })
  await db.insert('users', { id: 2, name: 'Bob' })

  assertEqual(await db.exists('users'), true, 'users exists')
  assertEqual(await db.exists('missing_users'), false, 'missing collection does not exist')

  const collections = await db.list()
  const users = collections.find((collection) => collection.name === 'users')
  assert(users, 'users is listed')
  assertEqual(users.model, 'table', 'users model')
  assert(Array.isArray(users.capabilities), 'capabilities array')

  const rows = await db
    .from('users')
    .select('id', 'name')
    .where('id = $1', 2)
    .run()
  assertEqual(rows.length, 1, 'builder row count')
  assertEqual(rows[0].name, 'Bob', 'builder row value')

  await db.close()
})

await test('parameterized query: $N int + text + null bindings', async () => {
  const db = await connect('memory://', { binary: BINARY })
  await db.query('CREATE TABLE u (id INTEGER, name TEXT, nickname TEXT)')
  await db.insert('u', { id: 1, name: 'Alice', nickname: 'al' })
  await db.insert('u', { id: 2, name: 'Bob', nickname: 'bo' })

  // int + text
  const r1 = await db.query('SELECT * FROM u WHERE id = $1 AND name = $2', 1, 'Alice')
  assertEqual(r1.rows.length, 1, 'one row matches')
  assertEqual(r1.rows[0].name, 'Alice', 'name match')

  // null binding
  const r2 = await db.query('SELECT * FROM u WHERE name = $1', [null])
  assertEqual(r2.rows.length, 0, 'no rows for null name')

  // legacy single-arg form still works
  const r3 = await db.query('SELECT * FROM u')
  assertEqual(r3.rows.length, 2, 'legacy two rows')

  const inserted = await db.execute(
    'INSERT INTO u (id, name, nickname) VALUES ($1, $2, $3)',
    3,
    'Cara',
    null,
  )
  assertEqual(inserted.affected, 1, 'execute with params affected')

  await db.close()
})

await test('parameterized query: typed value params round trip', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const seenAt = new Date('2024-01-02T03:04:05.006Z')
  const uuid = '00112233-4455-6677-8899-aabbccddeeff'

  await db.query(
    'CREATE TABLE typed_params (ok BOOLEAN, score FLOAT, payload BLOB, body JSON, seen_at TIMESTAMP, ident UUID)',
  )
  const inserted = await db.query(
    'INSERT INTO typed_params (ok, score, payload, body, seen_at, ident) VALUES ($1, $2, $3, $4, $5, $6)',
    [true, Number.NaN, new Uint8Array([0xde, 0xad, 0xbe, 0xef]), { b: [1, true], a: null }, seenAt, uuid],
  )
  assertEqual(inserted.affected, 1, 'typed insert affected')

  const result = await db.query('SELECT * FROM typed_params')
  assertEqual(result.rows.length, 1, 'one typed row')
  const row = result.rows[0]
  assertEqual(row.ok, true, 'boolean round trip')
  assert(Number.isNaN(row.score), 'NaN round trip')
  assert(row.payload instanceof Uint8Array, 'bytes return Uint8Array')
  assertEqual(Buffer.from(row.payload).toString('hex'), 'deadbeef', 'bytes payload')
  assertEqual(JSON.stringify(row.body), JSON.stringify({ a: null, b: [1, true] }), 'json body')
  assert(row.seen_at instanceof Date, 'timestamp returns Date')
  assertEqual(row.seen_at.toISOString(), seenAt.toISOString(), 'timestamp value')
  assertEqual(row.ident, uuid, 'uuid string')

  await db.close()
})

await test('parameterized query: arity mismatch rejects with INVALID_PARAMS', async () => {
  const db = await connect('memory://', { binary: BINARY })
  await db.query('CREATE TABLE pp (id INTEGER)')
  try {
    await db.query('SELECT * FROM pp WHERE id = $1', [1, 2])
    throw new Error('expected reject')
  } catch (err) {
    assert(err instanceof RedDBError, 'RedDBError')
    assertEqual(err.code, 'INVALID_PARAMS', 'code')
  }
  await db.close()
})

await test('parameterized SEARCH SIMILAR $N with vector param (#355)', async () => {
  const db = await connect('memory://', { binary: BINARY })
  // Seed a tiny vector collection.
  await db.query(
    "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway')",
  )
  await db.query(
    "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database')",
  )

  // number[] form.
  const r1 = await db.query(
    'SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1',
    [[1.0, 0.0]],
  )
  assertEqual(r1.rows.length, 1, 'one row')
  assertEqual(r1.rows[0].score, 1, 'closest vector scores exactly')

  // Float32Array form — driver coerces to plain array on the wire.
  const r2 = await db.query(
    'SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1',
    [new Float32Array([0.0, 1.0])],
  )
  assertEqual(r2.rows.length, 1, 'one row')
  assertEqual(r2.rows[0].score, 1, 'closest vector scores exactly')

  // Type mismatch rejects.
  try {
    await db.query('SEARCH SIMILAR $1 COLLECTION embeddings', [42])
    throw new Error('expected reject')
  } catch (err) {
    assert(err instanceof RedDBError, 'RedDBError')
    assertEqual(err.code, 'INVALID_PARAMS', 'code')
  }

  await db.close()
})

await test('parameterized INSERT VALUES with vector param (#355)', async () => {
  const db = await connect('memory://', { binary: BINARY })
  // INSERT with vector + text params, then verify SEARCH finds it.
  const vec = [0.7, 0.7]
  const inserted = await db.query(
    'INSERT INTO embeddings VECTOR (dense, content) VALUES ($1, $2)',
    [vec, 'parameterized doc'],
  )
  assertEqual(inserted.affected, 1, 'parameterized vector insert affected')
  const r = await db.query(
    'SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1',
    [new Float32Array([0.7, 0.7])],
  )
  assertEqual(r.rows.length, 1, 'one row')
  assertEqual(r.rows[0].score, 1, 'matches inserted vector')
  await db.close()
})

await test('embedded stdio ASK returns the full citation envelope (#406)', async () => {
  const provider = await startAskProvider()
  const previousEnv = {
    REDDB_AI_PROVIDER: process.env.REDDB_AI_PROVIDER,
    REDDB_AI_MODEL: process.env.REDDB_AI_MODEL,
    REDDB_OLLAMA_API_BASE: process.env.REDDB_OLLAMA_API_BASE,
  }
  process.env.REDDB_AI_PROVIDER = 'ollama'
  process.env.REDDB_AI_MODEL = 'mock-ask'
  process.env.REDDB_OLLAMA_API_BASE = provider.baseUrl

  let db
  try {
    db = await connect('memory://', { binary: BINARY })
    await db.query('CREATE TABLE travel (id INT, passport TEXT, notes TEXT)')
    await db.query(
      "INSERT INTO travel (id, passport, notes) VALUES (1, 'PT-002', 'incident FDD-12313 escalated')",
    )

    const result = await db.query("ASK 'passport FDD-12313'")

    assertEqual(result.answer, 'FDD-12313 escalated [^1].', 'answer')
    assertEqual(result.provider, 'ollama', 'provider')
    assertEqual(result.model, 'mock-ask', 'model')
    assertEqual(result.mode, 'lenient', 'mode')
    assertEqual(result.prompt_tokens, 10, 'prompt tokens')
    assertEqual(result.completion_tokens, 4, 'completion tokens')
    assertEqual(result.cache_hit, false, 'cache hit default')
    assertEqual(result.cost_usd, 0, 'cost default')
    assertEqual(result.retry_count, 0, 'retry count')
    assert(Array.isArray(result.sources_flat), 'sources_flat is array')
    assert(Array.isArray(result.citations), 'citations is array')
    assertEqual(result.sources_flat.length, 1, 'one source')
    assertEqual(result.citations.length, 1, 'one citation')
    assertEqual(result.citations[0].marker, 1, 'citation marker')
    assertEqual(result.citations[0].urn, result.sources_flat[0].urn, 'citation urn')
    assertEqual(result.validation.ok, true, 'validation ok')
    assert(Array.isArray(result.validation.warnings), 'validation warnings')
    assert(Array.isArray(result.validation.errors), 'validation errors')
    assert(!('rows' in result), 'ASK result is not row-wrapped')
  } finally {
    if (db) await db.close()
    restoreEnv(previousEnv)
    await provider.close()
  }
})

await test('query error rejects with RedDBError', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    await db.query('NOT A VALID SQL STATEMENT $$$')
    throw new Error('expected query to fail')
  } catch (err) {
    assert(err instanceof RedDBError, 'expected RedDBError')
    assertEqual(err.code, 'QUERY_ERROR', 'error code')
  }
  await db.close()
})

await test('pipelined calls keep order', async () => {
  const db = await connect('memory://', { binary: BINARY })
  const [v1, v2, v3] = await Promise.all([db.version(), db.health(), db.version()])
  assertEqual(v1.protocol, '1.0', 'v1 protocol')
  assertEqual(v2.ok, true, 'v2 ok')
  assertEqual(v3.protocol, '1.0', 'v3 protocol')
  await db.close()
})

await test('close() lets the binary exit', async () => {
  const db = await connect('memory://', { binary: BINARY })
  await db.close()
  // second close is a no-op
  await db.close()
})

await test('calls after close reject', async () => {
  const db = await connect('memory://', { binary: BINARY })
  await db.close()
  try {
    await db.version()
    throw new Error('expected throw')
  } catch (err) {
    assert(err instanceof RedDBError, 'expected RedDBError')
    assertEqual(err.code, 'CLIENT_CLOSED', 'error code')
  }
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed > 0 ? 1 : 0)

function detectRuntime() {
  if (typeof globalThis.Bun !== 'undefined') return `bun ${globalThis.Bun.version}`
  if (typeof globalThis.Deno !== 'undefined') return `deno ${globalThis.Deno.version.deno}`
  return `node ${process.version}`
}

function restoreEnv(previousEnv) {
  for (const [key, value] of Object.entries(previousEnv)) {
    if (value === undefined) {
      delete process.env[key]
    } else {
      process.env[key] = value
    }
  }
}

function startAskProvider() {
  const server = createServer(async (req, res) => {
    await drain(req)
    res.setHeader('content-type', 'application/json')
    if (req.method === 'POST' && req.url === '/v1/embeddings') {
      res.end(
        JSON.stringify({
          model: 'mock-embedding',
          data: [{ index: 0, embedding: [1, 0, 0] }],
          usage: { prompt_tokens: 3, total_tokens: 3 },
        }),
      )
      return
    }
    if (req.method === 'POST' && req.url === '/v1/chat/completions') {
      res.end(
        JSON.stringify({
          model: 'mock-ask',
          choices: [
            {
              message: { role: 'assistant', content: 'FDD-12313 escalated [^1].' },
              finish_reason: 'stop',
            },
          ],
          usage: { prompt_tokens: 10, completion_tokens: 4, total_tokens: 14 },
        }),
      )
      return
    }
    res.statusCode = 404
    res.end(JSON.stringify({ error: { message: `unexpected ${req.method} ${req.url}` } }))
  })
  return new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => {
      server.off('error', reject)
      const address = server.address()
      resolve({
        baseUrl: `http://127.0.0.1:${address.port}/v1`,
        close: () => new Promise((closeResolve) => server.close(closeResolve)),
      })
    })
  })
}

async function drain(req) {
  for await (const _chunk of req) {
    // consume the request body so the client can reuse the connection
  }
}
