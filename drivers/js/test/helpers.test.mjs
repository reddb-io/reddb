/**
 * SDK Helper Spec — helper-surface unit tests (no binary required).
 *
 * These exercise the spec-conformant additions made for issue #559 against a
 * fake RPC client so they run everywhere `node --test` does:
 *   - HELPER_SPEC_VERSION / db.helperSpecVersion (spec §14)
 *   - kv.set alias + kv.delete `{ affected, deleted }` envelope (spec §5)
 *   - queues.create idempotent SQL + `db.queues` alias (spec §6)
 *   - tx.begin / commit / rollback + tx.run commit / rollback / nested reject (spec §7)
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  RedDB,
  RedDBError,
  KvClient,
  QueueClient,
  HELPER_SPEC_VERSION,
} from '../src/index.js'

function fakeDb(handler) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return handler?.(method, params) ?? { rows: [] }
    },
    async close() {},
  }
  return { db: new RedDB(client, { transport: 'embedded' }), calls }
}

function fakeClient(responder) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return responder?.(method, params) ?? { rows: [] }
    },
  }
  return { client, calls }
}

test('HELPER_SPEC_VERSION is 1.0 and exposed on the db', () => {
  assert.equal(HELPER_SPEC_VERSION, '1.0')
  const { db } = fakeDb()
  assert.equal(db.helperSpecVersion, '1.0')
})

test('kv.set is an alias for put and emits the same SQL', async () => {
  const { client, calls } = fakeClient()
  const kv = new KvClient(client, 'settings')
  await kv.set('characters:hansel', 'crumbs')
  assert.equal(calls[0].method, 'query')
  assert.equal(calls[0].params.sql, "KV PUT settings.'characters:hansel' = 'crumbs'")
})

test('kv.delete returns the { affected, deleted } envelope', async () => {
  const { client } = fakeClient((_, params) => (
    params.sql.endsWith('.gone') ? { affected: 0 } : { affected: 1 }
  ))
  const kv = new KvClient(client, 'settings')
  assert.deepEqual(await kv.delete('present'), { affected: 1, deleted: true })
  assert.deepEqual(await kv.delete('gone'), { affected: 0, deleted: false })
})

test('queues.create emits idempotent CREATE QUEUE IF NOT EXISTS', async () => {
  const { client, calls } = fakeClient()
  const queue = new QueueClient(client)
  await queue.create('jobs')
  assert.equal(calls[0].params.sql, 'CREATE QUEUE IF NOT EXISTS jobs')
})

test('queues.create rejects bad identifiers before issuing a query', () => {
  const { client, calls } = fakeClient()
  const queue = new QueueClient(client)
  assert.throws(() => queue.create('bad-name'), (err) => {
    assert.ok(err instanceof RedDBError)
    assert.equal(err.code, 'INVALID_QUEUE_NAME')
    return true
  })
  assert.equal(calls.length, 0)
})

test('db.queues aliases db.queue', () => {
  const { db } = fakeDb()
  assert.ok(db.queues instanceof QueueClient)
  assert.equal(db.queues, db.queue)
})

test('tx.begin / commit issue BEGIN then COMMIT and clear tx state', async () => {
  const { db, calls } = fakeDb()
  const tx = db.tx()
  await tx.begin()
  assert.equal(db.inTransaction, true)
  await tx.commit()
  assert.equal(db.inTransaction, false)
  assert.deepEqual(calls.map((c) => c.params.sql), ['BEGIN', 'COMMIT'])
})

test('tx.rollback issues ROLLBACK and clears tx state', async () => {
  const { db, calls } = fakeDb()
  const tx = db.tx()
  await tx.begin()
  await tx.rollback()
  assert.equal(db.inTransaction, false)
  assert.deepEqual(calls.map((c) => c.params.sql), ['BEGIN', 'ROLLBACK'])
})

test('tx.commit / rollback without an open transaction reject with INVALID_ARGUMENT', async () => {
  const { db } = fakeDb()
  await assert.rejects(
    () => db.tx().commit(),
    (err) => err instanceof RedDBError && err.code === 'INVALID_ARGUMENT',
  )
  await assert.rejects(
    () => db.tx().rollback(),
    (err) => err instanceof RedDBError && err.code === 'INVALID_ARGUMENT',
  )
})

test('tx.run commits on success and returns the callback value', async () => {
  const { db, calls } = fakeDb((method) => (
    method === 'insert' ? { affected: 1, id: 7 } : { rows: [] }
  ))
  const value = await db.tx().run(async (tx) => {
    await tx.insert('users', { name: 'Ada' })
    return { ok: true }
  })
  assert.deepEqual(value, { ok: true })
  assert.deepEqual(calls.map((c) => c.params?.sql ?? c.method), ['BEGIN', 'insert', 'COMMIT'])
  assert.equal(db.inTransaction, false)
})

test('tx.run rolls back and re-throws when the callback throws', async () => {
  const { db, calls } = fakeDb((method) => (
    method === 'insert' ? { affected: 1, id: 8 } : { rows: [] }
  ))
  await assert.rejects(
    () => db.tx().run(async (tx) => {
      await tx.insert('users', { name: 'Grace' })
      throw new Error('boom')
    }),
    /boom/,
  )
  assert.deepEqual(calls.map((c) => c.params?.sql ?? c.method), ['BEGIN', 'insert', 'ROLLBACK'])
  assert.equal(db.inTransaction, false)
})

test('nested tx.run rejects with INVALID_ARGUMENT and the outer tx still commits', async () => {
  const { db, calls } = fakeDb()
  await db.tx().run(async () => {
    await assert.rejects(
      () => db.tx().run(async () => 'nested'),
      (err) => err instanceof RedDBError && err.code === 'INVALID_ARGUMENT',
    )
  })
  assert.deepEqual(calls.map((c) => c.params.sql), ['BEGIN', 'COMMIT'])
})
