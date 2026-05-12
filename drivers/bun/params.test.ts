import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { connect } from '../js/src/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const DEFAULT_BINARY = resolve(HERE, '..', '..', 'target', 'debug', 'red')
const BINARY = process.env.REDDB_BINARY_PATH || DEFAULT_BINARY

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

function assertEqual<T>(actual: T, expected: T, message: string) {
  if (actual !== expected) {
    throw new Error(
      `${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    )
  }
}

if (!existsSync(BINARY)) {
  console.log(`SKIP: binary not found at ${BINARY}`)
  process.exit(0)
}

const db = await connect('memory://', { binary: BINARY })
try {
  await db.query('CREATE TABLE bun_params (id INTEGER, name TEXT)')
  await db.query('INSERT INTO bun_params (id, name) VALUES ($1, $2)', [1, 'Bun'])
  await db.query('INSERT INTO bun_params (id, name) VALUES ($1, $2)', [2, 'Node'])

  const selected = await db.query(
    'SELECT * FROM bun_params WHERE id = $1 AND name = $2',
    [1, 'Bun'],
  )
  assert(Array.isArray(selected.rows), 'SELECT rows should be an array')
  assertEqual(selected.rows.length, 1, 'SELECT should match one row')
  assertEqual(selected.rows[0].name, 'Bun', 'SELECT should bind text and int params')

  await db.query(
    'INSERT INTO bun_embeddings VECTOR (dense, content) VALUES ($1, $2)',
    [new Float32Array([1.0, 0.0]), 'bun vector'],
  )
  await db.query(
    'INSERT INTO bun_embeddings VECTOR (dense, content) VALUES ($1, $2)',
    [new Float32Array([0.0, 1.0]), 'other vector'],
  )

  const similar = await db.query(
    'SEARCH SIMILAR $1 COLLECTION bun_embeddings LIMIT 1',
    [new Float32Array([1.0, 0.0])],
  )
  assert(Array.isArray(similar.rows), 'SEARCH rows should be an array')
  assertEqual(similar.rows.length, 1, 'SEARCH should match one vector row')
  assertEqual(similar.rows[0].score, 1, 'SEARCH should bind Float32Array vector')
} finally {
  await db.close()
}

console.log('ok shared SDK parameterized queries run under Bun')
