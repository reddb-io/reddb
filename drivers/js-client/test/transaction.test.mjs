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

test('transaction commits on success and returns callback value', async () => {
  const { db, calls } = fakeDb((method) => {
    if (method === 'insert') return { affected: 1, id: 7 }
    return { rows: [] }
  })

  const value = await db.transaction(async (tx) => {
    await tx.insert('users', { name: 'Ada' })
    return { ok: true }
  })

  assert.deepEqual(value, { ok: true })
  assert.deepEqual(calls, [
    { method: 'query', params: { sql: 'BEGIN' } },
    { method: 'insert', params: { collection: 'users', payload: { name: 'Ada' } } },
    { method: 'query', params: { sql: 'COMMIT' } },
  ])
})

test('transaction rolls back when callback throws', async () => {
  const { db, calls } = fakeDb((method) => {
    if (method === 'insert') return { affected: 1, id: 8 }
    return { rows: [] }
  })

  await assert.rejects(
    () => db.transaction(async (tx) => {
      await tx.insert('users', { name: 'Grace' })
      throw new Error('boom')
    }),
    /boom/,
  )

  assert.deepEqual(calls.map((call) => call.params?.sql ?? call.method), [
    'BEGIN',
    'insert',
    'ROLLBACK',
  ])
})

test('transaction rolls back and rethrows query errors', async () => {
  const original = new RedDBError('QUERY_ERROR', 'bad query')
  const { db, calls } = fakeDb((method, params) => {
    if (method === 'query' && params.sql === 'SELECT FAIL') throw original
    return { rows: [] }
  })

  await assert.rejects(
    () => db.transaction((tx) => tx.query('SELECT FAIL')),
    (err) => err === original,
  )

  assert.deepEqual(calls.map((call) => call.params.sql), ['BEGIN', 'SELECT FAIL', 'ROLLBACK'])
})

test('nested transactions fail loudly and outer transaction can commit', async () => {
  const { db, calls } = fakeDb(() => ({ rows: [] }))

  await db.transaction(async () => {
    await assert.rejects(
      () => db.transaction(async () => 'nested'),
      (err) => err instanceof RedDBError && err.code === 'NESTED_TX_NOT_SUPPORTED',
    )
  })

  assert.deepEqual(calls.map((call) => call.params.sql), ['BEGIN', 'COMMIT'])
})
