import { test } from 'node:test'
import assert from 'node:assert/strict'

import { RedDB, RedDBError } from '../src/index.js'

function fakeDb(handler) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return handler(method, params)
    },
    async close() {},
  }
  return { db: new RedDB(client), calls }
}

test('insert returns required id and preserves remote affected shim', async () => {
  const { db, calls } = fakeDb(() => ({ id: 42, entity: { id: 42 } }))
  assert.deepEqual(await db.insert('users', { name: 'Ada' }), {
    affected: 1,
    id: 42,
    entity: { id: 42 },
  })
  assert.deepEqual(calls[0], {
    method: 'insert',
    params: { collection: 'users', payload: { name: 'Ada' } },
  })
})

test('bulkInsert returns ordered ids', async () => {
  const { db } = fakeDb(() => ({ affected: 2, ids: [101, 102] }))
  const result = await db.bulkInsert('users', [{ name: 'Ada' }, { name: 'Grace' }])
  assert.deepEqual(result, { affected: 2, ids: [101, 102] })
})

test('missing insert ids surface ENGINE_TOO_OLD', async () => {
  const { db } = fakeDb((method) => (
    method === 'insert' ? { affected: 1 } : { affected: 2 }
  ))

  await assert.rejects(
    () => db.insert('users', { name: 'Ada' }),
    (err) => err instanceof RedDBError && err.code === 'ENGINE_TOO_OLD',
  )
  await assert.rejects(
    () => db.bulkInsert('users', [{ name: 'Ada' }, { name: 'Grace' }]),
    (err) => err instanceof RedDBError && err.code === 'ENGINE_TOO_OLD',
  )
})

test('bulkInsert rejects id count mismatches', async () => {
  const { db } = fakeDb(() => ({ affected: 2, ids: [101] }))
  await assert.rejects(
    () => db.bulkInsert('users', [{ name: 'Ada' }, { name: 'Grace' }]),
    (err) => err instanceof RedDBError && err.code === 'INVALID_RESPONSE',
  )
})
