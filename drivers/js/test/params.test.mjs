import { test } from 'node:test'
import assert from 'node:assert/strict'

import { RedDB, RedDBError } from '../src/index.js'

function fakeDb(handler = () => ({ ok: true, statement: 'SELECT', affected: 0, columns: [], rows: [] })) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return handler(method, params)
    },
    async close() {},
  }
  return { db: new RedDB(client, { transport: 'embedded' }), calls }
}

test('query(sql, ...params) sends params envelope to the underlying client', async () => {
  const { db, calls } = fakeDb()
  await db.query(
    'SELECT * FROM t WHERE id = $1 AND name = $2 AND seen_at = $3',
    42,
    'Ada',
    new Date('2024-01-02T03:04:05.006Z'),
  )

  assert.equal(calls.length, 1)
  assert.equal(calls[0].method, 'query')
  assert.equal(calls[0].params.sql, 'SELECT * FROM t WHERE id = $1 AND name = $2 AND seen_at = $3')
  assert.deepEqual(calls[0].params.params.slice(0, 2), [42, 'Ada'])
  assert.deepEqual(calls[0].params.params[2], { $ts: '1704164645006000000' })
})

test('query(sql, paramsArray) remains supported', async () => {
  const { db, calls } = fakeDb()
  await db.query('SELECT * FROM t WHERE payload = $1', [new Uint8Array([0xde, 0xad])])
  assert.deepEqual(calls[0].params.params, [{ $bytes: '3q0=' }])
})

test('query serializes bigint params as exact integer envelope', async () => {
  const { db, calls } = fakeDb()
  await db.query('SELECT $1', 9007199254740993n)
  assert.deepEqual(calls[0].params.params, [{ $int: '9007199254740993' }])

  const next = fakeDb()
  await next.db.query('SELECT $1', 9223372036854775808n)
  assert.deepEqual(next.calls[0].params.params, [{ $uint: '9223372036854775808' }])
})

test('insert serializes bigint body values as exact integer envelope', async () => {
  const { db, calls } = fakeDb(() => ({ affected: 1, rid: '1' }))
  await db.insert('docs', { id: 9007199254740993n })
  assert.deepEqual(calls[0].params.payload, { id: { $int: '9007199254740993' } })
})

test('query result decodes exact-number envelopes and rejects superseded forms', async () => {
  const { db } = fakeDb(() => ({
    ok: true,
    statement: 'SELECT',
    affected: 0,
    columns: ['n', 'u', 'd'],
    rows: [{ n: { $int: '9007199254740993' }, u: { $uint: '9223372036854775808' }, d: { $decimal: '3.14159265358979323846' } }],
  }))
  const result = await db.query('SELECT exact')
  assert.equal(result.rows[0].n, 9007199254740993n)
  assert.equal(result.rows[0].u, 9223372036854775808n)
  assert.equal(result.rows[0].d, '3.14159265358979323846')

  const bad = fakeDb(() => ({ rows: [{ n: { $number: '9007199254740993' } }] })).db
  await assert.rejects(
    () => bad.query('SELECT old'),
    (err) => err instanceof RedDBError && err.code === 'UNSUPPORTED_EXACT_NUMBER',
  )
})

test('execute aliases parameterized query', async () => {
  const { db, calls } = fakeDb(() => ({ ok: true, statement: 'INSERT', affected: 1, columns: [], rows: [] }))
  const result = await db.execute('INSERT INTO t (id) VALUES ($1)', 7)
  assert.equal(result.affected, 1)
  assert.deepEqual(calls[0], {
    method: 'query',
    params: { sql: 'INSERT INTO t (id) VALUES ($1)', params: [7] },
  })
})

test('unmappable params reject at call site', () => {
  const { db, calls } = fakeDb(() => {
    throw new Error('query should not be dispatched')
  })
  assert.throws(
    () => db.query('SELECT $1', Symbol('bad')),
    (err) => err instanceof RedDBError && err.code === 'UNSUPPORTED_PARAM',
  )
  assert.equal(calls.length, 0)
})
