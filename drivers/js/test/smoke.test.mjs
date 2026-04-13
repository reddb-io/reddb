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

await test('uriToArgs grpc:// forwards --connect to the binary', async () => {
  const args = uriToArgs('grpc://localhost:50051')
  assert(
    JSON.stringify(args) ===
      JSON.stringify(['rpc', '--stdio', '--connect', 'grpc://localhost:50051']),
    `got ${JSON.stringify(args)}`,
  )
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
  const ins2 = await db.insert('users', { name: 'Bob', age: 25 })
  assertEqual(ins2.affected, 1, 'inserted 1')
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
  const q = await db.query('SELECT * FROM items')
  assertEqual(q.rows.length, 3, 'rows count')
  await db.close()
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
