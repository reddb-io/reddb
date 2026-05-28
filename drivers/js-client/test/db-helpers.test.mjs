import { test } from 'node:test'
import assert from 'node:assert/strict'

import { RedDB, RedDBError } from '../src/index.js'

function fakeDb(handler) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return handler?.(method, params) ?? { rows: [] }
    },
    async close() {},
  }
  return { db: new RedDB(client), calls }
}

test('db.exists returns true and false from SHOW COLLECTIONS without throwing', async () => {
  const { db, calls } = fakeDb((_, params) => {
    if (params.sql.includes("'users'")) {
      return { rows: [{ name: 'users', model: 'table' }] }
    }
    return { rows: [] }
  })

  assert.equal(await db.exists('users'), true)
  assert.equal(await db.exists('missing'), false)
  assert.deepEqual(calls.map((call) => call.params.sql), [
    "SHOW COLLECTIONS WHERE name = 'users'",
    "SHOW COLLECTIONS WHERE name = 'missing'",
  ])
})

test('db.list returns collection metadata with capabilities array', async () => {
  const { db } = fakeDb(() => ({
    rows: [
      { name: 'users', model: 'table', entities: 2 },
      { name: 'jobs', model: 'queue', capabilities: ['push'] },
    ],
  }))

  assert.deepEqual(await db.list(), [
    { name: 'users', model: 'table', entities: 2, capabilities: [] },
    { name: 'jobs', model: 'queue', capabilities: ['push'] },
  ])
})

test('db.from builds typed SELECT queries and returns rows', async () => {
  const { db, calls } = fakeDb(() => ({
    rows: [{ id: 2, name: 'Bob' }],
  }))

  const rows = await db
    .from('users')
    .select('id', 'name')
    .where('id = $1', 2)
    .run()

  assert.deepEqual(rows, [{ id: 2, name: 'Bob' }])
  assert.deepEqual(calls[0], {
    method: 'query',
    params: {
      sql: 'SELECT id, name FROM users WHERE id = $1',
      params: [2],
    },
  })
})

test('db.from validates identifiers before query dispatch', async () => {
  const { db, calls } = fakeDb(() => {
    throw new Error('query should not be dispatched')
  })

  await assert.rejects(
    () => db.from('bad-name').select('id').run(),
    (err) => err instanceof RedDBError && err.code === 'INVALID_IDENTIFIER',
  )
  assert.equal(calls.length, 0)
})
