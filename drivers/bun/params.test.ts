import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { connect } from '../js/src/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const DEFAULT_BINARY = resolve(HERE, '..', '..', 'target', 'debug', 'red')
const BINARY = process.env.REDDB_BINARY_PATH || DEFAULT_BINARY

const HAS_BINARY = existsSync(BINARY)

if (!HAS_BINARY) {
  console.error(`SKIP: binary not found at ${BINARY}`)
  console.error('Run "cargo build" first or set REDDB_BINARY_PATH.')
  process.exit(0)
}

try {
  const db = await connect('memory://', { binary: BINARY })
  try {
    await db.query('CREATE TABLE users (id INTEGER, name TEXT)')
    await db.query('INSERT INTO users (id, name) VALUES ($1, $2)', [1, 'Alice'])
    await db.query('INSERT INTO users (id, name) VALUES ($1, $2)', [2, 'Bob'])

    const selected = await db.query('SELECT * FROM users WHERE id = $1 AND name = $2', [
      1,
      'Alice',
    ])
    assertEqual(selected.rows.length, 1, 'SELECT returns one row')
    assertEqual(selected.rows[0].name, 'Alice', 'SELECT returns the bound text row')

    await db.query('INSERT INTO embeddings VECTOR (dense, content) VALUES ($1, $2)', [
      new Float32Array([1.0, 0.0]),
      'gateway',
    ])
    await db.query('INSERT INTO embeddings VECTOR (dense, content) VALUES ($1, $2)', [
      [0.0, 1.0],
      'database',
    ])

    const nearest = await db.query('SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2', [
      new Float32Array([1.0, 0.0]),
      1,
    ])
    assertEqual(nearest.rows.length, 1, 'SEARCH SIMILAR returns one row')
    assertEqual(nearest.rows[0].score, 1, 'SEARCH SIMILAR uses the bound vector')
  } finally {
    await db.close()
  }

  console.log('ok shared SDK parameterized queries work under Bun')
} catch (err) {
  console.error(`FAIL shared SDK parameterized queries work under Bun`)
  console.error(err?.stack || err?.message || String(err))
  process.exit(1)
}

function assertEqual(actual: unknown, expected: unknown, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`)
  }
}
