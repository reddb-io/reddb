/**
 * Bun runtime verification for the shared @reddb-io/sdk parameterized query API.
 *
 * This is an engine-backed smoke test. It skips when the local `red` binary is
 * not built, matching the JS SDK smoke-test behavior.
 */

import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

import { connect } from '../../js/src/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const DEFAULT_BINARY = resolve(HERE, '..', '..', '..', 'target', 'debug', 'red')
const BINARY = process.env.REDDB_BINARY_PATH || DEFAULT_BINARY

if (typeof globalThis.Bun === 'undefined') {
  console.error('SKIP: Bun runtime required')
  process.exit(0)
}

if (!existsSync(BINARY)) {
  console.error(`SKIP: binary not found at ${BINARY}`)
  console.error('Run "cargo build --bin red" first or set REDDB_BINARY_PATH.')
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

console.log(`reddb Bun params smoke test (binary: ${BINARY})`)
console.log(`runtime: bun ${globalThis.Bun.version}`)

await test('parameterized SELECT, INSERT, and SEARCH SIMILAR work under Bun', async () => {
  const db = await connect('memory://', { binary: BINARY })
  try {
    await db.query('CREATE TABLE bun_params (id INTEGER, name TEXT, nickname TEXT)')
    const inserted = await db.query(
      'INSERT INTO bun_params (id, name, nickname) VALUES ($1, $2, $3)',
      [7, 'Bun', null],
    )
    assertEqual(inserted.affected, 1, 'insert affected')

    const selected = await db.query(
      'SELECT * FROM bun_params WHERE id = $1 AND name = $2',
      [7, 'Bun'],
    )
    assertEqual(selected.rows.length, 1, 'selected row count')
    assertEqual(selected.rows[0].name, 'Bun', 'selected text param')
    assertEqual(selected.rows[0].nickname, null, 'selected null param')

    await db.query(
      'INSERT INTO embeddings VECTOR (dense, content) VALUES ($1, $2)',
      [new Float32Array([0.25, 0.75]), 'bun vector'],
    )
    const similar = await db.query(
      'SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1',
      [new Float32Array([0.25, 0.75])],
    )
    assertEqual(similar.rows.length, 1, 'similar row count')
    assertEqual(similar.rows[0].content, 'bun vector', 'similar vector result')
    assert(Array.isArray(similar.rows[0].dense), 'similar vector payload')
  } finally {
    await db.close()
  }
})

console.log(`\n${passed} passed, ${failed} failed`)
process.exit(failed > 0 ? 1 : 0)
