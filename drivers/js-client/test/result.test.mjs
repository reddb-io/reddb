import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { normalizeQueryResult } from '../src/core/result.js'
import { HttpRpcClient } from '../src/http.js'

// Replies below are trimmed copies of what a 1.23.4 server returns for
// `POST /query` (and for RedWire QueryWithParams, which carries the same
// envelope). They are the contract the client has to turn into
// `{ statement, affected, columns, rows }`.
const META = { collection: 't', created_at: 1791254412, kind: 'row', rid: 1027, tenant: null, updated_at: 1791254412 }

const SELECT_ENVELOPE = {
  ok: true,
  mode: 'sql',
  engine: 'runtime-table',
  statement: 'table',
  query: 'SELECT id, name FROM t ORDER BY id',
  record_count: 2,
  descriptor: { columns: [{ name: 'id', type: 'number' }, { name: 'name', type: 'string' }] },
  result: {
    columns: ['id', 'name'],
    records: [
      { edges: {}, meta: META, nodes: {}, paths: [], values: { id: 1, name: 'Ada' }, vector_results: [] },
      { edges: {}, meta: { ...META, rid: 1028 }, nodes: {}, paths: [], values: { id: 2, name: 'Bo' }, vector_results: [] },
    ],
    stats: { rows_scanned: 2 },
  },
}

const DML_ENVELOPE = {
  ok: true,
  mode: 'sql',
  engine: 'runtime-dml',
  statement: 'insert',
  statement_type: 'insert',
  affected_rows: 2,
  bookmark: 'rbm1.00000000000000010000000000000002',
  record_count: 0,
  result: { columns: [], records: [], stats: {} },
}

const DDL_ENVELOPE = {
  ok: true,
  engine: 'runtime-ddl',
  statement: 'create',
  statement_type: 'create',
  record_count: 1,
  result: { columns: ['message'], records: [{ meta: META, values: { message: "table 't' created" } }], stats: {} },
}

test('a SELECT envelope becomes columns and plain-object rows', () => {
  const result = normalizeQueryResult(SELECT_ENVELOPE)
  assert.deepEqual(result.columns, ['id', 'name'])
  assert.deepEqual(result.rows, [{ id: 1, name: 'Ada' }, { id: 2, name: 'Bo' }])
  assert.equal(result.affected, 0)
  assert.equal(result.statement, 'table')
  assert.deepEqual(result.stats, { rows_scanned: 2 })
})

test('a DML envelope keeps affected_rows, the statement and the causal bookmark', () => {
  const result = normalizeQueryResult(DML_ENVELOPE)
  assert.equal(result.affected, 2)
  assert.equal(result.statement, 'insert')
  assert.deepEqual(result.rows, [])
  assert.equal(result.bookmark, 'rbm1.00000000000000010000000000000002')
})

test('a DDL envelope surfaces its message row', () => {
  const result = normalizeQueryResult(DDL_ENVELOPE)
  assert.equal(result.statement, 'create')
  assert.deepEqual(result.rows, [{ message: "table 't' created" }])
})

test('an empty SELECT yields no rows rather than undefined', () => {
  const result = normalizeQueryResult({
    ok: true,
    statement: 'table',
    result: { columns: [], records: [], stats: {} },
  })
  assert.deepEqual(result.rows, [])
  assert.deepEqual(result.columns, [])
  assert.equal(result.affected, 0)
})

test('columns fall back to the descriptor when the result carries none', () => {
  const result = normalizeQueryResult({
    ok: true,
    descriptor: { columns: [{ name: 'a' }, { name: 'b' }] },
    result: { records: [{ values: { a: 1, b: 2 } }] },
  })
  assert.deepEqual(result.columns, ['a', 'b'])
  assert.deepEqual(result.rows, [{ a: 1, b: 2 }])
})

test('exact integers survive normalization', () => {
  const big = 9007199254740993n
  const result = normalizeQueryResult({ result: { columns: ['n'], records: [{ values: { n: big } }] }, affected_rows: big })
  assert.equal(result.rows[0].n, big)
  assert.equal(result.affected, big)
})

test('an already canonical result normalizes to itself, so applying twice is safe', () => {
  const canonical = { ok: true, statement: 'SELECT', affected: 0, columns: ['x'], rows: [{ x: 1 }] }
  assert.deepEqual(normalizeQueryResult(canonical), canonical)
  const once = normalizeQueryResult(SELECT_ENVELOPE)
  assert.deepEqual(normalizeQueryResult(once), once)
})

test('the legacy RedWire summary reply stays a valid, rowless result', () => {
  const result = normalizeQueryResult({ ok: true, statement: 'INSERT', affected: 1 })
  assert.deepEqual(result, { ok: true, statement: 'INSERT', affected: 1, columns: [], rows: [] })
})

test('gRPC style records are reduced to their values', () => {
  const result = normalizeQueryResult({ records: [{ values: { id: 7 }, meta: META }, { id: 8 }] })
  assert.deepEqual(result.rows, [{ id: 7 }, { id: 8 }])
})

test('values that are not a query reply are left alone', () => {
  const health = { status: 'ok' }
  assert.equal(normalizeQueryResult(health), health)
  assert.equal(normalizeQueryResult(null), null)
  assert.equal(normalizeQueryResult('x'), 'x')
})

// ---- HTTP transport: query normalization and the insert request body ----

async function withHttpServer(handler, fn) {
  const requests = []
  const server = createServer(async (req, res) => {
    let body = ''
    for await (const chunk of req) body += chunk
    requests.push({ method: req.method, url: req.url, body: body ? JSON.parse(body) : null })
    const reply = handler(req.method, req.url)
    res.writeHead(reply.status ?? 200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(reply.body))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  try {
    return await fn(`http://127.0.0.1:${server.address().port}`, requests)
  } finally {
    server.close()
  }
}

test('HTTP query() returns rows, affected and statement, not the bare result object', async () => {
  await withHttpServer(
    (_method, url) => ({ body: url === '/query' ? SELECT_ENVELOPE : DML_ENVELOPE }),
    async (baseUrl) => {
      const client = new HttpRpcClient({ baseUrl })
      const select = await client.call('query', { sql: 'SELECT id, name FROM t' })
      assert.deepEqual(select.rows, [{ id: 1, name: 'Ada' }, { id: 2, name: 'Bo' }])
      assert.deepEqual(select.columns, ['id', 'name'])
    },
  )
})

test('HTTP query() of a DML statement keeps affected_rows from the envelope top level', async () => {
  await withHttpServer(
    () => ({ body: DML_ENVELOPE }),
    async (baseUrl) => {
      const client = new HttpRpcClient({ baseUrl })
      const result = await client.call('query', { sql: "INSERT INTO t (id) VALUES (1), (2)" })
      assert.equal(result.affected, 2)
      assert.equal(result.statement, 'insert')
    },
  )
})

test('HTTP insert() sends the row under `fields`, which is what the server requires', async () => {
  await withHttpServer(
    () => ({ body: { ok: true, rid: 42, id: 42, entity: null } }),
    async (baseUrl, requests) => {
      const client = new HttpRpcClient({ baseUrl })
      const result = await client.call('insert', { collection: 'users', payload: { name: 'Ada', age: 36 } })
      assert.equal(result.rid, 42)
      assert.equal(requests[0].method, 'POST')
      assert.equal(requests[0].url, '/collections/users/rows')
      assert.deepEqual(requests[0].body, { fields: { name: 'Ada', age: 36 } })
    },
  )
})

test('HTTP query() still rejects an {ok:false} envelope with a typed error', async () => {
  await withHttpServer(
    () => ({ body: { ok: false, error: 'boom', error_code: 'QUERY_ERROR' } }),
    async (baseUrl) => {
      const client = new HttpRpcClient({ baseUrl })
      await assert.rejects(client.call('query', { sql: 'SELECT 1' }), (err) => err.code === 'QUERY_ERROR' && /boom/.test(err.message))
    },
  )
})
